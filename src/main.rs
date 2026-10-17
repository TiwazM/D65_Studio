#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
use d65_studio::{
    disk::{Disk, Entry, Format, Result},
    store::{atomic_write, Store},
};
use eframe::egui::{self, Color32, RichText};
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
    dialog: Option<(Pick, Dialog)>,
    rename: String,
    edit: String,
    delete_open: bool,
    editor_open: bool,
    new_text: bool,
    readonly: bool,
    mountpoint: Option<PathBuf>,
    sd_task: Option<std::thread::JoinHandle<Result<d65_studio::sd_export::ExportReport>>>,
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
            Pick::Create(d) => match Store::create(&first, d) {
                Ok(s) => {
                    let big = d65_studio::launcher::is_editable(&s.disk);
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
            let stale = d65_studio::launcher::start_outdated(disk).unwrap_or(false);
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
                let big = d65_studio::launcher::is_editable(&s.disk);
                let warning = s.disk.validate_writable().err().filter(|_| !big);
                self.readonly = warning.is_some() || big;
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
                    .set_title("Add PRG, SEQ or USR files")
                    .add_filter("Commodore files", &["prg", "seq", "usr"])
                    .pick_files(),
            ),
        );
    }
    fn import_paths(&mut self, paths: Vec<PathBuf>) {
        let r = (|| {
            let mut files = Vec::new();
            for p in paths {
                let ext = p
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let kind = match ext.as_str() {
                    "prg" => 2,
                    "seq" => 1,
                    "usr" => 3,
                    _ => return Err("Use .prg, .seq or .usr files.".into()),
                };
                let name = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                files.push((name, kind, fs::read(p).map_err(|e| e.to_string())?));
            }
            self.store
                .as_ref()
                .ok_or("No image")?
                .lock()
                .unwrap()
                .change(|d| {
                    for (name, kind, data) in &files {
                        d.put(name, *kind, data, false)?
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
        let name = d65_studio::sd_export::suggested_name(&source, disk.format);
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
                d65_studio::direct_sd::export_with_progress(&disk, &destination, |message| {
                    *progress.lock().unwrap() = message;
                })
            }
            #[cfg(target_os = "macos")]
            d65_studio::direct_sd::export(&disk, &destination)
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
                d65_studio::launcher::build_with_compat(&sources, self.load_compatibility)?
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
                d65_studio::mount::available()?;
                let p = d65_studio::mount::free_drive()?;
                let h = d65_studio::mount::start(
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
            if let Err(e) = d65_studio::mount::available() {
                self.report(Err(e), "");
                return;
            }
            let label = self.store.as_ref().unwrap().lock().unwrap().disk.label();
            match d65_studio::mount::new_mountpoint(&label) {
                Ok(p) => self.mount_at(p),
                Err(e) => self.report(Err(e), ""),
            }
        }
        #[cfg(not(any(target_os="macos", windows)))] self.report(Err("file manager mounting is available on macOS. Windows drive mounting is not implemented yet.".into()),"");
    }
    #[allow(unused_variables)]
    fn mount_at(&mut self, p: PathBuf) {
        #[cfg(target_os = "macos")]
        match d65_studio::mount::start(self.store.as_ref().unwrap().clone(), &p, self.readonly) {
            Ok(h) => {
                self.mount_thread = Some(h);
                self.mountpoint = Some(p);
                self.report(Ok(()),"Mounting. The D65 Studio volume opens in Finder; eject it before editing here.")
            }
            Err(e) => {
                d65_studio::mount::remove_mountpoint(&p);
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
        if self.sd_task.as_ref().is_some_and(|task| task.is_finished()) {
            match self
                .sd_task
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err("SD export worker stopped unexpectedly.".into()))
            {
                Ok(report) => {
                    self.error = !matches!(report.layout, d65_studio::sd_export::Layout::Verified);
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
                d65_studio::mount::remove_mountpoint(p);
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
                Err("Eject the file manager volume before closing D65 Studio.".into()),
                "",
            );
        }
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(19, 28, 35))
                    .inner_margin(22.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("▣").size(32.0).color(TEAL));
                    ui.vertical(|ui| {
                        ui.label(RichText::new("D65 STUDIO").size(25.0).strong());
                        ui.label(
                            RichText::new("A disk workbench for your MEGA65").color(Color32::GRAY),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !mounted,
                                egui::Button::new("Open image…").min_size(egui::vec2(115.0, 36.0)),
                            )
                            .clicked()
                        {
                            self.ask(
                                Pick::Open,
                                one(rfd::AsyncFileDialog::new()
                                    .add_filter("MEGA65 images", &["d65", "d81"])
                                    .pick_file()),
                            );
                        }
                        if ui
                            .add_enabled(!mounted, egui::Button::new("Merge D81s…"))
                            .clicked()
                        {
                            self.merge_open = true;
                        }
                        if ui
                            .add_enabled(
                                !mounted,
                                egui::Button::new("+ New disk")
                                    .fill(Color32::from_rgb(30, 90, 78))
                                    .min_size(egui::vec2(110.0, 36.0)),
                            )
                            .clicked()
                        {
                            self.new_open = true;
                        }
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
        let snapshot = self.store.as_ref().map(|s| {
            let s = s.lock().unwrap();
            (s.disk.clone(), s.path.clone(), s.backup.clone())
        });
        if let Some((disk, path, backup)) = snapshot {
            let writable = disk.validate_writable();
            let big = d65_studio::launcher::is_editable(&disk);
            let can_edit = (writable.is_ok() || big) && !mounted;
            let start_stale = big && self.start_stale(&disk);
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
                    ui.label(RichText::new(if start_stale { "START is older than this version of D65 Studio." } else { "START is current for this version of D65 Studio." }).small().color(if start_stale { Color32::from_rgb(215,184,117) } else { Color32::GRAY }));
                    if ui.add_enabled(can_edit, egui::Button::new("Update START")).clicked() {
                        let r = self.store.as_ref().unwrap().lock().unwrap().change(|d| d65_studio::launcher::update_start(d));
                        self.report(r, "START replaced with the current reader and menu. Files are unchanged.");
                    }
                } else { ui.label(RichText::new("READ-ONLY INSPECTION").color(TEAL));ui.label("Free space is not available for this layout."); }
                ui.add_space(14.0);ui.label(format!("Container: {} bytes",disk.bytes.len()));
                if disk.format==Format::D65{ui.add_space(8.0);ui.label(RichText::new("Standard and resident images are editable. Experimental merges are read-only. Later banks require the START reader or compatible software.").color(Color32::from_rgb(215,184,117)));}
                ui.separator();ui.label(RichText::new(if cfg!(windows) { "EXPLORER DRIVE" } else { "FINDER VOLUME" }).small().color(Color32::GRAY));
                if mounted{
                    ui.label("Mounted — editing in file manager");
                    if ui.button("Eject volume").clicked(){#[cfg(any(target_os="macos", windows))] {let r=d65_studio::mount::unmount(self.mountpoint.as_ref().unwrap());self.report(r,"Eject requested.");}}
                    if ui.button(if cfg!(windows) { "Show in Explorer" } else { "Show in Finder" }).clicked(){#[cfg(any(target_os="macos", windows))] {let _=std::process::Command::new(if cfg!(windows) { "explorer.exe" } else { "open" }).arg(if cfg!(windows) { PathBuf::from(format!("{}\\", self.mountpoint.as_ref().unwrap().display())) } else { self.mountpoint.as_ref().unwrap().clone() }).spawn();}}
                }else{
                    if writable.is_err() { self.readonly=true; } ui.add_enabled(writable.is_ok(),egui::Checkbox::new(&mut self.readonly,"Mount read-only"));if ui.add_enabled(!self.new_open&&!self.editor_open&&!self.delete_open,egui::Button::new(if cfg!(windows) { "Mount in Explorer…" } else { "Mount in Finder" })).clicked(){self.start_mount()}
                    ui.label(RichText::new(if cfg!(windows) { "Built-in Windows WebClient · no driver installation" } else { "Built-in macOS connection · no driver needed" }).small().color(TEAL));
                }
                ui.add_space(10.0);ui.label(RichText::new("Use .prg, .seq or .usr files with names up to 16 characters. The disk has a flat directory.").small());
                ui.separator();if ui.add_enabled(!mounted,egui::Button::new("Export as D81…")).clicked(){self.convert(Format::D81)}if ui.add_enabled(!mounted,egui::Button::new("Export as D65…")).clicked(){self.convert(Format::D65)}
                #[cfg(any(target_os="macos", windows))]
                {
                    ui.add_space(8.0);
                    if ui.add_enabled(!mounted, egui::Button::new("Export to SD card…")).clicked() { self.export_sd(); }
                    ui.label(RichText::new(if cfg!(windows) { "Choose an existing folder on the SD card and a new filename. Temporarily unmounts the card; administrator access is requested." } else { "Choose the card root and a new filename. Temporarily unmounts the card; administrator access is requested." }).small());
                }
                ui.add_space(12.0);ui.label(RichText::new(path.display().to_string()).small().color(Color32::GRAY));if backup.is_some(){ui.label(RichText::new("Original backed up beside image").small().color(TEAL));}
            }); });
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
                            if ui
                                .add_enabled(can_edit, egui::Button::new("New text file"))
                                .clicked()
                            {
                                self.edit.clear();
                                self.rename = "README".into();
                                self.new_text = true;
                                self.editor_open = true;
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
            egui::CentralPanel::default().show(ctx,|ui|{ui.add_space(75.0);ui.vertical_centered(|ui|{ui.label(RichText::new("Your next disk starts here.").size(35.0).strong());ui.add_space(15.0);ui.label("Create a MEGA65 D65 image, browse its files, and take it into file manager.");ui.label("D81 images work too. Everything stays on this computer.");ui.add_space(28.0);if ui.button(RichText::new("Create a disk").size(20.0)).clicked(){self.new_open=true;}ui.add_space(38.0);ui.label(RichText::new("D65 support is based on the MEGA65 Freezer layout.\nSTART can load extra-sector programs; ordinary editing uses D81 capacity.").color(Color32::from_rgb(215,184,117)));});});
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
                                d65_studio::launcher::create_editable(&self.label, &self.id)
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
        Some("create") if args.len()==4&&args[1].eq_ignore_ascii_case("d65big")=>{Store::create(Path::new(&args[2]),d65_studio::launcher::create_editable(&args[3],"65")?)?;println!("Large D65 with START resident reader created. Use add to put files on it; START is rebuilt each time.");},
        Some("update-start") if args.len()==2=>{let mut s=Store::open(Path::new(&args[1]))?;if !d65_studio::launcher::start_outdated(&s.disk)?{println!("START is already current.");}else{s.change(|d|d65_studio::launcher::update_start(d))?;println!("START replaced with the current reader and menu. Files are unchanged; a backup was kept beside the image.");}},
        Some("create") if args.len()==4=>{let format=if args[1].eq_ignore_ascii_case("d81"){Format::D81}else if args[1].eq_ignore_ascii_case("d65"){Format::D65}else{return Err("Format must be d65 or d81".into())};Store::create(Path::new(&args[2]),Disk::create(format,&args[3],"65")?)?;},
        Some("list") if args.len()==2=>{let d=Disk::parse(fs::read(&args[1]).map_err(|e|e.to_string())?)?;println!("{} [{}] {} bytes",d.label(),d.disk_id(),d.bytes.len());for e in d.entries()?{println!("{:5} {:16} {}",e.blocks,e.name,e.type_name())}match d.resident_space().filter(|_|d65_studio::launcher::is_editable(&d)){Some((used,total))=>{println!("{} blocks free (resident reader, START included)",total-used);println!("START {}",if d65_studio::launcher::start_outdated(&d)?{"is older than this version of D65 Studio: run update-start"}else{"is current"});}None=>{println!("{} blocks free",d.free_blocks());d.validate_writable()?;}}},
        Some("add") if args.len()==4=>{let mut s=Store::open(Path::new(&args[1]))?;let data=fs::read(&args[2]).map_err(|e|e.to_string())?;let kind=match Path::new(&args[2]).extension().and_then(|x|x.to_str()).unwrap_or("").to_ascii_lowercase().as_str(){"seq"=>1,"usr"=>3,_=>2};s.change(|d|d.put(&args[3],kind,&data,false))?;},
        Some("launch" | "launch-compat") if args.len()>=3=>{let sources:Result<Vec<_>>=args[2..].iter().map(|p|Disk::parse(fs::read(p).map_err(|e|e.to_string())?)).collect();Store::create(Path::new(&args[1]),d65_studio::launcher::build_with_compat(&sources?, args[0]=="launch-compat")?)?;println!("Resident launch disk created. RUN \"START\" from drive 8. About 5.4 MB minus START; 55,041-byte program limit. Direct-SD reader: hardware validation pending.");},
        Some("merge") if args.len()>=4=>{let sources:Result<Vec<_>>=args[2..].iter().map(|p|Disk::parse(fs::read(p).map_err(|e|e.to_string())?)).collect();Store::create(Path::new(&args[1]),Disk::merge_d81(&sources?)?)?;println!("Experimental merge verified offline. Read-only; ROM 920413 cannot load later banks. A modified ROM or custom loader is required.");},
        Some("convert") if args.len()==4=>{let format=match args[3].as_str(){"d81"=>Format::D81,"d65"=>Format::D65,_=>return Err("Format must be d65 or d81".into())};let d=Disk::parse(fs::read(&args[1]).map_err(|e|e.to_string())?)?.convert(format)?;Store::create(Path::new(&args[2]),d)?;},
        #[cfg(any(target_os="macos", windows))]
        Some("direct-sd-helper")=>{println!("{}",d65_studio::direct_sd::helper(&args[1..])?);},
        #[cfg(any(target_os="macos", windows))]
        Some("export-sd") if args.len()==3=>{let d=Disk::parse(fs::read(&args[1]).map_err(|e|e.to_string())?)?;let r=d65_studio::direct_sd::export(&d,Path::new(&args[2]))?;println!("{}",r.message());},
        #[cfg(any(target_os="macos", windows))]
        Some("serve") if args.len()==2=>{let s=Arc::new(Mutex::new(Store::open(Path::new(&args[1]))?));let b=d65_studio::mount::bridge(s,false)?;println!("{}",b.url);loop{std::thread::sleep(std::time::Duration::from_secs(1));}},
        #[cfg(any(target_os="macos", windows))]
        Some("mount") if args.len()==3||args.len()==4=>{let s=Arc::new(Mutex::new(Store::open(Path::new(&args[1]))?));let ro=args.get(3).map(String::as_str)==Some("--read-only");d65_studio::mount::start(s,Path::new(&args[2]),ro)?.join().map_err(|_|"Mount worker panicked")??;},
        _=>return Err("Usage: d65-studio [IMAGE] | create d65|d81|d65big PATH LABEL | list IMAGE | update-start IMAGE | add IMAGE FILE NAME | convert INPUT OUTPUT d65|d81 | export-sd INPUT NEW_OUTPUT | merge OUTPUT.d65 FIRST.d81 SECOND.d81 [THIRD.d81] | launch OUTPUT.d65 FIRST.d81 [MORE.d81 ...] | launch-compat OUTPUT.d65 FIRST.d81 [MORE.d81 ...] | mount IMAGE EMPTY_FOLDER [--read-only]".into())
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
                d65_studio::windows::show_error(&e);
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
        "D65 Studio",
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
