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
    let h = home();
    let mut candidates = Vec::new();
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

/// The commands that upload `image` as `name` in the card's root folder,
/// check that it landed in one contiguous run (m65rs reports the chain), and
/// optionally ask the Hypervisor to mount it.
pub fn plan(tool: &Tool, link: &Link, image: &Path, name: &str, mount: bool) -> Result<Vec<Step>> {
    fat_export::short_name(name)?;
    let image = image.to_string_lossy().into_owned();
    let mut steps = Vec::new();
    match tool.kind {
        Kind::M65rs => {
            let device = match link {
                Link::Serial(d) => d.clone(),
                Link::Ethernet(_) => {
                    return Err("m65rs talks to the MEGA65 over the JTAG serial port only. Use mega65_ftp for Ethernet.".into())
                }
            };
            let common = |args: &[&str], keep: bool| {
                let mut v: Vec<String> = ["sd"].iter().chain(args).map(|s| s.to_string()).collect();
                if let Some(d) = &device {
                    v.extend(["--device".to_string(), d.clone()]);
                }
                if keep {
                    v.push("--keep".into());
                }
                v
            };
            let mut put = common(&["put", &image, name], true);
            put.push("--force".into());
            steps.push(Step {
                label: "put",
                program: tool.path.clone(),
                args: put,
            });
            steps.push(Step {
                label: "clusters",
                program: tool.path.clone(),
                args: common(&["clusters", name], mount),
            });
            if mount {
                steps.push(Step {
                    label: "mount",
                    program: tool.path.clone(),
                    args: common(&["mount", name], false),
                });
            }
        }
        Kind::Mega65Ftp => {
            let mut args: Vec<String> = match link {
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
            args.extend(["-y", "-F", "-0", "1", "-c"].map(String::from));
            args.push(format!("put {image} {name}"));
            if mount {
                args.extend(["-c".to_string(), format!("mount {name}")]);
            }
            args.extend(["-c".to_string(), "quit".to_string()]);
            steps.push(Step {
                label: "put",
                program: tool.path.clone(),
                args,
            });
        }
    }
    Ok(steps)
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

/// Run the steps, reporting each output line (progress uses carriage returns)
/// as it arrives. Returns the last lines of output on success.
pub fn run(steps: &[Step], mut progress: impl FnMut(&str)) -> Result<String> {
    let mut log = String::new();
    for step in steps {
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
                return Err(format!(
                    "The image was written but is fragmented on the card ({} clusters, not in one run), so the MEGA65 cannot mount it. Delete it, free space on the card and send again.",
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
        let s = plan(
            &tool(Kind::M65rs),
            &Link::Serial(None),
            img,
            "GAME.D65",
            true,
        )
        .unwrap();
        assert_eq!(
            s.iter().map(|s| s.label).collect::<Vec<_>>(),
            ["put", "clusters", "mount"]
        );
        assert_eq!(
            s[0].args,
            ["sd", "put", "/tmp/x.d65", "GAME.D65", "--keep", "--force"]
        );
        assert_eq!(s[1].args, ["sd", "clusters", "GAME.D65", "--keep"]);
        assert_eq!(s[2].args, ["sd", "mount", "GAME.D65"]);
        let s = plan(
            &tool(Kind::M65rs),
            &Link::Serial(Some("/dev/cu.usbserial-101".into())),
            img,
            "GAME.D65",
            false,
        )
        .unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[1].args,
            [
                "sd",
                "clusters",
                "GAME.D65",
                "--device",
                "/dev/cu.usbserial-101"
            ]
        );
        assert!(plan(
            &tool(Kind::M65rs),
            &Link::Ethernet(None),
            img,
            "GAME.D65",
            false
        )
        .is_err());
        assert!(plan(
            &tool(Kind::M65rs),
            &Link::Serial(None),
            img,
            "game.d65",
            false
        )
        .is_err());
        let s = plan(
            &tool(Kind::Mega65Ftp),
            &Link::Ethernet(Some("fe80::1%en0".into())),
            img,
            "GAME.D65",
            true,
        )
        .unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].args,
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
        let s = plan(
            &tool(Kind::Mega65Ftp),
            &Link::Serial(Some("COM3".into())),
            img,
            "GAME.D65",
            false,
        )
        .unwrap();
        assert_eq!(&s[0].args[..4], ["-l", "COM3", "-s", "2000000"]);
        assert!(!s[0].args.iter().any(|a| a.starts_with("mount")));
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
        let mut seen = Vec::new();
        let out = run(&[step("put", "put"), step("clusters", "clusters")], |l| {
            seen.push(l.to_string())
        })
        .unwrap();
        assert!(seen.contains(&"50%".to_string()) && seen.contains(&"put done".to_string()));
        assert!(seen
            .iter()
            .any(|l| l.starts_with("Contiguous on the card: 3 clusters from 9")));
        assert!(out.contains("clusters done"));
        let e = run(&[step("put", "fail")], |_| {}).unwrap_err();
        assert!(e.starts_with("put failed") && e.contains("boom"), "{e}");
        std::fs::write(&script, "#!/bin/sh\necho '3 clusters: [9, 11, 12]'\n").unwrap();
        assert!(run(&[step("clusters", "x")], |_| {})
            .unwrap_err()
            .contains("fragmented"));
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
    }
}
