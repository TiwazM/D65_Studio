//! Send an image to the SD card through the MEGA65 itself, over the JTAG
//! adapter's serial monitor or over Ethernet, using an installed copy of
//! `m65rs` (mega65-jtag-rs) or `mega65_ftp` (mega65-tools). The MEGA65 writes
//! the file and both tools allocate it contiguously, so the card never has to
//! be unmounted or opened raw from this computer.
use crate::{disk::Result, fat_export};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    M65rs,
    Mega65Ftp,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub kind: Kind,
    pub path: PathBuf,
}
impl Tool {
    pub fn name(&self) -> &'static str {
        match self.kind {
            Kind::M65rs => "m65rs",
            Kind::Mega65Ftp => "mega65_ftp",
        }
    }
    pub fn supports_ethernet(&self) -> bool {
        self.kind == Kind::Mega65Ftp
    }
    fn at(kind: Kind, path: PathBuf) -> Option<Self> {
        path.is_file().then_some(Self { kind, path })
    }
}
/// How the computer reaches the MEGA65: the JTAG adapter's monitor port
/// (`None` lets the tool pick the port) or Ethernet with an optional IPv6
/// address such as `fe80::...%en0`.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Serial(Option<String>),
    Ethernet(Option<String>),
}
/// One external command of a transfer, run in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub label: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
fn on_path(name: &str) -> Option<PathBuf> {
    let names = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    std::env::var_os("PATH")?
        .to_str()?
        .split(if cfg!(windows) { ';' } else { ':' })
        .flat_map(|d| names.iter().map(move |n| Path::new(d).join(n)))
        .find(|p| p.is_file())
}
/// Every usable tool, preferred first: an explicit `D65_M65RS` or
/// `D65_MEGA65_FTP` path, then PATH, then where MEGA65 Studio and M65Connect
/// keep their copies.
pub fn find_tools() -> Vec<Tool> {
    find_tools_with(&[])
}
/// `find_tools` with the user's own paths first (blank or missing ones are
/// ignored, so automatic detection still applies).
pub fn find_tools_with(overrides: &[(Kind, PathBuf)]) -> Vec<Tool> {
    let h = home();
    let mut candidates: Vec<(Kind, PathBuf)> = overrides.to_vec();
    for (var, kind) in [
        ("D65_M65RS", Kind::M65rs),
        ("D65_MEGA65_FTP", Kind::Mega65Ftp),
    ] {
        if let Some(p) = std::env::var_os(var) {
            candidates.push((kind, PathBuf::from(p)));
        }
    }
    for (name, kind) in [("m65rs", Kind::M65rs), ("mega65_ftp", Kind::Mega65Ftp)] {
        if let Some(p) = on_path(name) {
            candidates.push((kind, p));
        }
    }
    let m65rs = [
        PathBuf::from("/Applications/mega65studio/m65rs"),
        h.join("mega65-jtag-rs/target/release/m65rs"),
        h.join("mega65-jtag-rs/target/release/m65rs.exe"),
    ];
    let ftp = [
        h.join("m65connect/Resources/Mac/m65ftp/mega65_ftp.osx"),
        h.join("m65connect/Resources/Windows/m65ftp/mega65_ftp.exe"),
        h.join("m65connect/Resources/Linux/m65ftp/mega65_ftp"),
        h.join("Documents/Mega65Resources/Mega65Connect/Mac/m65ftp/mega65_ftp.osx"),
        h.join("Documents/Mega65Resources/Mega65Connect/Windows/m65ftp/mega65_ftp.exe"),
        PathBuf::from("/Applications/M65Connect.app/Contents/Resources/mega65_ftp.osx"),
    ];
    candidates.extend(m65rs.into_iter().map(|p| (Kind::M65rs, p)));
    candidates.extend(ftp.into_iter().map(|p| (Kind::Mega65Ftp, p)));
    let mut out: Vec<Tool> = Vec::new();
    for (kind, p) in candidates {
        if let Some(t) = Tool::at(kind, p) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}
/// Serial ports that look like a MEGA65 JTAG adapter.
pub fn serial_devices() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/dev") {
        for e in dir.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("cu.usbserial") || n.starts_with("ttyUSB") {
                out.push(format!("/dev/{n}"));
            }
        }
    }
    out.sort();
    out
}

