#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
use eframe::egui::{self, Color32, RichText};
use mega65_disklab::{
    disk::{Disk, Entry, Format, Result},
    store::{atomic_write, Store},
    transfer::{self, Link, Tool},
};
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
type Shared = Arc<Mutex<Store>>;
/// A file dialog shown without blocking the event loop. Blocking dialogs run a
/// nested modal loop that panics in winit when an SD card mounts meanwhile.
type Dialog = Pin<Box<dyn Future<Output = Vec<PathBuf>>>>;
/// What to do with the chosen paths once a dialog closes.
enum Pick {
    Open,
    Import,
    Extract(Entry),
    Convert(Disk, bool),
    Sd(Disk, PathBuf),
    MergeSources,
    MergeSave(Disk, bool, bool),
    Create(Disk),
    Replace(Entry),
    ToolPath(transfer::Kind),
}
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
/// Recently opened images, one path per line, in the user's application data.
fn recent_file() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
    } else {
        home().join(".config")
    };
    let dir = base.join("MEGA65 DiskLab");
    // Settings written while the app was called D65 Studio move along.
    let old = base.join("D65 Studio");
    if !dir.exists() && old.is_dir() {
        let _ = fs::rename(&old, &dir);
    }
    dir.join("recent.txt")
}
/// User settings next to the recent list: `m65rs=PATH` and `mega65_ftp=PATH`.
fn settings_file() -> PathBuf {
    recent_file().with_file_name("settings.txt")
}
fn load_settings() -> (String, String) {
    let text = fs::read_to_string(settings_file()).unwrap_or_default();
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    (get("m65rs"), get("mega65_ftp"))
}
fn tool_overrides((m65rs, ftp): &(String, String)) -> Vec<(transfer::Kind, PathBuf)> {
    [
        (transfer::Kind::M65rs, m65rs),
        (transfer::Kind::Mega65Ftp, ftp),
    ]
    .into_iter()
    .filter(|(_, p)| !p.trim().is_empty())
    .map(|(k, p)| (k, PathBuf::from(p.trim())))
    .collect()
}
fn load_recent() -> Vec<PathBuf> {
    fs::read_to_string(recent_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}
fn one(f: impl Future<Output = Option<rfd::FileHandle>> + 'static) -> Dialog {
    Box::pin(async move {
        f.await
            .map(|h| h.path().to_path_buf())
            .into_iter()
            .collect()
    })
}
fn many(f: impl Future<Output = Option<Vec<rfd::FileHandle>>> + 'static) -> Dialog {
    Box::pin(async move {
        f.await
            .unwrap_or_default()
            .iter()
            .map(|h| h.path().to_path_buf())
            .collect()
    })
}
const TEAL: Color32 = Color32::from_rgb(81, 219, 182);
struct Studio {
    store: Option<Shared>,
    selected: Option<usize>,
    status: String,
    error: bool,
    new_open: bool,
    merge_open: bool,
    launch_build: bool,
    load_compatibility: bool,
    label: String,
    id: String,
    format: Format,
    big: bool,
    /// START bytes last checked against a freshly generated START, and the verdict.
    start_check: Option<(Vec<u8>, bool)>,
    recent: Vec<PathBuf>,
    show_sidebar: bool,
    show_details: bool,
    info_open: bool,
    rename_open: bool,
    send_open: bool,
    send_tools: Vec<Tool>,
    send_tool: Option<Tool>,
    send_ethernet: bool,
    send_device: String,
    send_address: String,
    send_name: String,
    send_mount: bool,
    send_task: Option<std::thread::JoinHandle<Result<String>>>,
    send_progress: Arc<Mutex<Vec<String>>>,
    send_started: Option<std::time::Instant>,
    send_result: Option<String>,
    prefs_open: bool,
    tool_m65rs: String,
    tool_ftp: String,
    dialog: Option<(Pick, Dialog)>,
    rename: String,
    edit: String,
    delete_open: bool,
    editor_open: bool,
    new_text: bool,
    readonly: bool,
    mountpoint: Option<PathBuf>,
    sd_task: Option<std::thread::JoinHandle<Result<mega65_disklab::sd_export::ExportReport>>>,
    sd_result: Option<String>,
    sd_progress: Arc<Mutex<String>>,
    sd_started: Option<std::time::Instant>,
    #[cfg(any(target_os = "macos", windows))]
    mount_thread: Option<std::thread::JoinHandle<Result<()>>>,
}
impl Default for Studio {
    fn default() -> Self {
        Self {
            store: None,
            selected: None,
            status: "Ready. Create a disk or open an existing image.".into(),
            error: false,
            new_open: false,
            merge_open: false,
            launch_build: false,
            load_compatibility: false,
            label: "MY MEGA65 DISK".into(),
            id: "65".into(),
            format: Format::D65,
            big: false,
            start_check: None,
            recent: load_recent(),
            show_sidebar: true,
            show_details: true,
            info_open: false,
            rename_open: false,
            send_open: false,
            send_tools: Vec::new(),
            send_tool: None,
            send_ethernet: false,
            send_device: String::new(),
            send_address: String::new(),
            send_name: String::new(),
            send_mount: true,
            send_task: None,
            send_progress: Arc::new(Mutex::new(Vec::new())),
            send_started: None,
            send_result: None,
            prefs_open: false,
            tool_m65rs: load_settings().0,
            tool_ftp: load_settings().1,
            dialog: None,
            rename: String::new(),
            edit: String::new(),
            delete_open: false,
            editor_open: false,
            new_text: false,
            readonly: false,
            mountpoint: None,
            sd_task: None,
            sd_result: None,
            sd_progress: Arc::new(Mutex::new(String::new())),
            sd_started: None,
            #[cfg(any(target_os = "macos", windows))]
            mount_thread: None,
        }
    }
}
impl Studio {
    fn remember(&mut self, p: &Path) {
        self.recent.retain(|r| r != p);
        self.recent.insert(0, p.to_path_buf());
        self.recent.truncate(8);
        let f = recent_file();
        if let Some(dir) = f.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(
            f,
            self.recent
                .iter()
                .map(|p| format!("{}\n", p.display()))
                .collect::<String>(),
        );
    }
    fn open_dialog(&mut self) {
        self.ask(
            Pick::Open,
            one(rfd::AsyncFileDialog::new()
                .add_filter("MEGA65 images", &["d65", "d81"])
                .pick_file()),
        );
    }
    fn close_disk(&mut self) {
        self.store = None;
        self.selected = None;
        self.start_check = None;
        self.report(Ok(()), "Image closed.");
    }
    fn refresh(&mut self) {
        if let Some(p) = self.store.as_ref().map(|s| s.lock().unwrap().path.clone()) {
            self.store = None;
            self.start_check = None;
            self.load(p);
        }
    }
    /// Re-run every structural check and say what the image is.
    fn validate(&mut self) {
        let Some(disk) = self.store.as_ref().map(|s| s.lock().unwrap().disk.clone()) else {
            return;
        };
        let r = (|| {
            let entries = disk.entries()?;
            for e in &entries {
                disk.chain(e)?;
            }
            if mega65_disklab::launcher::is_editable(&disk) {
                let (used, total) = disk
                    .resident_space()
                    .ok_or("Resident image has overlapping or invalid file chains.")?;
                let start = if mega65_disklab::launcher::start_outdated(&disk)? {
                    "START is older than this version of MEGA65 DiskLab."
                } else {
                    "START is current."
                };
                Ok(format!(
                    "Valid resident image: {} files, {} of {} blocks used. {start}",
                    entries.len(),
                    used,
                    total
                ))
            } else {
                disk.validate_writable()?;
                Ok(format!(
                    "Valid {} image: {} files, {} blocks free, BAM consistent.",
                    disk.format.ext().to_uppercase(),
                    entries.len(),
                    disk.free_blocks()
                ))
            }
        })();
        match r {
            Ok(m) => self.report(Ok(()), &m),
            Err(e) => self.report(Err(e), ""),
        }
    }
    fn save_settings(&self) {
        let f = settings_file();
        if let Some(dir) = f.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(
            f,
            format!(
                "m65rs={}\nmega65_ftp={}\n",
                self.tool_m65rs.trim(),
                self.tool_ftp.trim()
            ),
        );
    }
    fn tools(&self) -> Vec<Tool> {
        transfer::find_tools_with(&tool_overrides(&(
            self.tool_m65rs.clone(),
            self.tool_ftp.clone(),
        )))
    }
    fn open_send(&mut self, ethernet: bool) {
        self.send_tools = self.tools();
        self.send_tool = transfer::preferred(&self.send_tools, ethernet).cloned();
        self.send_ethernet = ethernet;
        if let Some(s) = self.store.as_ref() {
            let s = s.lock().unwrap();
            self.send_name = mega65_disklab::sd_export::suggested_name(&s.path, s.disk.format);
        }
        self.send_open = true;
    }
    fn start_send(&mut self) {
        let Some(tool) = self.send_tool.clone() else {
            return;
        };
        let (bytes, path) = {
            let s = self.store.as_ref().unwrap().lock().unwrap();
            (s.disk.bytes.clone(), s.path.clone())
        };
        if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            self.report(
                Err("Image changed on disk. Reopen it before sending.".into()),
                "",
            );
            return;
        }
        let blank = |s: &String| s.trim().is_empty();
        let link = if self.send_ethernet {
            Link::Ethernet(
                (!blank(&self.send_address)).then(|| self.send_address.trim().to_string()),
            )
        } else {
            Link::Serial((!blank(&self.send_device)).then(|| self.send_device.clone()))
        };
        let name = self.send_name.trim().to_ascii_uppercase();
        let steps = match transfer::plan(
            &tool,
            &self.send_tools,
            &link,
            &path,
            &name,
            self.send_mount,
        ) {
            Ok(s) => s,
            Err(e) => {
                self.report(Err(e), "");
                return;
            }
        };
        self.send_open = false;
        self.send_result = None;
        self.send_started = Some(std::time::Instant::now());
        self.send_progress.lock().unwrap().clear();
        let progress = self.send_progress.clone();
        self.status = format!("Sending {name} to the MEGA65 with {}…", tool.name());
        self.error = false;
        self.send_task = Some(std::thread::spawn(move || {
            transfer::run(&steps, |line| {
                let mut p = progress.lock().unwrap();
                p.push(line.to_string());
                if p.len() > 12 {
                    p.remove(0);
                }
            })
        }));
    }
    fn ask(&mut self, pick: Pick, dialog: Dialog) {
        if self.dialog.is_none() {
            self.dialog = Some((pick, dialog));
        }
    }
    fn picked_paths(&mut self, pick: Pick, paths: Vec<PathBuf>) {
        let Some(first) = paths.first().cloned() else {
            return;
        };
        match pick {
            Pick::Open => self.load(first),
            Pick::Import => self.import_paths(paths),
            Pick::Extract(e) => self.extract_to(e, first),
            Pick::Convert(d, same) => self.convert_to(d, same, first),
            #[cfg(any(target_os = "macos", windows))]
            Pick::Sd(disk, source) => self.export_sd_to(disk, source, first),
            #[cfg(not(any(target_os = "macos", windows)))]
            Pick::Sd(..) => {}
            Pick::MergeSources => self.merge_paths(paths),
            Pick::MergeSave(disk, launch, compat) => self.merge_save(disk, launch, compat, first),
            Pick::ToolPath(kind) => {
                let p = first.display().to_string();
                match kind {
                    transfer::Kind::M65rs => self.tool_m65rs = p,
                    transfer::Kind::Mega65Ftp => self.tool_ftp = p,
                }
                self.save_settings();
            }
            Pick::Replace(e) => {
                let r = fs::read(&first)
                    .map_err(|e| e.to_string())
                    .and_then(|data| {
                        self.store
                            .as_ref()
                            .ok_or("No image")?
                            .lock()
                            .unwrap()
                            .change(|d| d.put(&e.name, e.kind, &data, true))
                    });
                self.report(r, "File replaced.");
            }
            Pick::Create(d) => match self.create_image(&first, d) {
                Ok(s) => {
                    let big = mega65_disklab::launcher::is_editable(&s.disk);
                    self.remember(&s.path);
                    self.store = Some(Arc::new(Mutex::new(s)));
                    self.selected = None;
                    self.readonly = big;
                    self.new_open = false;
                    self.report(
                        Ok(()),
                        if big {
                            "Resident D65 created. Add files here; START and its menu are rebuilt on every change."
                        } else {
                            "New image created."
                        },
                    );
                }
                Err(e) => self.report(Err(e), ""),
            },
        }
    }
    /// The system save dialog has already asked whether to replace an existing
    /// file, so honour that here; the open image itself is never replaced.
    fn create_image(&self, path: &Path, disk: Disk) -> Result<Store> {
        if path.exists() {
            let open = self.store.as_ref().map(|s| s.lock().unwrap().path.clone());
            if path.canonicalize().ok() == open {
                return Err("That image is open in MEGA65 DiskLab. Choose another name.".into());
            }
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        Store::create(path, disk)
    }
    /// Whether a resident image's START is older than this app would generate.
    /// Regenerating is a full repack, so the verdict is cached per START bytes.
    fn start_stale(&mut self, disk: &Disk) -> bool {
        let Some(start) = disk.entries().ok().and_then(|e| e.into_iter().next()) else {
            return false;
        };
        let Ok(bytes) = disk.read(&start) else {
            return false;
        };
        if self.start_check.as_ref().map(|(b, _)| b) != Some(&bytes) {
            let stale = mega65_disklab::launcher::start_outdated(disk).unwrap_or(false);
            self.start_check = Some((bytes, stale));
        }
        self.start_check.as_ref().unwrap().1
    }
    fn report(&mut self, r: Result<()>, ok: &str) {
        match r {
            Ok(()) => {
                self.error = false;
                self.status = ok.into()
            }
            Err(e) => {
                self.error = true;
                self.status = e
            }
        }
    }
    fn load(&mut self, p: PathBuf) {
        match Store::open(&p) {
            Ok(s) => {
                let big = mega65_disklab::launcher::is_editable(&s.disk);
                let warning = s.disk.validate_writable().err().filter(|_| !big);
                self.readonly = warning.is_some() || big;
                self.remember(&s.path);
                self.store = Some(Arc::new(Mutex::new(s)));
                self.selected = None;
                self.status = warning.unwrap_or_else(|| {
                    if big {
                        return "Resident D65 opened. Edits save automatically and rebuild START; the first edit creates a backup.".into();
                    }
                    "Image opened. Edits save automatically; the first edit creates a backup."
                        .into()
                });
                self.error = false
            }
            Err(e) => self.report(Err(e), ""),
        }
    }
    fn picked(&self) -> Option<Entry> {
        let s = self.store.as_ref()?.lock().ok()?;
        s.disk
            .entries()
            .ok()?
            .into_iter()
            .find(|e| Some(e.offset) == self.selected)
    }
    fn import(&mut self) {
        self.ask(
            Pick::Import,
            many(
                rfd::AsyncFileDialog::new()
                    .set_title("Add files to the disk")
                    .pick_files(),
            ),
        );
    }
    fn import_paths(&mut self, paths: Vec<PathBuf>) {
        let r = (|| {
            let mut files = Vec::new();
            for p in paths {
                files.extend(mega65_disklab::disk::import_files(&p)?);
            }
            self.store
                .as_ref()
                .ok_or("No image")?
                .lock()
                .unwrap()
                .change(|d| {
                    for (name, kind, data) in &files {
                        d.put(name, *kind, data, false)
                            .map_err(|e| format!("{name}: {e}"))?
                    }
                    Ok(())
                })
        })();
        self.report(r, "Files added and saved.");
    }
    fn export(&mut self) {
        if let Some(e) = self.picked() {
            let name = e.host_name();
            self.ask(
                Pick::Extract(e),
                one(rfd::AsyncFileDialog::new().set_file_name(name).save_file()),
            );
        }
    }
    fn extract_to(&mut self, e: Entry, p: PathBuf) {
        let r = {
            let s = self.store.as_ref().unwrap().lock().unwrap();
            if p.canonicalize().ok().as_ref() == Some(&s.path) {
                Err("Choose a different path; exporting over the open image is not allowed.".into())
            } else {
                s.disk.read(&e).and_then(|b| atomic_write(&p, &b))
            }
        };
        self.report(r, "File exported.")
    }
    fn convert(&mut self, format: Format) {
        let disk = self.store.as_ref().unwrap().lock().unwrap().disk.clone();
        match disk.convert(format) {
            Err(e) => self.report(Err(e), ""),
            Ok(d) => {
                let name = format!("{}.{}", disk.label(), format.ext());
                self.ask(
                    Pick::Convert(d, disk.format == format),
                    one(rfd::AsyncFileDialog::new().set_file_name(name).save_file()),
                );
            }
        }
    }
    fn convert_to(&mut self, d: Disk, same: bool, p: PathBuf) {
        let r = if p.canonicalize().ok().as_ref()
            == self
                .store
                .as_ref()
                .map(|s| s.lock().unwrap().path.clone())
                .as_ref()
        {
            Err("Choose a different path for the converted image.".into())
        } else {
            atomic_write(&p, &d.bytes)
        };
        self.report(
            r,
            if same {
                "Image copy saved."
            } else {
                "Converted image exported."
            },
        )
    }
    #[cfg(any(target_os = "macos", windows))]
    fn export_sd(&mut self) {
        let (disk, source) = {
            let s = self.store.as_ref().unwrap().lock().unwrap();
            (s.disk.clone(), s.path.clone())
        };
        let name = mega65_disklab::sd_export::suggested_name(&source, disk.format);
        self.ask(
            Pick::Sd(disk, source),
            one(rfd::AsyncFileDialog::new()
                .set_title(if cfg!(windows) {
                    "SD export — choose an existing card folder and a NEW filename"
                } else {
                    "Direct SD export — choose the card root and a NEW filename"
                })
                .set_file_name(name)
                .save_file()),
        );
    }
    #[cfg(any(target_os = "macos", windows))]
    fn export_sd_to(&mut self, disk: Disk, source: PathBuf, destination: PathBuf) {
        if fs::read(&source).ok().as_deref() != Some(disk.bytes.as_slice()) {
            self.report(
                Err(
                    "Source image changed or became unavailable. Reopen it before exporting."
                        .into(),
                ),
                "",
            );
            return;
        }
        self.sd_result = None;
        self.sd_started = Some(std::time::Instant::now());
        *self.sd_progress.lock().unwrap() = "Checking the card".into();
        #[cfg(windows)]
        let progress = self.sd_progress.clone();
        self.status = "Checking the card and requesting administrator disk access; writing and verifying consecutive clusters…".into();
        self.sd_task = Some(std::thread::spawn(move || {
            #[cfg(windows)]
            {
                mega65_disklab::direct_sd::export_with_progress(&disk, &destination, |message| {
                    *progress.lock().unwrap() = message;
                })
            }
            #[cfg(target_os = "macos")]
            mega65_disklab::direct_sd::export(&disk, &destination)
        }));
    }
    fn merge(&mut self) {
        self.ask(
            Pick::MergeSources,
            many(
                rfd::AsyncFileDialog::new()
                    .set_title(if self.launch_build {
                        "Select D81 images — files are packed automatically"
                    } else {
                        "Select two or three D81 images"
                    })
                    .add_filter("D81 images", &["d81"])
                    .pick_files(),
            ),
        );
    }
    fn merge_paths(&mut self, paths: Vec<PathBuf>) {
        let result = (|| {
            let sources: Result<Vec<_>> = paths
                .iter()
                .map(|p| Disk::parse(fs::read(p).map_err(|e| e.to_string())?))
                .collect();
            let sources = sources?;
            let disk = if self.launch_build {
                mega65_disklab::launcher::build_with_compat(&sources, self.load_compatibility)?
            } else {
                Disk::merge_d81(&sources)?
            };
            Ok(disk)
        })();
        match result {
            Ok(disk) => {
                let name = if self.launch_build {
                    "LAUNCH.d65"
                } else {
                    "MERGED-EXPERIMENTAL.d65"
                };
                self.ask(
                    Pick::MergeSave(disk, self.launch_build, self.load_compatibility),
                    one(rfd::AsyncFileDialog::new().set_file_name(name).save_file()),
                );
            }
            Err(e) => self.report(Err(e), ""),
        }
    }
    fn merge_save(&mut self, disk: Disk, launch: bool, compat: bool, path: PathBuf) {
        let result = Store::create(&path, disk).map(|store| {
            self.remember(&store.path);
            self.store = Some(Arc::new(Mutex::new(store)));
            self.selected = None;
            self.readonly = true;
            self.merge_open = false;
        });
        self.report(result, if launch && compat { "LOAD-compatible launch disk created for ROM 920413. Mount in drive 8 and RUN \"START\". Experimental; hardware test pending." } else if launch { "Resident launch disk created. Mount in drive 8 and RUN \"START\". Editable here; START is rebuilt on every change." } else { "Experimental merge created and verified offline. ROM 920413 cannot load later banks; a modified ROM or custom loader is required. Read-only." });
    }
    fn start_mount(&mut self) {
        #[cfg(windows)]
        {
            let result = (|| {
                mega65_disklab::mount::available()?;
                let p = mega65_disklab::mount::free_drive()?;
                let h = mega65_disklab::mount::start(
                    self.store.as_ref().unwrap().clone(),
                    &p,
                    self.readonly,
                )?;
                self.status = format!(
                    "Mapping {} in Explorer. Eject the volume before editing here.",
                    p.display()
                );
                self.mountpoint = Some(p);
                self.mount_thread = Some(h);
                Ok(())
            })();
            if let Err(e) = result {
                self.report(Err(e), "");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Err(e) = mega65_disklab::mount::available() {
                self.report(Err(e), "");
                return;
            }
            let label = self.store.as_ref().unwrap().lock().unwrap().disk.label();
            match mega65_disklab::mount::new_mountpoint(&label) {
                Ok(p) => self.mount_at(p),
                Err(e) => self.report(Err(e), ""),
            }
        }
        #[cfg(not(any(target_os="macos", windows)))] self.report(Err("file manager mounting is available on macOS. Windows drive mounting is not implemented yet.".into()),"");
    }
    #[allow(unused_variables)]
    fn mount_at(&mut self, p: PathBuf) {
        #[cfg(target_os = "macos")]
        match mega65_disklab::mount::start(self.store.as_ref().unwrap().clone(), &p, self.readonly)
        {
            Ok(h) => {
                self.mount_thread = Some(h);
                self.mountpoint = Some(p);
                self.report(Ok(()),"Mounting. The MEGA65 DiskLab volume opens in Finder; eject it before editing here.")
            }
            Err(e) => {
                mega65_disklab::mount::remove_mountpoint(&p);
                self.report(Err(e), "")
            }
        }
    }
}
impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((_, dialog)) = self.dialog.as_mut() {
            if let Poll::Ready(paths) = dialog
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                let (pick, _) = self.dialog.take().unwrap();
                self.picked_paths(pick, paths);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if self
            .send_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let r = self
                .send_task
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err("Transfer worker stopped unexpectedly.".into()));
            match r {
                Ok(tail) => {
                    self.report(Ok(()), "Image sent to the MEGA65's SD card.");
                    self.send_result =
                        Some(format!("Image sent to the MEGA65's SD card.\n\n{tail}"));
                }
                Err(e) => {
                    self.report(Err(e.clone()), "");
                    self.send_result = Some(e);
                }
            }
        }
        if self.send_task.is_some() {
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Sending to MEGA65");
                ui.spinner();
                if let Some(start) = self.send_started {
                    ui.label(format!("Elapsed: {} seconds", start.elapsed().as_secs()));
                }
                ui.label("The MEGA65 is writing the image to its SD card. Keep it switched on and connected.");
                ui.add_space(8.0);
                for line in self.send_progress.lock().unwrap().iter() {
                    ui.monospace(line);
                }
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if let Some(message) = self.send_result.clone() {
            egui::Window::new("Send to MEGA65 result")
                .collapsible(false)
                .default_width(560.0)
                .show(ctx, |ui| {
                    ui.monospace(message);
                    if ui.button("OK").clicked() {
                        self.send_result = None;
                    }
                });
        }
        if self.sd_task.as_ref().is_some_and(|task| task.is_finished()) {
            match self
                .sd_task
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err("SD export worker stopped unexpectedly.".into()))
            {
                Ok(report) => {
                    self.error =
                        !matches!(report.layout, mega65_disklab::sd_export::Layout::Verified);
                    self.status = report.message();
                }
                Err(error) => self.report(Err(error), ""),
            }
            self.sd_result = Some(self.status.clone());
        }
        if self.sd_task.is_some() {
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Exporting to SD card");
                ui.spinner();
                ui.label(self.sd_progress.lock().unwrap().as_str());
                if let Some(start) = self.sd_started { ui.label(format!("Elapsed: {} seconds", start.elapsed().as_secs())); }
                ui.label("Checking the FAT32 card, allocating consecutive clusters, and verifying the saved copy.");
                ui.label("Approve the administrator prompt when it appears. Keep the SD card connected until the result appears.");
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if let Some(message) = self.sd_result.clone() {
            egui::Window::new("SD-card export result")
                .collapsible(false)
                .default_width(520.0)
                .show(ctx, |ui| {
                    ui.label(message);
                    if ui.button("OK").clicked() {
                        self.sd_result = None;
                    }
                });
        }
        #[cfg(any(target_os = "macos", windows))]
        if self.mount_thread.as_ref().is_some_and(|h| h.is_finished()) {
            let r = self
                .mount_thread
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err("Mount worker stopped unexpectedly.".into()));
            #[cfg(target_os = "macos")]
            if let Some(p) = &self.mountpoint {
                mega65_disklab::mount::remove_mountpoint(p);
            }
            self.mountpoint = None;
            self.report(r, "file manager volume ejected. Image saved.");
        }
        let mounted = self.mountpoint.is_some();
        if mounted {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if ctx.input(|i| i.viewport().close_requested()) && mounted {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.report(
                Err("Eject the file manager volume before closing MEGA65 DiskLab.".into()),
                "",
            );
        }
        let snapshot = self.store.as_ref().map(|s| {
            let s = s.lock().unwrap();
            (s.disk.clone(), s.path.clone(), s.backup.clone())
        });
        let (has_disk, big, writable_ok) = match &snapshot {
            Some((disk, _, _)) => (
                true,
                mega65_disklab::launcher::is_editable(disk),
                disk.validate_writable().is_ok(),
            ),
            None => (false, false, false),
        };
        let can_edit = (writable_ok || big) && !mounted;
        let picked = self.picked();
        let has_pick = picked.is_some();
        let modal = self.new_open
            || self.merge_open
            || self.editor_open
            || self.delete_open
            || self.info_open
            || self.rename_open
            || self.send_open
            || self.dialog.is_some();
        let cmd = egui::Modifiers::COMMAND;
        let key = |k| ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(cmd, k)));
        let shift_key = |k| {
            ctx.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(
                    cmd | egui::Modifiers::SHIFT,
                    k,
                ))
            })
        };
        if !modal && !mounted && key(egui::Key::N) {
            self.format = Format::D65;
            self.new_open = true;
        }
        if !modal && !mounted && key(egui::Key::O) {
            self.open_dialog();
        }
        if !modal && !mounted && has_disk && key(egui::Key::W) {
            self.close_disk();
        }
        if !modal && has_disk && key(egui::Key::I) {
            self.info_open = true;
        }
        if !modal && !mounted && has_disk && key(egui::Key::R) {
            self.refresh();
        }
        if !modal && can_edit && shift_key(egui::Key::A) {
            self.import();
        }
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(19, 28, 35))
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.add_enabled(!mounted, egui::Button::new("New D65…").shortcut_text("⌘N")).clicked() { self.format = Format::D65; self.new_open = true; ui.close_menu(); }
                        if ui.add_enabled(!mounted, egui::Button::new("New D81…")).clicked() { self.format = Format::D81; self.big = false; self.new_open = true; ui.close_menu(); }
                        if ui.add_enabled(!mounted, egui::Button::new("Open Disk Image…").shortcut_text("⌘O")).clicked() { self.open_dialog(); ui.close_menu(); }
                        ui.add_enabled_ui(!mounted && !self.recent.is_empty(), |ui| {
                            ui.menu_button("Open Recent", |ui| {
                                for p in self.recent.clone() {
                                    if ui.button(p.file_name().unwrap_or_default().to_string_lossy().to_string()).on_hover_text(p.display().to_string()).clicked() { self.load(p); ui.close_menu(); }
                                }
                                ui.separator();
                                if ui.button("Clear Menu").clicked() { self.recent.clear(); let _ = fs::remove_file(recent_file()); ui.close_menu(); }
                            });
                        });
                        if ui.add_enabled(!mounted, egui::Button::new("Merge D81s…")).clicked() { self.merge_open = true; ui.close_menu(); }
                        ui.separator();
                        ui.add_enabled_ui(has_disk && !mounted, |ui| {
                            if ui.button("Save a Copy As…").on_hover_text("Edits are saved automatically; this writes a copy in the same format.").clicked() { let f = snapshot.as_ref().unwrap().0.format; self.convert(f); ui.close_menu(); }
                            ui.menu_button("Export Disk Image", |ui| {
                                if ui.button("As D65…").clicked() { self.convert(Format::D65); ui.close_menu(); }
                                if ui.button("As D81…").clicked() { self.convert(Format::D81); ui.close_menu(); }
                            });
                            #[cfg(any(target_os = "macos", windows))]
                            if ui.button("Export to SD Card…").on_hover_text("Writes the image straight to a card in this computer. Unmounts the card and asks for administrator access.").clicked() { self.export_sd(); ui.close_menu(); }
                            ui.menu_button("Send to MEGA65", |ui| {
                                if ui.button("Over USB JTAG…").clicked() { self.open_send(false); ui.close_menu(); }
                                if ui.button("Over Ethernet…").clicked() { self.open_send(true); ui.close_menu(); }
                            });
                        });
                        ui.separator();
                        if ui.button("Preferences…").clicked() { self.prefs_open = true; ui.close_menu(); }
                        if ui.add_enabled(has_disk && !mounted, egui::Button::new("Close Disk").shortcut_text("⌘W")).clicked() { self.close_disk(); ui.close_menu(); }
                    });
                    ui.menu_button("Edit", |ui| {
                        if ui.add_enabled(can_edit, egui::Button::new("Add Files…").shortcut_text("⇧⌘A")).clicked() { self.import(); ui.close_menu(); }
                        if ui.add_enabled(can_edit, egui::Button::new("Import from Disk Image…")).on_hover_text("Copies every PRG, SEQ and USR file of a D81 or D65 into this image.").clicked() { self.ask(Pick::Import, many(rfd::AsyncFileDialog::new().set_title("Choose D81 or D65 images to copy files from").add_filter("Disk images", &["d81", "d65"]).pick_files())); ui.close_menu(); }
                        if ui.add_enabled(can_edit, egui::Button::new("New Text File…")).clicked() { self.edit.clear(); self.rename = "README".into(); self.new_text = true; self.editor_open = true; ui.close_menu(); }
                        ui.separator();
                        ui.add_enabled_ui(can_edit && has_pick, |ui| {
                            if ui.button("Rename…").clicked() { self.rename = picked.as_ref().map(|e| e.name.clone()).unwrap_or_default(); self.rename_open = true; ui.close_menu(); }
                            if ui.button("Replace File…").on_hover_text("Keeps the name and type, takes the bytes from a file on this computer.").clicked() { let e = picked.clone().unwrap(); self.ask(Pick::Replace(e), one(rfd::AsyncFileDialog::new().set_title("Choose the new contents").pick_file())); ui.close_menu(); }
                            if ui.button("Edit as Text…").clicked() {
                                let e = picked.clone().unwrap();
                                match snapshot.as_ref().unwrap().0.read(&e).map(|b| String::from_utf8(b)) {
                                    Ok(Ok(text)) => { self.edit = text; self.new_text = false; self.editor_open = true; }
                                    _ => self.report(Err("That file is not UTF-8/ASCII text.".into()), ""),
                                }
                                ui.close_menu();
                            }
                            if ui.button("Delete Selected File…").clicked() { self.delete_open = true; ui.close_menu(); }
                        });
                        if ui.add_enabled(has_pick, egui::Button::new("Extract Selected File…")).clicked() { self.export(); ui.close_menu(); }
                        ui.separator();
                        if ui.add_enabled(can_edit && big, egui::Button::new("Update START")).on_hover_text("Replaces START, its reader and catalogue with this version's. Files are unchanged.").clicked() {
                            let r = self.store.as_ref().unwrap().lock().unwrap().change(|d| mega65_disklab::launcher::update_start(d));
                            self.report(r, "START replaced with the current reader and menu. Files are unchanged.");
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Disk", |ui| {
                        if ui.add_enabled(has_disk, egui::Button::new("Disk Information…").shortcut_text("⌘I")).clicked() { self.info_open = true; ui.close_menu(); }
                        if ui.add_enabled(has_disk, egui::Button::new("Validate Image")).clicked() { self.validate(); ui.close_menu(); }
                        ui.separator();
                        if ui.add_enabled(has_disk && !mounted && !modal, egui::Button::new(if cfg!(windows) { "Mount in Explorer" } else { "Mount in Finder" })).clicked() { self.start_mount(); ui.close_menu(); }
                        if ui.add_enabled(mounted, egui::Button::new("Eject")).clicked() {
                            #[cfg(any(target_os = "macos", windows))]
                            { let r = mega65_disklab::mount::unmount(self.mountpoint.as_ref().unwrap()); self.report(r, "Eject requested."); }
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("View", |ui| {
                        ui.checkbox(&mut self.show_sidebar, "Disk Information Sidebar");
                        ui.checkbox(&mut self.show_details, "File Details");
                        ui.separator();
                        if ui.add_enabled(has_disk && !mounted, egui::Button::new("Refresh").shortcut_text("⌘R")).on_hover_text("Reads the image from disk again.").clicked() { self.refresh(); ui.close_menu(); }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("▣").size(32.0).color(TEAL));
                    ui.vertical(|ui| {
                        ui.label(RichText::new("MEGA65 DISKLAB").size(25.0).strong());
                        ui.label(
                            RichText::new("A disk workbench for your MEGA65").color(Color32::GRAY),
                        );
                    });
                });
            });
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().inner_margin(14.0))
            .show(ctx, |ui| {
                ui.label(RichText::new(&self.status).color(if self.error {
                    Color32::from_rgb(255, 158, 139)
                } else {
                    TEAL
                }));
            });
        if let Some((disk, path, backup)) = snapshot {
            let writable = disk.validate_writable();
            let start_stale = big && self.start_stale(&disk);
            if self.show_sidebar {
                egui::SidePanel::left("disk").exact_width(248.0).frame(egui::Frame::new().fill(Color32::from_rgb(19,28,35)).inner_margin(20.0)).show(ctx,|ui|{ egui::ScrollArea::vertical().id_salt("sidebar_scroll").show(ui,|ui|{
                ui.add_space(8.0);ui.label(RichText::new(disk.format.ext().to_uppercase()).size(44.0).color(TEAL).strong());ui.heading(disk.label());ui.label(format!("DISK ID  {}",disk.disk_id()));ui.add_space(24.0);
                if writable.is_ok() {
                    ui.label(RichText::new("AVAILABLE DATA SPACE").small().color(Color32::GRAY));ui.label(RichText::new(format!("{} blocks",disk.free_blocks())).size(23.0));ui.add(egui::ProgressBar::new(1.0-disk.free_blocks()as f32/3160.0).fill(TEAL));ui.label(format!("{:.1} KiB free",disk.free_blocks()as f32*254.0/1024.0));
                    ui.label("Filesystem: 80 tracks × 40 sectors");
                } else if let Some((used,total))=disk.resident_space() {
                    ui.label(RichText::new("COMPILATION SPACE").color(TEAL));
                    ui.add(egui::ProgressBar::new(used as f32/total as f32).fill(TEAL));
                    ui.label(format!("{:.2} MB used of {:.2} MB",used as f32*254.0/1_000_000.0,total as f32*254.0/1_000_000.0));
                    ui.label(format!("{:.0} KB available when rebuilding",(total-used) as f32*254.0/1000.0));
                    ui.label("Includes START. Add, rename or delete files here; START is rebuilt each time.");
                    ui.add_space(6.0);
                    ui.label(RichText::new(if start_stale { "START is older than this version of MEGA65 DiskLab." } else { "START is current for this version of MEGA65 DiskLab." }).small().color(if start_stale { Color32::from_rgb(215,184,117) } else { Color32::GRAY }));
                    if start_stale && ui.add_enabled(can_edit, egui::Button::new("Update START")).clicked() {
                        let r = self.store.as_ref().unwrap().lock().unwrap().change(|d| mega65_disklab::launcher::update_start(d));
                        self.report(r, "START replaced with the current reader and menu. Files are unchanged.");
                    }
                } else { ui.label(RichText::new("READ-ONLY INSPECTION").color(TEAL));ui.label("Free space is not available for this layout."); }
                ui.add_space(14.0);ui.label(format!("Container: {} bytes",disk.bytes.len()));
                ui.separator();ui.label(RichText::new(if cfg!(windows) { "EXPLORER DRIVE" } else { "FINDER VOLUME" }).small().color(Color32::GRAY));
                if mounted{
                    ui.label("Mounted — editing in file manager");
                    if ui.button("Eject volume").clicked(){#[cfg(any(target_os="macos", windows))] {let r=mega65_disklab::mount::unmount(self.mountpoint.as_ref().unwrap());self.report(r,"Eject requested.");}}
                    if ui.button(if cfg!(windows) { "Show in Explorer" } else { "Show in Finder" }).clicked(){#[cfg(any(target_os="macos", windows))] {let _=std::process::Command::new(if cfg!(windows) { "explorer.exe" } else { "open" }).arg(if cfg!(windows) { PathBuf::from(format!("{}\\", self.mountpoint.as_ref().unwrap().display())) } else { self.mountpoint.as_ref().unwrap().clone() }).spawn();}}
                }else{
                    if writable.is_err() { self.readonly=true; } ui.add_enabled(writable.is_ok(),egui::Checkbox::new(&mut self.readonly,"Mount read-only"));
                    ui.label(RichText::new(if cfg!(windows) { "Disk › Mount in Explorer · built-in WebClient" } else { "Disk › Mount in Finder · built-in macOS connection" }).small().color(TEAL));
                }
                ui.add_space(10.0);ui.label(RichText::new("Any file can be added. .prg, .seq and .usr set the type and drop the extension; other files keep their full name (up to 16 characters) as PRG, .txt as SEQ. A .d81 or .d65 adds all of its files. The disk has a flat directory.").small());
                ui.add_space(12.0);ui.label(RichText::new(path.display().to_string()).small().color(Color32::GRAY));if backup.is_some(){ui.label(RichText::new("Original backed up beside image").small().color(TEAL));}
            }); });
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(24.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Disk contents");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(can_edit, egui::Button::new("Add files…"))
                                .clicked()
                            {
                                self.import()
                            }
                        });
                    });
                    ui.add_space(8.0);
                    if let (Err(e), false) = (writable, big) {
                        ui.label(RichText::new(format!("Read-only: {e}")).color(Color32::YELLOW));
                    }
                    let entries = disk.entries().unwrap_or_default();
                    ui.label(
                        RichText::new(format!("{} files", entries.len())).color(Color32::GRAY),
                    );
                    ui.add_space(12.0);
                    egui::ScrollArea::vertical()
                        .id_salt("files")
                        .max_height(300.0)
                        .show(ui, |ui| {
                            egui::Grid::new("directory")
                                .num_columns(4)
                                .striped(true)
                                .spacing([24.0, 10.0])
                                .min_col_width(55.0)
                                .show(ui, |ui| {
                                    ui.strong("NAME");
                                    ui.strong("TYPE");
                                    ui.strong("BLOCKS");
                                    ui.strong("BYTES");
                                    ui.end_row();
                                    for e in &entries {
                                        if ui
                                            .selectable_label(
                                                self.selected == Some(e.offset),
                                                RichText::new(&e.name).monospace(),
                                            )
                                            .clicked()
                                        {
                                            self.selected = Some(e.offset);
                                            self.rename = e.name.clone();
                                        }
                                        ui.label(e.type_name());
                                        ui.label(e.blocks.to_string());
                                        ui.label(
                                            disk.read(e)
                                                .map(|b| b.len().to_string())
                                                .unwrap_or_else(|_| "—".into()),
                                        );
                                        ui.end_row();
                                    }
                                });
                            if entries.is_empty() {
                                ui.add_space(35.0);
                                ui.label(RichText::new("Your disk is ready.").size(22.0));
                                ui.label("Add a PRG, SEQ or USR file to get started.");
                                ui.add_space(35.0);
                            }
                        });
                    ui.add_space(18.0);
                    ui.separator();
                    if let Some(e) = entries.iter().find(|e| Some(e.offset) == self.selected) {
                        ui.horizontal(|ui| {
                            ui.heading(&e.name);
                            if ui.button("Extract…").clicked() {
                                self.export()
                            }
                            if ui
                                .add_enabled(can_edit, egui::Button::new("Delete…"))
                                .clicked()
                            {
                                self.delete_open = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_enabled(
                                can_edit,
                                egui::TextEdit::singleline(&mut self.rename)
                                    .desired_width(220.0)
                                    .char_limit(16),
                            );
                            if ui
                                .add_enabled(can_edit, egui::Button::new("Rename"))
                                .clicked()
                            {
                                let r = self
                                    .store
                                    .as_ref()
                                    .unwrap()
                                    .lock()
                                    .unwrap()
                                    .change(|d| d.rename(e, &self.rename));
                                self.report(r, "File renamed.");
                            }
                        });
                        match disk.read(e) {
                            Ok(bytes) => {
                                if e.kind != 2 {
                                    if let Ok(text) = std::str::from_utf8(&bytes) {
                                        if ui
                                            .add_enabled(
                                                can_edit,
                                                egui::Button::new("Edit UTF-8 / ASCII text…"),
                                            )
                                            .clicked()
                                        {
                                            self.edit = text.into();
                                            self.new_text = false;
                                            self.editor_open = true;
                                        }
                                    }
                                }
                                if self.show_details {
                                    ui.add_space(10.0);
                                    ui.label(
                                    RichText::new(
                                        "BYTE PREVIEW · first 512 bytes · no PETSCII translation",
                                    )
                                    .small()
                                    .color(Color32::GRAY),
                                );
                                    let preview = bytes
                                        .iter()
                                        .take(512)
                                        .copied()
                                        .collect::<Vec<_>>()
                                        .chunks(16)
                                        .enumerate()
                                        .map(|(i, c)| {
                                            format!(
                                                "{:04X}   {:48}  {}",
                                                i * 16,
                                                c.iter()
                                                    .map(|b| format!("{b:02X} "))
                                                    .collect::<String>(),
                                                c.iter()
                                                    .map(|b| if (32..127).contains(b) {
                                                        *b as char
                                                    } else {
                                                        '.'
                                                    })
                                                    .collect::<String>()
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    egui::ScrollArea::both()
                                        .id_salt("preview")
                                        .max_height(210.0)
                                        .show(ui, |ui| {
                                            ui.monospace(preview);
                                        });
                                }
                            }
                            Err(err) => {
                                ui.label(err);
                            }
                        }
                    } else {
                        ui.add_space(12.0);
                        ui.label("Select a file to inspect, rename, extract or edit it.");
                    }
                });
        } else {
            egui::CentralPanel::default().show(ctx,|ui|{ui.add_space(75.0);ui.vertical_centered(|ui|{ui.label(RichText::new("Your next disk starts here.").size(35.0).strong());ui.add_space(15.0);ui.label("Create a MEGA65 D65 image, browse its files, and take it into file manager.");ui.label("D81 images work too. Everything stays on this computer.");ui.add_space(28.0);if ui.button(RichText::new("Create a disk").size(20.0)).clicked(){self.new_open=true;}ui.add_space(6.0);ui.label(RichText::new("File › Open Disk Image… (⌘O) · File › Open Recent").color(Color32::GRAY));ui.add_space(38.0);ui.label(RichText::new("D65 support is based on the MEGA65 Freezer layout.\nSTART can load extra-sector programs; ordinary editing uses D81 capacity.").color(Color32::from_rgb(215,184,117)));});});
        }
        if self.merge_open {
            egui::Window::new("Experimental D81 merge").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label("Build a launch disk from multiple D81s, or merge two or three whole images.");
                ui.label("Every file is checked byte-for-byte. Source images stay unchanged.");
                ui.label("PRG, SEQ and USR files only. Duplicate names must be renamed first.");
                ui.colored_label(Color32::YELLOW, "Ordinary ROM loading cannot read later banks. The optional START reader bypasses this limit. Do not SAVE, SCRATCH or VALIDATE.");
                ui.checkbox(&mut self.launch_build, "Include START launcher (automatically imports multiple D81s)");
                ui.label("Resident launcher: about 5.4 MB minus START. Menu programs up to 55,041 bytes with a BASIC entry at $2001. Other files are retained. Drive 8; top 64 KB of Attic RAM reserved.");
                ui.add_enabled_ui(self.launch_build, |ui| {
                    ui.checkbox(&mut self.load_compatibility, "Experimental LOAD compatibility (ROM 920413)");
                });
                ui.label("Compatibility mode redirects literal-name LOAD/VERIFY from drive 8; no OPEN channels, custom disk loaders or writes. Reserves 8 KB more Attic RAM.");
                ui.label("Without compatibility mode, extra file reads use the documented resident API. Ordinary LOAD/OPEN calls are not redirected. The result can be edited here; START is rebuilt on every change.");
                ui.horizontal(|ui| {
                    if ui.button("Choose D81 images…").clicked() { self.merge(); }
                    if ui.button("Cancel").clicked() { self.merge_open=false; }
                });
            });
        }
        if self.new_open {
            egui::Window::new("Create disk image")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Disk label");
                    ui.add(egui::TextEdit::singleline(&mut self.label).char_limit(16));
                    ui.label("Two-character ID");
                    ui.add(egui::TextEdit::singleline(&mut self.id).char_limit(2));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.format, Format::D65, "D65 · MEGA65");
                        ui.radio_value(&mut self.format, Format::D81, "D81 · 1581");
                    });
                    ui.add_enabled_ui(self.format == Format::D65, |ui| {
                        ui.checkbox(&mut self.big, "Large D65 with START resident reader");
                    });
                    let big = self.big && self.format == Format::D65;
                    ui.label(if big {
                        "About 5.4 MB shared by START and your files. RUN \"START\" on the MEGA65 for a menu of BASIC programs; the resident reader loads the rest. Files are packed again on every change."
                    } else {
                        "Both use the supported 800 KiB filesystem geometry."
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Create…").clicked() {
                            let disk = if big {
                                mega65_disklab::launcher::create_editable(&self.label, &self.id)
                            } else {
                                Disk::create(self.format, &self.label, &self.id)
                            };
                            match disk {
                                Err(e) => self.report(Err(e), ""),
                                Ok(d) => {
                                    let name = format!("{}.{}", self.label, self.format.ext());
                                    self.ask(
                                        Pick::Create(d),
                                        one(rfd::AsyncFileDialog::new().set_file_name(name).save_file()),
                                    );
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.new_open = false;
                        }
                    });
                });
        }
        if self.info_open {
            if let Some(s) = self.store.as_ref().map(|s| s.lock().unwrap()) {
                let d = &s.disk;
                let entries = d.entries().unwrap_or_default();
                let resident = mega65_disklab::launcher::is_editable(d);
                let mut lines = vec![
                    format!("Label: {}    ID: {}", d.label(), d.disk_id()),
                    format!(
                        "Format: {}    Container: {} bytes",
                        d.format.ext().to_uppercase(),
                        d.bytes.len()
                    ),
                    format!("Files: {}", entries.len()),
                    format!("Path: {}", s.path.display()),
                ];
                if resident {
                    if let Some((used, total)) = d.resident_space() {
                        lines.push(format!(
                            "Resident reader image: {} of {} blocks used ({:.2} MB free)",
                            used,
                            total,
                            (total - used) as f32 * 254.0 / 1_000_000.0
                        ));
                    }
                    lines.push(format!(
                        "Compatibility LOAD hook: {}",
                        if d.bytes[d.offset(40, 0).unwrap() + 60] == 1 {
                            "on"
                        } else {
                            "off"
                        }
                    ));
                } else {
                    lines.push(format!(
                        "Free: {} blocks ({:.1} KiB)",
                        d.free_blocks(),
                        d.free_blocks() as f32 * 254.0 / 1024.0
                    ));
                    if let Err(e) = d.validate_writable() {
                        lines.push(format!("Read-only: {e}"));
                    }
                }
                if let Some(b) = &s.backup {
                    lines.push(format!("Backup of the original: {}", b.display()));
                }
                if let Some(m) = &self.mountpoint {
                    lines.push(format!("Mounted at: {}", m.display()));
                }
                drop(s);
                egui::Window::new("Disk information")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        for l in &lines {
                            ui.label(l);
                        }
                        ui.add_space(6.0);
                        if ui.button("Close").clicked() {
                            self.info_open = false;
                        }
                    });
            } else {
                self.info_open = false;
            }
        }
        if self.rename_open {
            egui::Window::new("Rename file")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("New name (up to 16 characters)");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.rename).char_limit(16));
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked()
                            || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            if let Some(e) = self.picked() {
                                let r = self
                                    .store
                                    .as_ref()
                                    .unwrap()
                                    .lock()
                                    .unwrap()
                                    .change(|d| d.rename(&e, &self.rename));
                                self.report(r, "File renamed.");
                            }
                            self.rename_open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.rename_open = false;
                        }
                    });
                });
        }
        if self.prefs_open {
            egui::Window::new("Preferences").collapsible(false).resizable(false).default_width(560.0).show(ctx, |ui| {
                ui.label("Transfer tools for Send to MEGA65. Leave a field blank to find the tool automatically (PATH, MEGA65 Studio, M65Connect, or the D65_M65RS / D65_MEGA65_FTP variables).");
                ui.add_space(6.0);
                for (label, kind) in [("m65rs (mega65-jtag-rs)", transfer::Kind::M65rs), ("mega65_ftp (mega65-tools / M65Connect)", transfer::Kind::Mega65Ftp)] {
                    ui.label(label);
                    ui.horizontal(|ui| {
                        let field = match kind { transfer::Kind::M65rs => &mut self.tool_m65rs, transfer::Kind::Mega65Ftp => &mut self.tool_ftp };
                        let changed = ui.add(egui::TextEdit::singleline(field).desired_width(430.0).hint_text("automatic")).changed();
                        if ui.button("Browse…").clicked() {
                            self.ask(Pick::ToolPath(kind), one(rfd::AsyncFileDialog::new().set_title("Choose the program").pick_file()));
                        }
                        if changed { self.save_settings(); }
                    });
                }
                ui.add_space(6.0);
                let found = self.tools();
                if found.is_empty() {
                    ui.colored_label(Color32::YELLOW, "No transfer tool found right now.");
                } else {
                    ui.label(RichText::new("Found now:").small().color(Color32::GRAY));
                    for t in &found {
                        ui.label(RichText::new(format!("{} — {}", t.name(), t.path.display())).small());
                    }
                }
                ui.label(RichText::new(format!("Saved in {}", settings_file().display())).small().color(Color32::GRAY));
                ui.add_space(6.0);
                if ui.button("Close").clicked() { self.prefs_open = false; }
            });
        }
        if self.send_open {
            egui::Window::new("Send to MEGA65").collapsible(false).resizable(false).default_width(520.0).show(ctx, |ui| {
                ui.label("The MEGA65 writes the image to its own SD card in one contiguous run, so nothing is written to a card from this computer and no administrator prompt appears.");
                ui.label("The MEGA65 must be switched on, connected, and at the READY prompt. The transfer helper replaces whatever program is in its memory.");
                ui.add_space(6.0);
                if self.send_tools.is_empty() {
                    ui.colored_label(Color32::YELLOW, "No transfer tool found. Install MEGA65 Studio (m65rs) or M65Connect (mega65_ftp), or set its path under File › Preferences…, then reopen this window.");
                } else {
                    let selected = self.send_tool.as_ref().map(|t| format!("{} — {}", t.name(), t.path.display())).unwrap_or_else(|| "Choose a tool".into());
                    egui::ComboBox::from_label("Writer").width(420.0).selected_text(selected).show_ui(ui, |ui| {
                        for t in self.send_tools.clone() {
                            let label = format!("{} — {}", t.name(), t.path.display());
                            ui.selectable_value(&mut self.send_tool, Some(t), label);
                        }
                    });
                }
                ui.label(RichText::new("mega65_ftp writes the image in one run; m65rs takes free clusters one at a time and can fragment it (a fragmented result is deleted again). When both are installed the chain is read back with m65rs.").small().color(Color32::GRAY));
                let ethernet_ok = self.send_tool.as_ref().is_some_and(|t| t.supports_ethernet());
                if !ethernet_ok { self.send_ethernet = false; }
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.send_ethernet, false, "USB JTAG adapter (serial)");
                    ui.add_enabled_ui(ethernet_ok, |ui| { ui.radio_value(&mut self.send_ethernet, true, "Ethernet (mega65_ftp)"); });
                });
                if self.send_ethernet {
                    ui.label("MEGA65 IPv6 address, e.g. fe80::...%en0. Leave blank to auto-discover.");
                    ui.add(egui::TextEdit::singleline(&mut self.send_address).desired_width(420.0));
                } else {
                    let devices = transfer::serial_devices();
                    let shown = if self.send_device.is_empty() { "Automatic".to_string() } else { self.send_device.clone() };
                    egui::ComboBox::from_label("Serial port").width(420.0).selected_text(shown).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.send_device, String::new(), "Automatic");
                        for d in devices { ui.selectable_value(&mut self.send_device, d.clone(), d); }
                    });
                }
                ui.label("Filename on the card (8 characters, .D65 or .D81)");
                ui.add(egui::TextEdit::singleline(&mut self.send_name).char_limit(12).desired_width(200.0));
                ui.checkbox(&mut self.send_mount, "Ask the MEGA65 to mount it as drive 8 afterwards");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.send_tool.is_some(), egui::Button::new("Send")).clicked() { self.start_send(); }
                    if ui.button("Cancel").clicked() { self.send_open = false; }
                });
            });
        }
        if self.delete_open {
            egui::Window::new("Delete file?").collapsible(false).show(ctx,|ui|{ui.label("Remove the selected file from this image? A backup is kept beside the image.");ui.horizontal(|ui|{if ui.button("Delete file").clicked(){if let Some(e)=self.picked(){let r=self.store.as_ref().unwrap().lock().unwrap().change(|d|d.remove(&e));self.report(r,"File deleted.");self.selected=None;}self.delete_open=false;}if ui.button("Cancel").clicked(){self.delete_open=false;}});});
        }
        if self.editor_open {
            egui::Window::new("Text file editor")
                .default_size([560.0, 400.0])
                .show(ctx, |ui| {
                    if self.new_text {
                        ui.label("Filename (SEQ)");
                        ui.add(egui::TextEdit::singleline(&mut self.rename).char_limit(16));
                    }
                    ui.label("Saves UTF-8 bytes exactly. This is not a BASIC or PETSCII editor.");
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.edit)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(15),
                            );
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Save to disk").clicked() {
                            let chosen = self.picked();
                            let r =
                                if self.new_text {
                                    self.store.as_ref().unwrap().lock().unwrap().change(|d| {
                                        d.put(&self.rename, 1, self.edit.as_bytes(), false)
                                    })
                                } else if let Some(e) = chosen {
                                    self.store.as_ref().unwrap().lock().unwrap().change(|d| {
                                        d.put(&e.name, e.kind, self.edit.as_bytes(), true)
                                    })
                                } else {
                                    Err("Select a file first.".into())
                                };
                            let ok = r.is_ok();
                            self.report(r, "Text saved.");
                            if ok {
                                self.editor_open = false;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.editor_open = false;
                        }
                    });
                });
        }
    }
}
fn cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str){
        Some("create") if args.len()==4&&args[1].eq_ignore_ascii_case("d65big")=>{Store::create(Path::new(&args[2]),mega65_disklab::launcher::create_editable(&args[3],"65")?)?;println!("Large D65 with START resident reader created. Use add to put files on it; START is rebuilt each time.");},
        Some("send") if args.len()>=3=>{
            let image=fs::canonicalize(&args[1]).map_err(|e|e.to_string())?;Disk::parse(fs::read(&image).map_err(|e|e.to_string())?)?;
            let name=args[2].to_ascii_uppercase();let (mut link,mut mount,mut tool)=(Link::Serial(None),false,None);let mut i=3;
            while i<args.len(){let next=args.get(i+1).filter(|a|!a.starts_with("--")).cloned();match args[i].as_str(){"--mount"=>mount=true,"--ethernet"=>{if next.is_some(){i+=1}link=Link::Ethernet(next)},"--serial"=>{if next.is_some(){i+=1}link=Link::Serial(next)},"--tool"=>{tool=next.ok_or("--tool needs a path")?.into();i+=1},o=>return Err(format!("Unknown option {o}"))}i+=1}
            let tool=match tool{Some(p)=>{let p=PathBuf::from(p);Tool{kind:if p.file_name().unwrap_or_default().to_string_lossy().contains("ftp"){transfer::Kind::Mega65Ftp}else{transfer::Kind::M65rs},path:p}},None=>transfer::preferred(&transfer::find_tools_with(&tool_overrides(&load_settings())),matches!(link,Link::Ethernet(_))).cloned().ok_or("No transfer tool found. Install MEGA65 Studio (m65rs) or M65Connect (mega65_ftp), or pass --tool PATH.")?};
            let tools=transfer::find_tools_with(&tool_overrides(&load_settings()));let steps=transfer::plan(&tool,&tools,&link,&image,&name,mount)?;let tail=transfer::run(&steps,|l|println!("{l}"))?;println!("Sent {name} to the MEGA65's SD card with {}.\n{tail}",tool.name());},
        Some("update-start") if args.len()==2=>{let mut s=Store::open(Path::new(&args[1]))?;if !mega65_disklab::launcher::start_outdated(&s.disk)?{println!("START is already current.");}else{s.change(|d|mega65_disklab::launcher::update_start(d))?;println!("START replaced with the current reader and menu. Files are unchanged; a backup was kept beside the image.");}},
        Some("create") if args.len()==4=>{let format=if args[1].eq_ignore_ascii_case("d81"){Format::D81}else if args[1].eq_ignore_ascii_case("d65"){Format::D65}else{return Err("Format must be d65 or d81".into())};Store::create(Path::new(&args[2]),Disk::create(format,&args[3],"65")?)?;},
        Some("list") if args.len()==2=>{let d=Disk::parse(fs::read(&args[1]).map_err(|e|e.to_string())?)?;println!("{} [{}] {} bytes",d.label(),d.disk_id(),d.bytes.len());for e in d.entries()?{println!("{:5} {:16} {}",e.blocks,e.name,e.type_name())}match d.resident_space().filter(|_|mega65_disklab::launcher::is_editable(&d)){Some((used,total))=>{println!("{} blocks free (resident reader, START included)",total-used);println!("START {}",if mega65_disklab::launcher::start_outdated(&d)?{"is older than this version of MEGA65 DiskLab: run update-start"}else{"is current"});}None=>{println!("{} blocks free",d.free_blocks());d.validate_writable()?;}}},
        Some("add") if args.len()==4=>{let mut s=Store::open(Path::new(&args[1]))?;let data=fs::read(&args[2]).map_err(|e|e.to_string())?;let kind=match Path::new(&args[2]).extension().and_then(|x|x.to_str()).unwrap_or("").to_ascii_lowercase().as_str(){"seq"=>1,"usr"=>3,_=>2};s.change(|d|d.put(&args[3],kind,&data,false))?;},
        Some("launch" | "launch-compat") if args.len()>=3=>{let sources:Result<Vec<_>>=args[2..].iter().map(|p|Disk::parse(fs::read(p).map_err(|e|e.to_string())?)).collect();Store::create(Path::new(&args[1]),mega65_disklab::launcher::build_with_compat(&sources?, args[0]=="launch-compat")?)?;println!("Resident launch disk created. RUN \"START\" from drive 8. About 5.4 MB minus START; 55,041-byte program limit. Direct-SD reader: hardware validation pending.");},
        Some("merge") if args.len()>=4=>{let sources:Result<Vec<_>>=args[2..].iter().map(|p|Disk::parse(fs::read(p).map_err(|e|e.to_string())?)).collect();Store::create(Path::new(&args[1]),Disk::merge_d81(&sources?)?)?;println!("Experimental merge verified offline. Read-only; ROM 920413 cannot load later banks. A modified ROM or custom loader is required.");},
        Some("convert") if args.len()==4=>{let format=match args[3].as_str(){"d81"=>Format::D81,"d65"=>Format::D65,_=>return Err("Format must be d65 or d81".into())};let d=Disk::parse(fs::read(&args[1]).map_err(|e|e.to_string())?)?.convert(format)?;Store::create(Path::new(&args[2]),d)?;},
        #[cfg(any(target_os="macos", windows))]
        Some("direct-sd-helper")=>{println!("{}",mega65_disklab::direct_sd::helper(&args[1..])?);},
        #[cfg(any(target_os="macos", windows))]
        Some("export-sd") if args.len()==3=>{let d=Disk::parse(fs::read(&args[1]).map_err(|e|e.to_string())?)?;let r=mega65_disklab::direct_sd::export(&d,Path::new(&args[2]))?;println!("{}",r.message());},
        #[cfg(any(target_os="macos", windows))]
        Some("serve") if args.len()==2=>{let s=Arc::new(Mutex::new(Store::open(Path::new(&args[1]))?));let b=mega65_disklab::mount::bridge(s,false)?;println!("{}",b.url);loop{std::thread::sleep(std::time::Duration::from_secs(1));}},
        #[cfg(any(target_os="macos", windows))]
        Some("mount") if args.len()==3||args.len()==4=>{let s=Arc::new(Mutex::new(Store::open(Path::new(&args[1]))?));let ro=args.get(3).map(String::as_str)==Some("--read-only");mega65_disklab::mount::start(s,Path::new(&args[2]),ro)?.join().map_err(|_|"Mount worker panicked")??;},
        _=>return Err("Usage: mega65-disklab [IMAGE] | create d65|d81|d65big PATH LABEL | list IMAGE | update-start IMAGE | send IMAGE NAME.D65 [--serial [PORT] | --ethernet [IPV6]] [--mount] [--tool PATH] | add IMAGE FILE NAME | convert INPUT OUTPUT d65|d81 | export-sd INPUT NEW_OUTPUT | merge OUTPUT.d65 FIRST.d81 SECOND.d81 [THIRD.d81] | launch OUTPUT.d65 FIRST.d81 [MORE.d81 ...] | launch-compat OUTPUT.d65 FIRST.d81 [MORE.d81 ...] | mount IMAGE EMPTY_FOLDER [--read-only]".into())
    }
    Ok(())
}
fn main() -> eframe::Result<()> {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() && !(args.len() == 1 && Path::new(&args[0]).is_file()) {
        if let Err(e) = cli(&args) {
            #[cfg(windows)]
            if args.first().map(String::as_str) == Some("direct-sd-helper") {
                mega65_disklab::windows::show_error(&e);
            }
            eprintln!("{e}");
            std::process::exit(1)
        }
        return Ok(());
    }
    let mut app = Studio::default();
    if let Some(p) = args.first() {
        app.load(p.into());
    }
    eframe::run_native(
        "MEGA65 DiskLab",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                // No minimum size: on macOS a minimum is applied twice after the
                // window moves between Retina and standard displays, leaving a
                // window larger than the screen that cannot be shrunk.
                .with_inner_size([1120.0, 790.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut v = egui::Visuals::dark();
            v.panel_fill = Color32::from_rgb(13, 20, 27);
            v.window_fill = Color32::from_rgb(22, 32, 42);
            v.selection.bg_fill = Color32::from_rgb(27, 91, 80);
            v.selection.stroke.color = Color32::WHITE;
            cc.egui_ctx.set_visuals(v);
            let mut style = (*cc.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(10.0, 9.0);
            style.spacing.button_padding = egui::vec2(12.0, 8.0);
            cc.egui_ctx.set_style(style);
            Ok(Box::new(app))
        }),
    )
}
