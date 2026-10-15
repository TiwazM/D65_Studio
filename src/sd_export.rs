#[cfg(any(target_os = "macos", test))]
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
};
// Legacy macOS reservation exporter, retained for regression tests.
use crate::disk::{Disk, Format, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Layout {
    Verified,
    ReservationOnly(String),
}
#[derive(Debug)]
pub struct ExportReport {
    pub path: PathBuf,
    pub bytes: usize,
    pub layout: Layout,
}
impl ExportReport {
    pub fn message(&self) -> String {
        let detail = match &self.layout {
            Layout::Verified => "Physical layout verified contiguous.".to_string(),
            Layout::ReservationOnly(reason) => format!("macOS accepted the contiguous reservation, but independent layout verification is unavailable: {reason}. Test mounting this copy on the MEGA65 before relying on it."),
        };
        format!("Saved {} ({} bytes). Contents verified byte-for-byte. {} Safely eject the SD card before removing it.", self.path.display(), self.bytes, detail)
    }
}
pub fn suggested_name(path: &Path, format: Format) -> String {
    let stem: String = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(8)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    format!(
        "{}.{}",
        if stem.is_empty() { "DISK" } else { &stem },
        format.ext().to_ascii_uppercase()
    )
}
#[cfg(any(target_os = "macos", test))]
fn validate_name(path: &Path, format: Format) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Choose an ASCII filename.")?;
    let (stem, ext) = name
        .rsplit_once('.')
        .ok_or("Include the image extension.")?;
    if stem.is_empty()
        || stem.len() > 8
        || !stem
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        || ext != format.ext().to_ascii_uppercase()
    {
        return Err(format!(
            "Use 1–8 uppercase letters, digits, - or _, followed by .{}; for example GAMES2.{}.",
            format.ext().to_ascii_uppercase(),
            format.ext().to_ascii_uppercase()
        ));
    }
    Ok(())
}
// The adapter allows tests to inject real allocation/verification failure modes.
#[cfg(any(target_os = "macos", test))]
trait Allocation {
    fn check_volume(&self, directory: &File, format: Format) -> Result<()>;
    fn reserve(&self, file: &File, bytes: u64) -> Result<u64>;
    fn extent(&self, file: &File, offset: u64, remaining: u64) -> std::io::Result<(u64, u64)>;
}
#[cfg(any(target_os = "macos", test))]
fn layout(file: &File, size: u64, allocator: &impl Allocation) -> Result<Layout> {
    let mut offset = 0;
    let mut base: Option<u64> = None;
    while offset < size {
        let (physical, length) = match allocator.extent(file, offset, size - offset) {
            Ok(e) => e,
            Err(e) => {
                if matches!(
                    e.raw_os_error(),
                    Some(libc::ENOTSUP | libc::ENOSYS | libc::EINVAL | libc::EPERM | libc::EACCES)
                ) {
                    return Ok(Layout::ReservationOnly(e.to_string()));
                }
                return Err(format!("Cannot verify physical layout: {e}"));
            }
        };
        if length == 0 || physical % 512 != 0 {
            return Err("Filesystem returned an invalid physical extent; export cancelled.".into());
        }
        let start = *base.get_or_insert(physical);
        if start.checked_add(offset) != Some(physical) {
            return Err("The saved image is fragmented despite the allocation request; export cancelled. No destination image was published.".into());
        }
        offset += length.min(size - offset);
    }
    Ok(Layout::Verified)
}
#[cfg(any(target_os = "macos", test))]
fn check_bytes(file: &mut File, bytes: &[u8]) -> Result<()> {
    if file.metadata().map_err(|e| e.to_string())?.len() != bytes.len() as u64 {
        return Err("Export size verification failed.".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 65536];
    for chunk in bytes.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])
            .map_err(|e| format!("Export read-back failed: {e}"))?;
        if buffer[..chunk.len()] != *chunk {
            return Err("Export read-back differs from the source image.".into());
        }
    }
    Ok(())
}
#[cfg(any(target_os = "macos", test))]
fn export_with(
    disk: &Disk,
    destination: &Path,
    allocator: &impl Allocation,
) -> Result<ExportReport> {
    validate_name(destination, disk.format)?;
    if disk.bytes.len() != disk.format.size() {
        return Err("Image size does not match its format.".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent =
        fs::canonicalize(parent).map_err(|e| format!("Cannot open destination folder: {e}"))?;
    let destination = parent.join(destination.file_name().unwrap());
    if fs::symlink_metadata(&destination).is_ok() {
        return Err("That filename already exists. Choose a new name; SD export never overwrites existing files.".into());
    }
    #[cfg(not(windows))]
    let directory = File::open(&parent).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let directory = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x02000000)
            .open(&parent)
            .map_err(|e| e.to_string())?
    };
    allocator.check_volume(&directory, disk.format)?;
    // Publish via macOS no-replace rename, preserving allocation instead of copying.
    // NamedTempFile removes incomplete output on failures before publication.
    let mut temporary = tempfile::Builder::new()
        .prefix(".d65-export-")
        .tempfile_in(&parent)
        .map_err(|e| format!("Cannot create SD export: {e}"))?;
    let file = temporary.as_file_mut();
    allocator.check_volume(file, disk.format)?;
    let size = disk.bytes.len() as u64;
    let reserved = allocator.reserve(file, size)?;
    if reserved < size {
        return Err(format!(
            "Only {reserved} of {size} bytes were reserved; export cancelled."
        ));
    }
    file.set_len(size)
        .map_err(|e| format!("Cannot set image length: {e}"))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(&disk.bytes)
        .map_err(|e| format!("SD-card write failed: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("SD-card flush failed: {e}"))?;
    check_bytes(file, &disk.bytes)?;
    let layout = layout(file, size, allocator)?;
    let published = temporary.persist_noclobber(&destination).map_err(|e| {
        format!(
            "Cannot publish SD export without replacing an existing file: {}",
            e.error
        )
    })?;
    published.sync_all().map_err(|e| {
        format!(
            "Image exists at {}, but its final flush failed: {e}. Do not use this copy yet.",
            destination.display()
        )
    })?;
    Ok(ExportReport {
        path: destination,
        bytes: disk.bytes.len(),
        layout,
    })
}
#[cfg(target_os = "macos")]
struct MacAllocation;
#[cfg(target_os = "macos")]
impl Allocation for MacAllocation {
    fn check_volume(&self, directory: &File, format: Format) -> Result<()> {
        use std::{ffi::CStr, os::fd::AsRawFd};
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(directory.as_raw_fd(), &mut info) } != 0 {
            return Err(format!(
                "Cannot identify destination filesystem: {}",
                std::io::Error::last_os_error()
            ));
        }
        let kind = unsafe { CStr::from_ptr(info.f_fstypename.as_ptr()) }.to_string_lossy();
        if kind != "msdos" {
            return Err(format!("Choose a folder on your MEGA65 FAT32 SD card. This destination uses {kind}, not FAT."));
        }
        if format == Format::D65 && info.f_bsize != 4096 {
            return Err(format!("This FAT volume reports {}-byte allocation blocks. D65 mounting on core 0.97.2 expects 4096-byte clusters; export cancelled.", info.f_bsize));
        }
        Ok(())
    }
    fn reserve(&self, file: &File, bytes: u64) -> Result<u64> {
        use std::os::fd::AsRawFd;
        let mut request = libc::fstore_t {
            fst_flags: libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: bytes as i64,
            fst_bytesalloc: 0,
        };
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut request) } == -1 {
            return Err(format!("macOS could not reserve one contiguous area: {}. The card may lack a large enough free run, or its driver may not support contiguous allocation. No ordinary-copy fallback was used and no destination image was created.", std::io::Error::last_os_error()));
        }
        u64::try_from(request.fst_bytesalloc)
            .map_err(|_| "Filesystem returned an invalid allocation size.".into())
    }
    fn extent(&self, file: &File, offset: u64, remaining: u64) -> std::io::Result<(u64, u64)> {
        use std::os::fd::AsRawFd;
        let mut query = libc::log2phys {
            l2p_flags: 0,
            l2p_contigbytes: remaining as i64,
            l2p_devoffset: offset as i64,
        };
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_LOG2PHYS_EXT, &mut query) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let physical = query.l2p_devoffset;
        let count = query.l2p_contigbytes;
        if physical < 0 || count <= 0 {
            return Err(std::io::Error::other(
                "Filesystem returned an unmapped or empty extent",
            ));
        }
        Ok((physical as u64, count as u64))
    }
}
pub fn export(disk: &Disk, destination: &Path) -> Result<ExportReport> {
    #[cfg(target_os = "macos")]
    {
        export_with(disk, destination, &MacAllocation)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (disk, destination);
        Err("Contiguous SD-card export is currently implemented for macOS only.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Copy)]
    enum Mode {
        Good,
        ReserveFails,
        Short,
        Fragmented,
        Unsupported,
        IoError,
        Invalid,
        Race,
    }
    struct Fake {
        mode: Mode,
        dest: PathBuf,
    }
    impl Allocation for Fake {
        fn check_volume(&self, _: &File, _: Format) -> Result<()> {
            Ok(())
        }
        fn reserve(&self, f: &File, n: u64) -> Result<u64> {
            assert_eq!(f.metadata().unwrap().len(), 0);
            match self.mode {
                Mode::ReserveFails => Err("Allocation rejected".into()),
                Mode::Short => Ok(n - 1),
                Mode::Race => {
                    fs::write(&self.dest, b"existing").unwrap();
                    Ok(n)
                }
                _ => Ok(n),
            }
        }
        fn extent(&self, _: &File, offset: u64, _: u64) -> std::io::Result<(u64, u64)> {
            match self.mode {
                Mode::Unsupported => Err(std::io::Error::from_raw_os_error(libc::ENOTSUP)),
                Mode::IoError => Err(std::io::Error::from_raw_os_error(libc::EIO)),
                Mode::Fragmented if offset > 0 => Ok((4096 + offset + 4096, 4096)),
                Mode::Invalid => Ok((4096, 0)),
                _ => Ok((4096 + offset, 4096)),
            }
        }
    }
    fn sample() -> Disk {
        let mut a = Disk::create(Format::D81, "A", "65").unwrap();
        let mut b = Disk::create(Format::D81, "B", "65").unwrap();
        a.put("FIRST", 2, b"first", false).unwrap();
        b.put("SECOND", 2, b"second", false).unwrap();
        let disk = Disk::merge_d81(&[a, b]).unwrap();
        assert!(disk.validate_writable().is_err());
        disk
    }
    #[test]
    fn exact_export_with_adjacent_extent_responses() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("TEST.D65");
        let disk = sample();
        let report = export_with(
            &disk,
            &dest,
            &Fake {
                mode: Mode::Good,
                dest: dest.clone(),
            },
        )
        .unwrap();
        assert_eq!(report.layout, Layout::Verified);
        assert_eq!(fs::read(dest).unwrap(), disk.bytes);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn failed_exports_leave_no_image_or_temporary_file() {
        for mode in [
            Mode::ReserveFails,
            Mode::Short,
            Mode::Fragmented,
            Mode::IoError,
            Mode::Invalid,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("TEST.D65");
            assert!(export_with(
                &sample(),
                &dest,
                &Fake {
                    mode,
                    dest: dest.clone()
                }
            )
            .is_err());
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }
    #[test]
    fn unsupported_layout_is_explicitly_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("TEST.D65");
        let report = export_with(
            &sample(),
            &dest,
            &Fake {
                mode: Mode::Unsupported,
                dest: dest.clone(),
            },
        )
        .unwrap();
        assert!(matches!(report.layout, Layout::ReservationOnly(_)));
        assert!(report
            .message()
            .contains("independent layout verification is unavailable"));
        assert_eq!(fs::read(dest).unwrap(), sample().bytes);
    }
    #[test]
    fn existing_files_and_publish_races_are_not_overwritten() {
        for existing in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("TEST.D65");
            if existing {
                fs::write(&dest, b"existing").unwrap();
            }
            assert!(export_with(
                &sample(),
                &dest,
                &Fake {
                    mode: Mode::Race,
                    dest: dest.clone()
                }
            )
            .is_err());
            assert_eq!(fs::read(&dest).unwrap(), b"existing");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }
    #[test]
    fn readback_detects_corruption_and_truncation() {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(b"wrong").unwrap();
        assert!(check_bytes(&mut f, b"right").is_err());
        assert!(check_bytes(&mut f, b"longer").is_err());
    }
    #[test]
    fn sd_names_are_short_uppercase_with_correct_extension() {
        for n in ["", "..", "A.d65", "TOOLONGGG.D65", "TEST.D81", "A B.D65"] {
            assert!(validate_name(Path::new(n), Format::D65).is_err());
        }
        assert_eq!(
            suggested_name(Path::new("A launch disk.d65"), Format::D65),
            "ALAUNCHD.D65"
        );
        assert!(validate_name(Path::new("GAMES2.D65"), Format::D65).is_ok());
    }
}