/// The commands of one transfer. `on_fragmented` removes the file again when
/// the chain read back is not one run, so a useless image never stays on the
/// card.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub on_fragmented: Vec<Step>,
}
/// The transfer tool to write with. Both allocate one contiguous run (m65rs
/// since its 2026-10-18 build; older m65rs builds fragment, which the chain
/// read-back catches). Over the JTAG port m65rs is preferred because it also
/// verifies the chain; Ethernet needs mega65_ftp.
pub fn preferred(tools: &[Tool], ethernet: bool) -> Option<&Tool> {
    if ethernet {
        return tools.iter().find(|t| t.supports_ethernet());
    }
    tools
        .iter()
        .find(|t| t.kind == Kind::M65rs)
        .or_else(|| tools.first())
}

/// Upload `image` as `name` in the card's root folder with `writer`, read the
/// cluster chain back with an m65rs from `tools` (direct register mode, so
/// nothing is loaded into the MEGA65), optionally mount it, and leave the
/// MEGA65 at its READY prompt rather than on the transfer helper.
pub fn plan(
    writer: &Tool,
    tools: &[Tool],
    link: &Link,
    image: &Path,
    name: &str,
    mount: bool,
) -> Result<Plan> {
    fat_export::short_name(name)?;
    let image = image.to_string_lossy().into_owned();
    let serial = match link {
        Link::Serial(d) => Some(d.clone()),
        Link::Ethernet(_) => None,
    };
    let m65rs = tools.iter().find(|t| t.kind == Kind::M65rs);
    let rs = |t: &Tool, label: &'static str, args: &[&str]| {
        let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        if let Some(Some(d)) = &serial {
            v.extend(["--device".to_string(), d.clone()]);
        }
        Step {
            label,
            program: t.path.clone(),
            args: v,
        }
    };
    let mut steps = Vec::new();
    let mut on_fragmented = Vec::new();
    match writer.kind {
        Kind::M65rs => {
            if serial.is_none() {
                return Err("m65rs talks to the MEGA65 over the JTAG serial port only. Use mega65_ftp for Ethernet.".into());
            }
            steps.push(rs(writer, "put", &["sd", "put", &image, name, "--force"]));
            steps.push(rs(
                writer,
                "clusters",
                &["sd", "clusters", name, "--direct"],
            ));
            if mount {
                steps.push(rs(writer, "mount", &["sd", "mount", name, "--force"]));
            } else {
                steps.push(rs(writer, "reset", &["reset"]));
            }
            on_fragmented.push(rs(writer, "delete", &["sd", "del", name, "--force"]));
            on_fragmented.push(rs(writer, "reset", &["reset"]));
        }
        Kind::Mega65Ftp => {
            let connection: Vec<String> = match link {
                Link::Serial(Some(d)) => {
                    vec!["-l".into(), d.clone(), "-s".into(), "2000000".into()]
                }
                Link::Serial(None) => {
                    let d = serial_devices()
                        .into_iter()
                        .last()
                        .ok_or("No JTAG serial port found. Plug in the MEGA65's USB JTAG adapter or choose the port.")?;
                    vec!["-l".into(), d, "-s".into(), "2000000".into()]
                }
                Link::Ethernet(Some(a)) => vec!["-i".into(), a.clone()],
                Link::Ethernet(None) => vec!["-e".into()],
            };
            let ftp = |label: &'static str, commands: &[String]| {
                let mut args = connection.clone();
                args.extend(["-y", "-F", "-0", "1"].map(String::from));
                for c in commands {
                    args.extend(["-c".to_string(), c.clone()]);
                }
                // "quit" releases the helper and resets the MEGA65 to READY.
                args.extend(["-c".to_string(), "quit".to_string()]);
                Step {
                    label,
                    program: writer.path.clone(),
                    args,
                }
            };
            let verify = m65rs.filter(|_| serial.is_some());
            let mut commands = vec![format!("put {image} {name}")];
            if mount && verify.is_none() {
                commands.push(format!("mount {name}"));
            }
            steps.push(ftp("put", &commands));
            if let Some(t) = verify {
                steps.push(rs(t, "clusters", &["sd", "clusters", name, "--direct"]));
                if mount {
                    steps.push(ftp("mount", &[format!("mount {name}")]));
                }
                on_fragmented.push(ftp("delete", &[format!("del {name}")]));
            }
        }
    }
    Ok(Plan {
        steps,
        on_fragmented,
    })
}

