//! macOS boundary for the FAT writer. The GUI never opens a raw disk: a separate
//! invocation of this same bundled executable obtains OS authorization, verifies
//! the chosen external partition, unmounts it, and opens it exclusively.
use crate::{
    disk::{Disk, Result},
    fat_export::{self, Device, Patch, Sector},
    sd_export::{ExportReport, Layout},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const REMOVABLE_ACCESS: &str = "macOS blocked MEGA65 DiskLab from the SD card. Open System Settings › Privacy & Security › Files & Folders and turn on Removable Volumes for MEGA65 DiskLab (or add MEGA65 DiskLab to Full Disk Access), then export again.";

fn command(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "{}: {} {}",
            program,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}
fn field(plist: &[u8], key: &str) -> Result<String> {
    let mut child = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(plist)
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "macOS did not provide the required disk property {key}."
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}
#[derive(Debug, PartialEq)]
struct Identity {
    device: String,
    whole: String,
    uuid: String,
    size: u64,
}
fn disk_id(s: &str, partition: bool) -> bool {
    let Some(s) = s.strip_prefix("disk") else {
        return false;
    };
    if partition {
        let Some((a, b)) = s.split_once('s') else {
            return false;
        };
        !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|b| b.is_ascii_digit())
    } else {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
}
fn inspect(path: &str) -> Result<(Identity, Option<PathBuf>)> {
    let plist = command("/usr/sbin/diskutil", &["info", "-plist", path])?;
    let whole = field(&plist, "ParentWholeDisk")?;
    if !disk_id(&whole, false) {
        return Err("Invalid parent disk identifier.".into());
    }
    let parent = command("/usr/sbin/diskutil", &["info", "-plist", &whole])?;
    let identity = identity_from_plists(&plist, &parent)?;
    let mount = mounted_path(&identity.device)?;
    Ok((identity, mount))
}

// diskutil's XML schema uses WholeDisk; "Whole" is a text-output label.
// Mount status is read independently below, not from a nonexistent Mounted key.
fn identity_from_plists(plist: &[u8], parent: &[u8]) -> Result<Identity> {
    let device = field(&plist, "DeviceIdentifier")?;
    let whole = field(&plist, "ParentWholeDisk")?;
    if !disk_id(&device, true)
        || !disk_id(&whole, false)
        || !device.starts_with(&format!("{whole}s"))
        || field(&plist, "Internal")? != "false"
        || field(&plist, "WholeDisk")? != "false"
        || field(&plist, "FilesystemType")? != "msdos"
    {
        return Err("Select the root of an external FAT32 SD-card partition. Internal disks and other filesystems are refused.".into());
    }
    if field(&parent, "DeviceIdentifier")? != whole
        || field(&parent, "Internal")? != "false"
        || field(&parent, "WholeDisk")? != "true"
        || field(&parent, "VirtualOrPhysical")? != "Physical"
    {
        return Err("The destination must be an external physical disk, not an internal disk or mounted disk image.".into());
    }
    let uuid = field(&plist, "VolumeUUID")?;
    if uuid.is_empty() {
        return Err("The card has no stable volume identity.".into());
    }
    // "Size" is the partition length, matching the raw device. "TotalSize" is
    // the mounted filesystem's capacity and changes once the volume unmounts.
    let size = field(&plist, "Size")?
        .parse::<u64>()
        .map_err(|_| "Invalid partition size")?;
    if size == 0 || size % 512 != 0 {
        return Err("Invalid partition size.".into());
    }
    Ok(Identity {
        device,
        whole,
        uuid,
        size,
    })
}

fn mounted_path(device: &str) -> Result<Option<PathBuf>> {
    use std::{
        ffi::{CStr, OsStr},
        os::unix::ffi::OsStrExt,
    };
    if !disk_id(device, true) {
        return Err("Invalid partition identifier.".into());
    }
    // Use a caller-owned array; getmntinfo uses shared static storage. Fail
    // closed if enumeration fails or grows past our spare capacity.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if !(1..=100000).contains(&count) {
        return Err("Cannot read the macOS mount table.".into());
    }
    let mut mounts: Vec<libc::statfs> = (0..count + 16)
        .map(|_| unsafe { std::mem::zeroed() })
        .collect();
    let size = (mounts.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    let used = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    if used <= 0 || used as usize >= mounts.len() {
        return Err("The macOS mount table changed or could not be read. Retry the export.".into());
    }
    let source = format!("/dev/{device}");
    for m in &mounts[..used as usize] {
        let from = unsafe { CStr::from_ptr(m.f_mntfromname.as_ptr()) };
        if from.to_bytes() == source.as_bytes() {
            let at = unsafe { CStr::from_ptr(m.f_mntonname.as_ptr()) };
            let path = PathBuf::from(OsStr::from_bytes(at.to_bytes()));
            if !path.is_absolute() {
                return Err("Invalid mount point returned by macOS.".into());
            }
            return Ok(Some(path));
        }
    }
    Ok(None)
}
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn apple_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

pub fn export(disk: &Disk, destination: &Path) -> Result<ExportReport> {
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid destination filename")?;
    fat_export::short_name(name)?;
    if destination.exists() {
        return Err("That filename already exists. Choose a new filename.".into());
    }
    let folder = fs::canonicalize(
        destination
            .parent()
            .ok_or("Choose the card's root folder")?,
    )
    .map_err(|e| e.to_string())?;
    // macOS checks the Removable Volumes permission of the app that started the
    // privileged helper before it may open the card's raw device. Listing the
    // card here, from MEGA65 DiskLab itself, makes macOS ask for that permission.
    fs::read_dir(&folder).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            REMOVABLE_ACCESS.to_string()
        } else {
            e.to_string()
        }
    })?;
    let (id, mount) = inspect(folder.to_str().ok_or("Invalid destination path")?)?;
    if mount.as_ref() != Some(&folder) {
        return Err(
            "Choose the SD card's root folder. Direct export does not yet support subfolders."
                .into(),
        );
    }
    let job = tempfile::Builder::new()
        .prefix("d65-direct-export-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    fs::set_permissions(job.path(), fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let input = job.path().join("image.bin");
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&input)
        .map_err(|e| e.to_string())?;
    f.write_all(&disk.bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    drop(f);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args = vec![
        exe.to_string_lossy().into_owned(),
        "direct-sd-helper".into(),
        input.to_string_lossy().into_owned(),
        id.device,
        id.whole,
        id.uuid,
        id.size.to_string(),
        name.into(),
    ];
    fs::write(
        job.path().join("request.txt"),
        args.iter()
            .skip(2)
            .map(|s| format!("{s:?}\n"))
            .collect::<String>(),
    )
    .map_err(|e| e.to_string())?;
    let shell = args
        .iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "do shell script {} with administrator privileges",
        apple_quote(&shell)
    );
    // Retain input and recovery journal on failure; never silently discard them.
    let job = job.keep();
    let result = command("/usr/bin/osascript", &["-e", &script]);
    match result {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            if !text.starts_with("D65_DIRECT_OK ") {
                return Err(format!(
                    "Unexpected helper response: {text}. Export records: {}",
                    job.display()
                ));
            }
            // The helper removes its root-owned journal after complete success.
            let _ = fs::remove_dir_all(&job);
            Ok(ExportReport {
                path: folder.join(name),
                bytes: disk.bytes.len(),
                layout: Layout::Verified,
            })
        }
        Err(e) => Err(format!("{e}\nExport records retained at {}", job.display())),
    }
}

