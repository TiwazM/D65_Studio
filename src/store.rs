use crate::disk::{Disk, Result};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
pub struct Store {
    pub disk: Disk,
    pub path: PathBuf,
    saved: Vec<u8>,
    _lock: File,
    lock_path: PathBuf,
    pub backup: Option<PathBuf>,
}
/// The lock file beside the image goes away with the Store that held it.
impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
        let lock_path = path.with_extension(format!(
            "{}.lock",
            path.extension().unwrap_or_default().to_string_lossy()
        ));
        // Another process may delete the lock file as it closes the image, so
        // make sure the file we locked is still the one at that path.
        let lock = loop {
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|e| e.to_string())?;
            lock.try_lock_exclusive().map_err(|_| {
                "This image is already open in another MEGA65 DiskLab process.".to_string()
            })?;
            if same_file(&lock, &lock_path) {
                break lock;
            }
        };
        let saved = fs::read(&path).map_err(|e| e.to_string())?;
        let disk = Disk::parse(saved.clone())?;
        Ok(Self {
            disk,
            path,
            saved,
            _lock: lock,
            lock_path,
            backup: None,
        })
    }
    pub fn create(path: &Path, disk: Disk) -> Result<Self> {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        f.write_all(&disk.bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        Self::open(path)
    }
    pub fn change(&mut self, edit: impl FnOnce(&mut Disk) -> Result<()>) -> Result<()> {
        let current = fs::read(&self.path).map_err(|e| e.to_string())?;
        if current != self.saved {
            return Err(
                "Image changed outside MEGA65 DiskLab. Close and reopen it before editing.".into(),
            );
        }
        let mut next = self.disk.clone();
        edit(&mut next)?;
        // Editable resident images are verified by the launcher's repack.
        if !crate::launcher::is_editable(&next) {
            next.validate_writable()?;
        }
        if self.backup.is_none() {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = self
                .path
                .with_extension(format!("{}.{}.bak", self.disk.format.ext(), stamp));
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&p)
                .map_err(|e| e.to_string())?;
            f.write_all(&self.saved)
                .and_then(|_| f.sync_all())
                .map_err(|e| e.to_string())?;
            self.backup = Some(p);
        }
        atomic_write(&self.path, &next.bytes)?;
        self.saved = next.bytes.clone();
        self.disk = next;
        Ok(())
    }
}
#[cfg(unix)]
fn same_file(f: &File, p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (f.metadata(), fs::metadata(p)) {
        (Ok(a), Ok(b)) => a.ino() == b.ino() && a.dev() == b.dev(),
        _ => false,
    }
}
#[cfg(not(unix))]
fn same_file(_f: &File, p: &Path) -> bool {
    p.exists()
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    f.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