/// The cluster chain from m65rs' `sd clusters` output.
pub fn parse_chain(text: &str) -> Option<Vec<u32>> {
    let list = text.rsplit("clusters:").next()?.trim();
    let list = list.strip_prefix('[')?.split(']').next()?;
    list.split(',')
        .map(|n| n.trim().parse::<u32>().ok())
        .collect()
}
pub fn contiguous(chain: &[u32]) -> bool {
    !chain.is_empty() && chain.windows(2).all(|w| w[1] == w[0] + 1)
}
fn runs(chain: &[u32]) -> usize {
    1 + chain.windows(2).filter(|w| w[1] != w[0] + 1).count()
}

/// Run the steps, reporting each output line (progress uses carriage returns)
/// as it arrives. Returns the last lines of output on success.
pub fn run(plan: &Plan, mut progress: impl FnMut(&str)) -> Result<String> {
    let mut log = String::new();
    for step in &plan.steps {
        progress(&format!("{}: {}…", step.label, step.program.display()));
        let mut child = Command::new(&step.program)
            .args(&step.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start {}: {e}", step.program.display()))?;
        let mut stderr = child.stderr.take().unwrap();
        let err_thread = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = stderr.read_to_end(&mut v);
            v
        });
        let mut out = child.stdout.take().unwrap();
        let mut buf = [0u8; 4096];
        let mut text = Vec::new();
        let mut line_start = 0;
        loop {
            let n = out.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            text.extend_from_slice(&buf[..n]);
            while let Some(i) = text[line_start..]
                .iter()
                .position(|b| *b == b'\n' || *b == b'\r')
            {
                let line = String::from_utf8_lossy(&text[line_start..line_start + i])
                    .trim()
                    .to_string();
                if !line.is_empty() {
                    progress(&line);
                }
                line_start += i + 1;
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        let errors = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).to_string();
        for line in errors
            .split(['\n', '\r'])
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            progress(line);
        }
        let stdout = String::from_utf8_lossy(&text).to_string();
        log.push_str(&stdout);
        log.push_str(&errors);
        if !status.success() {
            let combined = stdout.clone() + &errors;
            let tail: Vec<&str> = combined
                .split(['\n', '\r'])
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let tail = tail
                .iter()
                .rev()
                .take(4)
                .rev()
                .copied()
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(format!("{} failed ({status}): {tail}", step.label));
        }
        if step.label == "clusters" {
            let chain = parse_chain(&stdout).ok_or(
                "Could not read the cluster chain back; check the file with m65rs sd clusters.",
            )?;
            if !contiguous(&chain) {
                let mut removed = String::new();
                for undo in &plan.on_fragmented {
                    progress(&format!("{}: {}…", undo.label, undo.program.display()));
                    let outcome = Command::new(&undo.program)
                        .args(&undo.args)
                        .stdin(Stdio::null())
                        .output();
                    match outcome {
                        Ok(o) if o.status.success() => {
                            if undo.label == "delete" {
                                removed = " It has been deleted again.".into();
                            }
                        }
                        Ok(o) => {
                            removed = format!(
                                " Deleting it failed: {}",
                                String::from_utf8_lossy(&o.stderr).trim()
                            );
                            break;
                        }
                        Err(e) => {
                            removed = format!(" Deleting it failed: {e}");
                            break;
                        }
                    }
                }
                return Err(format!(
                    "The image was written but landed in {} separate runs ({} clusters), so the MEGA65 cannot mount it.{removed} Free space on the card or write with mega65_ftp, which uses one run.",
                    runs(&chain),
                    chain.len()
                ));
            }
            progress(&format!(
                "Contiguous on the card: {} clusters from {}.",
                chain.len(),
                chain[0]
            ));
        }
    }
    let tail: Vec<&str> = log
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Ok(tail
        .iter()
        .rev()
        .take(12)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tool(kind: Kind) -> Tool {
        Tool {
            kind,
            path: PathBuf::from("/opt/tool"),
        }
    }
    #[test]
    fn plans_m65rs_and_mega65_ftp_commands() {
        let img = Path::new("/tmp/x.d65");
        let rs = tool(Kind::M65rs);
        let ftp = Tool {
            kind: Kind::Mega65Ftp,
            path: PathBuf::from("/opt/mega65_ftp"),
        };
        let p = plan(
            &rs,
            &[rs.clone()],
            &Link::Serial(None),
            img,
            "GAME.D65",
            true,
        )
        .unwrap();
        assert_eq!(
            p.steps.iter().map(|s| s.label).collect::<Vec<_>>(),
            ["put", "clusters", "mount"]
        );
        assert_eq!(
            p.steps[0].args,
            ["sd", "put", "/tmp/x.d65", "GAME.D65", "--force"]
        );
        assert_eq!(p.steps[1].args, ["sd", "clusters", "GAME.D65", "--direct"]);
        assert_eq!(
            p.on_fragmented[0].args,
            ["sd", "del", "GAME.D65", "--force"]
        );
        let p = plan(
            &rs,
            &[rs.clone()],
            &Link::Serial(Some("/dev/cu.usbserial-101".into())),
            img,
            "GAME.D65",
            false,
        )
        .unwrap();
        assert_eq!(
            p.steps.iter().map(|s| s.label).collect::<Vec<_>>(),
            ["put", "clusters", "reset"]
        );
        assert_eq!(
            p.steps[1].args,
            [
                "sd",
                "clusters",
                "GAME.D65",
                "--direct",
                "--device",
                "/dev/cu.usbserial-101"
            ]
        );
        assert!(plan(&rs, &[], &Link::Ethernet(None), img, "GAME.D65", false).is_err());
        assert!(plan(&rs, &[], &Link::Serial(None), img, "game.d65", false).is_err());
        // mega65_ftp alone: one invocation does everything.
        let p = plan(
            &ftp,
            &[ftp.clone()],
            &Link::Ethernet(Some("fe80::1%en0".into())),
            img,
            "GAME.D65",
            true,
        )
        .unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(
            p.steps[0].args,
            [
                "-i",
                "fe80::1%en0",
                "-y",
                "-F",
                "-0",
                "1",
                "-c",
                "put /tmp/x.d65 GAME.D65",
                "-c",
                "mount GAME.D65",
                "-c",
                "quit"
            ]
        );
        assert!(p.on_fragmented.is_empty());
        // mega65_ftp writes, m65rs reads the chain back, mega65_ftp mounts last.
        let p = plan(
            &ftp,
            &[rs.clone(), ftp.clone()],
            &Link::Serial(Some("COM3".into())),
            img,
            "GAME.D65",
            true,
        )
        .unwrap();
        assert_eq!(
            p.steps.iter().map(|s| s.label).collect::<Vec<_>>(),
            ["put", "clusters", "mount"]
        );
        assert_eq!(&p.steps[0].args[..4], ["-l", "COM3", "-s", "2000000"]);
        assert!(!p.steps[0].args.iter().any(|a| a.starts_with("mount")));
        assert_eq!(p.steps[1].program, rs.path);
        let n = p.steps[2].args.len();
        assert_eq!(p.steps[2].args[n - 3], "mount GAME.D65");
        let n = p.on_fragmented[0].args.len();
        assert_eq!(p.on_fragmented[0].args[n - 3], "del GAME.D65");
        assert_eq!(preferred(&[ftp.clone(), rs.clone()], false), Some(&rs));
        assert_eq!(preferred(&[ftp.clone()], false), Some(&ftp));
        assert_eq!(preferred(&[rs.clone(), ftp.clone()], true), Some(&ftp));
        assert_eq!(preferred(&[rs.clone()], true), None);
    }
    #[test]
    fn reads_cluster_chains() {
        assert_eq!(
            parse_chain("1360 clusters: [23928, 23929, 23930]\n"),
            Some(vec![23928, 23929, 23930])
        );
        assert_eq!(
            parse_chain("put 5570560 bytes -> X\n3 clusters: [5, 6, 7]"),
            Some(vec![5, 6, 7])
        );
        assert_eq!(parse_chain("nothing"), None);
        assert!(contiguous(&[5, 6, 7]));
        assert!(!contiguous(&[5, 7, 8]));
        assert!(!contiguous(&[]));
    }
    #[cfg(unix)]
    #[test]
    fn runs_steps_and_reports_progress_and_failures() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'working\\r 50%%\\r100%%\\n'\necho \"$1 done\"\n[ \"$1\" = fail ] && { echo boom >&2; exit 3; }\n[ \"$1\" = clusters ] && echo '3 clusters: [9, 10, 11]'\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let step = |label, arg: &str| Step {
            label,
            program: script.clone(),
            args: vec![arg.into()],
        };
        let plan = |steps: Vec<Step>, undo: Vec<Step>| Plan {
            steps,
            on_fragmented: undo,
        };
        let mut seen = Vec::new();
        let out = run(
            &plan(
                vec![step("put", "put"), step("clusters", "clusters")],
                vec![],
            ),
            |l| seen.push(l.to_string()),
        )
        .unwrap();
        assert!(seen.contains(&"50%".to_string()) && seen.contains(&"put done".to_string()));
        assert!(seen
            .iter()
            .any(|l| l.starts_with("Contiguous on the card: 3 clusters from 9")));
        assert!(out.contains("clusters done"));
        let e = run(&plan(vec![step("put", "fail")], vec![]), |_| {}).unwrap_err();
        assert!(e.starts_with("put failed") && e.contains("boom"), "{e}");
        let marker = dir.path().join("deleted");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = del ] && touch '{}' && exit 0\necho '3 clusters: [9, 11, 12]'\n",
                marker.display()
            ),
        )
        .unwrap();
        let e = run(
            &plan(vec![step("clusters", "x")], vec![step("delete", "del")]),
            |_| {},
        )
        .unwrap_err();
        assert!(
            e.contains("2 separate runs") && e.contains("deleted again"),
            "{e}"
        );
        assert!(marker.exists());
    }
    #[test]
    fn finds_tools_from_environment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m65rs");
        std::fs::write(&p, "").unwrap();
        std::env::set_var("D65_M65RS", &p);
        std::env::set_var("D65_MEGA65_FTP", dir.path().join("missing"));
        let tools = find_tools();
        std::env::remove_var("D65_M65RS");
        std::env::remove_var("D65_MEGA65_FTP");
        assert_eq!(
            tools[0],
            Tool {
                kind: Kind::M65rs,
                path: p
            }
        );
        assert!(tools.iter().all(|t| t.path.is_file()));
        let ftp = dir.path().join("mega65_ftp");
        std::fs::write(&ftp, "").unwrap();
        let tools = find_tools_with(&[
            (Kind::Mega65Ftp, ftp.clone()),
            (Kind::M65rs, dir.path().join("gone")),
        ]);
        assert_eq!(
            tools[0],
            Tool {
                kind: Kind::Mega65Ftp,
                path: ftp
            }
        );
        assert!(!tools.iter().any(|t| t.path.ends_with("gone")));
    }
}