struct Partition {
    file: File,
    sectors: u64,
}
impl Device for Partition {
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn read(&mut self, s: u64) -> Result<Sector> {
        if s >= self.sectors {
            return Err("Read outside selected partition".into());
        }
        let mut b = [0; 512];
        self.file
            .seek(SeekFrom::Start(s * 512))
            .and_then(|_| self.file.read_exact(&mut b))
            .map_err(|e| e.to_string())?;
        Ok(b)
    }
    fn write(&mut self, s: u64, b: &Sector) -> Result<()> {
        if s >= self.sectors {
            return Err("Write outside selected partition".into());
        }
        self.file
            .seek(SeekFrom::Start(s * 512))
            .and_then(|_| self.file.write_all(b))
            .map_err(|e| e.to_string())
    }
    fn sync(&mut self) -> Result<()> {
        // File::sync_all uses F_FULLFSYNC, which raw disk nodes reject with
        // ENOTTY. Raw writes bypass the buffer cache; flush the device's own
        // write cache instead (sys/disk.h DKIOCSYNCHRONIZECACHE, _IO('d', 22)).
        if unsafe { libc::ioctl(self.file.as_raw_fd(), 0x20006416) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOTTY | libc::ENOTSUP)) {
            return Err(format!("Card cache flush failed: {error}"));
        }
        if unsafe { libc::fsync(self.file.as_raw_fd()) } != 0 {
            return Err(format!(
                "Card flush failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    fn read_sectors(&mut self, s: u64, bytes: &mut [u8]) -> Result<()> {
        if bytes.is_empty()
            || bytes.len() % 512 != 0
            || bytes.len() > fat_export::TRANSFER_BYTES
            || s.checked_add((bytes.len() / 512) as u64)
                .is_none_or(|end| end > self.sectors)
        {
            return Err("Transfer outside selected partition or unaligned length".into());
        }
        self.file
            .seek(SeekFrom::Start(s * 512))
            .and_then(|_| self.file.read_exact(bytes))
            .map_err(|e| e.to_string())
    }
    fn write_sectors(&mut self, s: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty()
            || bytes.len() % 512 != 0
            || bytes.len() > fat_export::TRANSFER_BYTES
            || s.checked_add((bytes.len() / 512) as u64)
                .is_none_or(|end| end > self.sectors)
        {
            return Err("Transfer outside selected partition or unaligned length".into());
        }
        self.file
            .seek(SeekFrom::Start(s * 512))
            .and_then(|_| self.file.write_all(bytes))
            .map_err(|e| e.to_string())
    }
}
fn save_journal(path: &Path, sectors: u64, boot: &Sector, patches: &[Patch]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(b"D65FATJ1")
        .and_then(|_| f.write_all(&sectors.to_le_bytes()))
        .and_then(|_| f.write_all(&(patches.len() as u64).to_le_bytes()))
        .and_then(|_| f.write_all(boot))
        .map_err(|e| e.to_string())?;
    for p in patches {
        f.write_all(&p.sector.to_le_bytes())
            .and_then(|_| f.write_all(&p.before))
            .and_then(|_| f.write_all(&p.after))
            .map_err(|e| e.to_string())?;
    }
    f.sync_all().map_err(|e| e.to_string())?;
    File::open(path.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
/// Private CLI entry point used only after the OS authorization prompt.
pub fn helper(args: &[String]) -> Result<String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("Direct SD export requires macOS administrator authorization.".into());
    }
    if args.len() != 6 {
        return Err("Invalid direct-export request.".into());
    }
    let input = Path::new(&args[0]);
    let device = &args[1];
    let whole = &args[2];
    let uuid = &args[3];
    let size = args[4]
        .parse::<u64>()
        .map_err(|_| "Invalid partition length")?;
    let name = &args[5];
    if !disk_id(device, true) || !disk_id(whole, false) {
        return Err("Invalid device identifier.".into());
    }
    fat_export::short_name(name)?;
    let expected = Identity {
        device: device.clone(),
        whole: whole.clone(),
        uuid: uuid.clone(),
        size,
    };
    let (current, mount) = inspect(device)?;
    if current != expected || mount.is_none() {
        return Err("The selected card changed or is no longer mounted; export cancelled.".into());
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(input)
        .map_err(|e| e.to_string())?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Invalid staged image.".into());
    }
    let mut data = Vec::new();
    Read::by_ref(&mut f)
        .take(5_570_561)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    let disk = Disk::parse(data)?;
    if !name.ends_with(&disk.format.ext().to_ascii_uppercase()) {
        return Err("Image extension mismatch.".into());
    }
    let journal = input.with_file_name("metadata.journal");
    if journal.exists() {
        return Err("This export already has a journal; create a new export request.".into());
    }
    command("/usr/sbin/diskutil", &["unmountDisk", whole])?;
    let operation = (|| {
        // diskutil can return before the kernel mount table and disk
        // arbitration have settled; wait briefly for the unmount to show.
        let mut settled = false;
        for _ in 0..50 {
            let (after, mount) = inspect(device)?;
            if after != expected {
                return Err("Card identity changed after unmounting; export cancelled.".into());
            }
            if mount.is_none() {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if !settled {
            return Err(
                "The card volume is still mounted after unmounting; export cancelled.".into(),
            );
        }
        let path = format!("/dev/r{device}");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_EXCL | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| {
                if e.raw_os_error() == Some(libc::EPERM) {
                    REMOVABLE_ACCESS.to_string()
                } else {
                    format!("Cannot exclusively open the unmounted card: {e}")
                }
            })?;
        if !file
            .metadata()
            .map_err(|e| e.to_string())?
            .file_type()
            .is_char_device()
        {
            return Err("Expected a raw partition device.".into());
        }
        // sys/disk.h: _IOR('d',24,uint32_t), _IOR('d',25,uint64_t).
        // Check the actual open descriptor rather than trusting a cached path size.
        let mut block_size = 0u32;
        let mut block_count = 0u64;
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x40046418, &mut block_size) } != 0
            || unsafe { libc::ioctl(file.as_raw_fd(), 0x40086419, &mut block_count) } != 0
            || block_size != 512
            || block_count.checked_mul(512) != Some(size)
        {
            return Err(
                "The raw device's size or sector size differs from the selected partition.".into(),
            );
        }
        let mut partition = Partition {
            file,
            sectors: size / 512,
        };
        let boot = partition.read(0)?;
        fat_export::export(&mut partition, name, &disk, |patches| {
            save_journal(&journal, size / 512, &boot, patches)
        })
    })();
    if operation
        .as_ref()
        .err()
        .is_some_and(|e| e.starts_with("RECOVERY REQUIRED:"))
    {
        return Err(format!(
            "{} Journal: {}. Card left unmounted.",
            operation.err().unwrap(),
            journal.display()
        ));
    }
    let remount = command("/usr/sbin/diskutil", &["mount", device]);
    match (operation, remount) {
        (Ok(r), Ok(_)) => {
            let (current, mounted) = inspect(device)?;
            if current != expected {
                return Err("Card identity changed before mounted read-back.".into());
            }
            let mounted = mounted.ok_or("Export succeeded but macOS did not remount the card.")?;
            let actual = fs::read(mounted.join(name))
                .map_err(|e| format!("Raw export verified, but mounted read-back failed: {e}"))?;
            if actual != disk.bytes {
                return Err("Raw export verified, but mounted file bytes differ. Keep export records for investigation.".into());
            }
            fs::remove_file(&journal)
                .map_err(|e| format!("Export verified; could not remove completed journal: {e}"))?;
            Ok(format!(
                "D65_DIRECT_OK {} {} {}",
                r.first_cluster, r.clusters, r.bytes
            ))
        }
        (Ok(_), Err(e)) => Err(format!(
            "Image and contiguous chain verified, but remount failed: {e}. Retain {}",
            journal.display()
        )),
        (Err(e), Ok(_)) => Err(e),
        (Err(e), Err(m)) => Err(format!("{e}; remount also failed: {m}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plist(body: &str) -> Vec<u8> {
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict>{body}</dict></plist>"#).into_bytes()
    }
    #[test]
    fn diskutil_schema_uses_whole_disk_without_mounted_or_whole_keys() {
        let partition = r#"<key>DeviceIdentifier</key><string>disk4s1</string><key>ParentWholeDisk</key><string>disk4</string><key>WholeDisk</key><false/><key>Internal</key><false/><key>FilesystemType</key><string>msdos</string><key>VolumeUUID</key><string>TEST-ID</string><key>TotalSize</key><integer>268800000</integer><key>Size</key><integer>268932096</integer>"#;
        let parent = r#"<key>DeviceIdentifier</key><string>disk4</string><key>WholeDisk</key><true/><key>Internal</key><false/><key>VirtualOrPhysical</key><string>Physical</string>"#;
        let actual = identity_from_plists(&plist(partition), &plist(parent)).unwrap();
        assert_eq!(
            actual,
            Identity {
                device: "disk4s1".into(),
                whole: "disk4".into(),
                uuid: "TEST-ID".into(),
                size: 268932096
            }
        );
        for wrong in [
            parent.replace("<key>Internal</key><false/>", "<key>Internal</key><true/>"),
            parent.replace("Physical</string>", "Virtual</string>"),
            parent.replace("disk4</string>", "disk5</string>"),
            parent.replace("WholeDisk", "Whole"),
        ] {
            assert!(identity_from_plists(&plist(partition), &plist(&wrong)).is_err());
        }
        for wrong in [
            partition.replace(
                "<key>WholeDisk</key><false/>",
                "<key>WholeDisk</key><true/>",
            ),
            partition.replace("msdos", "apfs"),
            partition.replace("<key>Internal</key><false/>", "<key>Internal</key><true/>"),
            partition.replace("WholeDisk</key><false/>", "Whole</key><false/>"),
        ] {
            assert!(identity_from_plists(&plist(&wrong), &plist(parent)).is_err());
        }
    }
    #[test]
    fn reads_live_kernel_mount_table_without_disk_management_service() {
        assert_eq!(mounted_path("disk999999999s1").unwrap(), None);
        assert!(mounted_path("/dev/disk4s1").is_err());
        if let Ok(path) = std::env::var("D65_TEST_MOUNT") {
            let file = File::open(&path).unwrap();
            let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) }, 0);
            let source = unsafe { std::ffi::CStr::from_ptr(stat.f_mntfromname.as_ptr()) }
                .to_str()
                .unwrap();
            let device = source.strip_prefix("/dev/").unwrap();
            assert_eq!(
                mounted_path(device).unwrap(),
                Some(fs::canonicalize(path).unwrap())
            );
        }
    }
    /// Full raw export through the macOS Partition adapter. Point it at the
    /// raw node of an attached scratch disk image, never at a real card:
    /// `D65_TEST_RAW_DEVICE=/dev/rdiskNs1 cargo test -- --ignored raw_device`.
    #[test]
    #[ignore]
    fn exports_through_raw_device() {
        let path = std::env::var("D65_TEST_RAW_DEVICE").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_EXCL | libc::O_NOFOLLOW)
            .open(&path)
            .unwrap();
        let mut count = 0u64;
        assert_eq!(
            unsafe { libc::ioctl(file.as_raw_fd(), 0x40086419, &mut count) },
            0
        );
        let mut partition = Partition {
            file,
            sectors: count,
        };
        partition.sync().unwrap();
        let mut disk = Disk::create(crate::disk::Format::D65, "RAW TEST", "65").unwrap();
        disk.put("HELLO", 1, b"raw export", false).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("metadata.journal");
        let boot = partition.read(0).unwrap();
        let receipt = fat_export::export(&mut partition, "RAWTEST.D65", &disk, |p| {
            save_journal(&journal, count, &boot, p)
        })
        .unwrap();
        assert_eq!(receipt.bytes, disk.bytes.len());
    }
    #[test]
    fn validates_devices_and_quotes_without_shell_expansion() {
        assert!(disk_id("disk4s1", true));
        assert!(disk_id("disk4", false));
        for bad in ["disk", "disk4s1;rm", "../disk4", "disk4s", "disk4s1s2"] {
            assert!(!disk_id(bad, true));
        }
        assert_eq!(shell_quote("a'b$(x)"), "'a'\\''b$(x)'");
        assert_eq!(apple_quote("a\\b\"c"), "\"a\\\\b\\\"c\"");
    }
    #[test]
    fn native_plist_fields_and_journal_are_roundtrippable() {
        let xml=br#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Internal</key><false/><key>TotalSize</key><integer>123456</integer><key>MountPoint</key><string>/Volumes/A &amp; B</string></dict></plist>"#;
        assert_eq!(field(xml, "Internal").unwrap(), "false");
        assert_eq!(field(xml, "TotalSize").unwrap(), "123456");
        assert_eq!(field(xml, "MountPoint").unwrap(), "/Volumes/A & B");
        assert!(field(xml, "Missing").is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.journal");
        let patch = Patch {
            sector: 32,
            before: [0x12; 512],
            after: [0x34; 512],
        };
        save_journal(&path, 10000, &[0x56; 512], &[patch]).unwrap();
        let b = fs::read(&path).unwrap();
        assert_eq!(&b[..8], b"D65FATJ1");
        assert_eq!(b.len(), 24 + 512 + 8 + 1024);
        assert_eq!(&b[24..536], &[0x56; 512]);
        assert_eq!(&b[544..1056], &[0x12; 512]);
        assert!(save_journal(&path, 10000, &[0; 512], &[]).is_err());
        assert_eq!(fs::read(path).unwrap(), b);
    }
}
