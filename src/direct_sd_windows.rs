//! Windows volume boundary for the shared, journaled contiguous FAT32 writer.
//! Only removable USB/SD/MMC media and a single partition are accepted.
use crate::{
    disk::{Disk, Result},
    fat_export::{self, Device, Patch, Sector},
    sd_export::{ExportReport, Layout},
    windows::{error, wide},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{Ioctl::*, IO::DeviceIoControl},
};

fn ioctl(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize> {
    let mut used = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            input.as_ptr().cast(),
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut used,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(error(&format!("Device control 0x{code:x}")))
    } else {
        Ok(used as usize)
    }
}
fn query<T: Copy>(f: &File, code: u32) -> Result<T> {
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(value.as_mut_ptr().cast(), std::mem::size_of::<T>())
    };
    if ioctl(f, code, &[], bytes)? < bytes.len() {
        return Err("Incomplete device identity response".into());
    }
    Ok(unsafe { value.assume_init() })
}
fn open_volume(guid: &str, write: bool) -> Result<File> {
    if !valid_guid(guid) {
        return Err("Invalid volume GUID".into());
    }
    OpenOptions::new()
        .access_mode(if write {
            GENERIC_READ | GENERIC_WRITE
        } else {
            0
        })
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_WRITE_THROUGH)
        .open(guid.trim_end_matches('\\'))
        .map_err(|e| {
            format!(
                "Cannot open selected volume{}: {e}",
                if write {
                    " for administrator access"
                } else {
                    ""
                }
            )
        })
}
fn valid_guid(s: &str) -> bool {
    s.strip_prefix("\\\\?\\Volume{")
        .and_then(|s| s.strip_suffix("}\\"))
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
}
fn root(path: &Path) -> Result<String> {
    let s = path.to_str().ok_or("Invalid destination path")?;
    let b = s.as_bytes();
    if b.len() != 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || !matches!(b[2], b'\\' | b'/')
    {
        return Err("Cannot identify a drive-letter volume root such as E:\\. Network paths and mounted folders are refused.".into());
    }
    Ok(format!("{}:\\", (b[0] as char).to_ascii_uppercase()))
}
fn folder_parts(path: &Path) -> Result<(String, Vec<String>)> {
    let value = path
        .to_str()
        .ok_or("Invalid destination path")?
        .replace('/', "\\");
    let value = value.strip_prefix("\\\\?\\").unwrap_or(&value);
    if value.len() < 3
        || !value.as_bytes()[0].is_ascii_alphabetic()
        || &value.as_bytes()[1..3] != b":\\"
    {
        return Err("Choose an existing folder on a drive-letter SD card, such as E:\\Games. Network and device paths are refused.".into());
    }
    let root = root(Path::new(&value[..3]))?;
    let tail = value[3..].trim_end_matches('\\');
    let folders: Vec<String> = if tail.is_empty() {
        Vec::new()
    } else {
        tail.split('\\').map(str::to_owned).collect()
    };
    if folders.len() > 64
        || folders.iter().any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || s.ends_with(['.', ' '])
                || s.chars()
                    .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        })
    {
        return Err("Invalid SD-card folder path".into());
    }
    Ok((root, folders))
}
fn check_folders(root: &str, folders: &[String]) -> Result<()> {
    let mut path = Path::new(root).to_path_buf();
    for part in folders {
        path.push(part);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| format!("Choose an existing destination folder: {e}"))?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("Destination must be an existing SD-card directory. Junctions and symbolic links are refused.".into());
        }
    }
    Ok(())
}
fn volume_guid(root: &str) -> Result<String> {
    let mut guid = [0u16; 64];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(wide(root).as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
    } == 0
    {
        return Err(error("Cannot identify volume"));
    }
    Ok(String::from_utf16_lossy(
        &guid[..guid.iter().position(|c| *c == 0).unwrap_or(guid.len())],
    ))
}
fn filesystem(root: &str) -> Result<u32> {
    let mut name = [0u16; 64];
    let mut serial = 0;
    if unsafe {
        GetVolumeInformationW(
            wide(root).as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } == 0
    {
        return Err(error("Cannot read volume information"));
    }
    if String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
        != "FAT32"
    {
        return Err("Select an existing FAT32 card. No formatting is performed.".into());
    }
    Ok(serial)
}
fn refuse_system_disk(id: &Identity) -> Result<()> {
    let mut windows_dir = [0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(
            windows_dir.as_mut_ptr(),
            windows_dir.len() as u32,
        )
    };
    if length == 0 || length as usize >= windows_dir.len() {
        return Err(error("Cannot locate the Windows system volume"));
    }
    let mut mount = [0u16; 32768];
    if unsafe { GetVolumePathNameW(windows_dir.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) }
        == 0
    {
        return Err(error("Cannot locate the Windows system mount"));
    }
    let mount = String::from_utf16_lossy(
        &mount[..mount
            .iter()
            .position(|c| *c == 0)
            .ok_or("Invalid system mount")?],
    );
    let system = open_volume(&volume_guid(&mount)?, false)?;
    let extents: VOLUME_DISK_EXTENTS = query(&system, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS)?;
    if extents.NumberOfDiskExtents != 1 || extents.Extents[0].DiskNumber == id.disk {
        return Err("The Windows system disk (including its other partitions) is refused.".into());
    }
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    disk: u32,
    partition: u32,
    offset: i64,
    size: i64,
    descriptor: Vec<u8>,
}
impl Identity {
    fn token(&self, guid: &str, serial: u32) -> String {
        format!(
            "{guid}|{serial}|{}|{}|{}|{}|{}",
            self.disk,
            self.partition,
            self.offset,
            self.size,
            self.descriptor
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
}
fn inspect(f: &File) -> Result<Identity> {
    let number: STORAGE_DEVICE_NUMBER = query(f, IOCTL_STORAGE_GET_DEVICE_NUMBER)?;
    let extents: VOLUME_DISK_EXTENTS = query(f, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS)?;
    let geometry: DISK_GEOMETRY = query(f, IOCTL_DISK_GET_DRIVE_GEOMETRY)?;
    // StorageDeviceProperty, PropertyStandardQuery, no additional parameters.
    let mut descriptor = vec![0u8; 4096];
    let used = ioctl(f, IOCTL_STORAGE_QUERY_PROPERTY, &[0; 12], &mut descriptor)?;
    descriptor.truncate(used);
    identity_from_info(number, extents, geometry, descriptor)
}
fn identity_from_info(
    number: STORAGE_DEVICE_NUMBER,
    extents: VOLUME_DISK_EXTENTS,
    geometry: DISK_GEOMETRY,
    mut descriptor: Vec<u8>,
) -> Result<Identity> {
    let used = descriptor.len();
    if used < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return Err("Missing storage device descriptor".into());
    }
    let d = unsafe {
        std::ptr::read_unaligned(descriptor.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>())
    };
    if d.Size as usize > used
        || d.Size < 36
        || !d.RemovableMedia
        || ![BusTypeUsb, BusTypeSd, BusTypeMmc].contains(&d.BusType)
        || d.DeviceType != 0
    {
        return Err("Only removable physical USB/SD/MMC cards are supported. Internal, fixed, virtual and unknown devices are refused.".into());
    }
    descriptor.truncate(d.Size as usize);
    let e = extents.Extents[0];
    if extents.NumberOfDiskExtents != 1
        || number.DeviceType != 7 // FILE_DEVICE_DISK
        || number.PartitionNumber == 0
        || number.PartitionNumber == u32::MAX
        || number.DeviceNumber != e.DiskNumber
        || e.StartingOffset <= 0
        || e.StartingOffset % 512 != 0
        || e.ExtentLength <= 0
        || e.ExtentLength % 512 != 0
        || geometry.BytesPerSector != 512
    {
        return Err("Expected one physical partition with 512-byte sectors; whole disks and spanned volumes are refused.".into());
    }
    Ok(Identity {
        disk: e.DiskNumber,
        partition: number.PartitionNumber,
        offset: e.StartingOffset,
        size: e.ExtentLength,
        descriptor,
    })
}

pub fn export(disk: &Disk, destination: &Path) -> Result<ExportReport> {
    export_with_progress(disk, destination, |_| {})
}
pub fn export_with_progress(
    disk: &Disk,
    destination: &Path,
    mut report: impl FnMut(String),
) -> Result<ExportReport> {
    report("Checking the destination and card identity".into());
    let name = destination
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid filename")?;
    fat_export::short_name(name)?;
    let (root, folders) = folder_parts(destination.parent().ok_or("Choose a folder on the card")?)?;
    check_folders(&root, &folders)?;
    if destination.exists() {
        return Err("That filename already exists. Choose a NEW filename.".into());
    }
    let serial = filesystem(&root)?;
    let guid = volume_guid(&root)?;
    let id = inspect(&open_volume(&guid, false)?)?;
    refuse_system_disk(&id)?;
    let job = tempfile::Builder::new()
        .prefix("d65-windows-export-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let input = job.path().join("image.bin");
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&input)
        .map_err(|e| e.to_string())?;
    f.write_all(&disk.bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    drop(f);
    // Keep the staged bytes immutable across the UAC boundary.
    let _guard = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&input)
        .map_err(|e| e.to_string())?;
    let progress = crate::windows::ExportProgress::create()?;
    report(progress.message());
    crate::windows::elevated(
        &[
            "direct-sd-helper".into(),
            input.to_string_lossy().into_owned(),
            root.clone(),
            guid.clone(),
            id.token(&guid, serial),
            name.into(),
            folders.join("/"),
            progress.name.clone(),
        ],
        || report(progress.message()),
    )?;
    report("Final mounted-file verification".into());
    if volume_guid(&root)? != guid
        || filesystem(&root)? != serial
        || inspect(&open_volume(&guid, false)?)? != id
    {
        return Err("Card identity changed before final verification.".into());
    }
    if fs::read(destination).map_err(|e| format!("Mounted read-back failed: {e}"))? != disk.bytes {
        return Err("Mounted read-back differs from source. Do not use the export.".into());
    }
    Ok(ExportReport {
        path: destination.to_owned(),
        bytes: disk.bytes.len(),
        layout: Layout::Verified,
    })
}

struct Partition {
    file: File,
    sectors: u64,
    buffer: Box<AlignedTransfer>,
    progress: Option<crate::windows::ExportProgress>,
}
// Windows volume I/O is noncached. Sector-sized transfers also need aligned
// buffers; a byte array alone does not provide the device alignment contract.
#[repr(align(4096))]
struct AlignedTransfer([u8; fat_export::TRANSFER_BYTES]);
impl Device for Partition {
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn read(&mut self, s: u64) -> Result<Sector> {
        let mut b = [0; 512];
        self.read_sectors(s, &mut b)?;
        Ok(b)
    }
    fn write(&mut self, s: u64, b: &Sector) -> Result<()> {
        self.write_sectors(s, b)
    }
    fn read_sectors(&mut self, s: u64, bytes: &mut [u8]) -> Result<()> {
        transfer_bounds(s, bytes.len(), self.sectors)?;
        self.file
            .seek(SeekFrom::Start(s * 512))
            .map_err(|e| e.to_string())?;
        self.file
            .read_exact(&mut self.buffer.0[..bytes.len()])
            .map_err(|e| e.to_string())?;
        bytes.copy_from_slice(&self.buffer.0[..bytes.len()]);
        Ok(())
    }
    fn write_sectors(&mut self, s: u64, bytes: &[u8]) -> Result<()> {
        transfer_bounds(s, bytes.len(), self.sectors)?;
        self.buffer.0[..bytes.len()].copy_from_slice(bytes);
        self.file
            .seek(SeekFrom::Start(s * 512))
            .and_then(|_| self.file.write_all(&self.buffer.0[..bytes.len()]))
            .map_err(|e| e.to_string())
    }
    fn sync(&mut self) -> Result<()> {
        self.file.sync_all().map_err(|e| e.to_string())
    }
    fn progress(&mut self, phase: fat_export::Phase, done: u64, total: u64) {
        if let Some(p) = &self.progress {
            p.set(phase as u32, done, total);
        }
    }
}
fn transfer_bounds(s: u64, len: usize, sectors: u64) -> Result<()> {
    if len == 0
        || len % 512 != 0
        || len > fat_export::TRANSFER_BYTES
        || s.checked_add((len / 512) as u64)
            .is_none_or(|end| end > sectors)
    {
        return Err("Unaligned transfer or transfer outside selected partition".into());
    }
    Ok(())
}
fn save_journal(path: &Path, sectors: u64, boot: &Sector, patches: &[Patch]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_WRITE_THROUGH)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(536 + patches.len() * 1032);
    bytes.extend_from_slice(b"D65FATJ1");
    bytes.extend_from_slice(&sectors.to_le_bytes());
    bytes.extend_from_slice(&(patches.len() as u64).to_le_bytes());
    bytes.extend_from_slice(boot);
    for p in patches {
        bytes.extend_from_slice(&p.sector.to_le_bytes());
        bytes.extend_from_slice(&p.before);
        bytes.extend_from_slice(&p.after);
    }
    f.write_all(&bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

pub fn helper(args: &[String]) -> Result<String> {
    if args.len() != 7 {
        return Err("Invalid direct-export request".into());
    }
    let input = Path::new(&args[0]);
    // Alternate-administrator UAC sessions may not be able to open the caller's
    // progress map. Missing progress must never relax disk validation or abort
    // an otherwise safe export; the GUI retains its elapsed-time indicator.
    let progress = crate::windows::ExportProgress::open(&args[6]).ok();
    let root = root(Path::new(&args[1]))?;
    let guid = &args[2];
    let expected = &args[3];
    let name = &args[4];
    let (parsed_root, folders) = folder_parts(&Path::new(&root).join(&args[5]))?;
    if parsed_root != root {
        return Err("Destination folder changed volumes".into());
    }
    check_folders(&root, &folders)?;
    fat_export::short_name(name)?;
    if !valid_guid(guid) || volume_guid(&root)? != *guid {
        return Err("Selected card changed before authorization".into());
    }
    let serial = filesystem(&root)?;
    let file = open_volume(guid, true)?;
    let id = inspect(&file)?;
    refuse_system_disk(&id)?;
    if id.token(guid, serial) != *expected {
        return Err("Selected card identity changed; export cancelled".into());
    }
    let mut source = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(input)
        .map_err(|e| e.to_string())?;
    if !source.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Staged image is not a file".into());
    }
    let mut data = Vec::new();
    Read::by_ref(&mut source)
        .take(5_570_561)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    let disk = Disk::parse(data)?;
    if !name.ends_with(&format!(".{}", disk.format.ext().to_ascii_uppercase())) {
        return Err("Image extension mismatch".into());
    }
    // A private, fresh helper-owned directory, never an output path supplied by
    // the unprivileged process. Retain all records until mounted verification.
    let records = tempfile::Builder::new()
        .prefix("d65-recovery-")
        .tempdir()
        .map_err(|e| e.to_string())?
        .keep();
    let journal = records.join("metadata.journal");
    let mut identity_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_WRITE_THROUGH)
        .open(records.join("identity.txt"))
        .map_err(|e| e.to_string())?;
    identity_file
        .write_all(format!("{expected}\n{}/{name}\n", folders.join("/")).as_bytes())
        .and_then(|_| identity_file.sync_all())
        .map_err(|e| e.to_string())?;
    // Lock MUST succeed before dismount. Do not force past open files.
    ioctl(&file, FSCTL_LOCK_VOLUME, &[], &mut []).map_err(|e| {
        format!(
            "Close Explorer windows and other card users, then retry. Exclusive lock failed: {e}"
        )
    })?;
    let mut partition = Partition {
        file,
        sectors: id.size as u64 / 512,
        buffer: Box::new(AlignedTransfer([0; fat_export::TRANSFER_BYTES])),
        progress,
    };
    let operation = (|| {
        ioctl(&partition.file, FSCTL_DISMOUNT_VOLUME, &[], &mut [])?;
        if inspect(&partition.file)? != id {
            return Err("Card changed after exclusive lock".into());
        }
        let length: GET_LENGTH_INFORMATION = query(&partition.file, IOCTL_DISK_GET_LENGTH_INFO)?;
        if length.Length != id.size {
            return Err("Open volume length changed".into());
        }
        let boot = partition.read(0)?;
        if u32::from_le_bytes(boot[67..71].try_into().unwrap()) != serial {
            return Err("FAT volume serial changed after locking".into());
        }
        fat_export::export_to(&mut partition, &folders, name, &disk, |patches| {
            save_journal(&journal, id.size as u64 / 512, &boot, patches)
        })
    })();
    if operation
        .as_ref()
        .err()
        .is_some_and(|e| e.starts_with("RECOVERY REQUIRED:"))
    {
        let offline = ioctl(&partition.file, IOCTL_VOLUME_OFFLINE, &[], &mut []);
        let message = format!(
            "{}\nRecovery records: {}\n{}",
            operation.unwrap_err(),
            records.display(),
            if offline.is_ok() {
                "Volume taken offline. Do not bring it online before manual recovery."
            } else {
                "Could not take the volume offline. The exclusive lock is being held. Remove the card BEFORE dismissing this message; do not use it before manual recovery."
            }
        );
        // If offline is unsupported, retain exclusive access through the warning.
        crate::windows::show_error(&message);
        drop(partition);
        return Err(message);
    }
    // Closing the sole locking handle releases the lock; the next filesystem
    // access remounts the dismounted FAT volume and sees fresh metadata.
    if let Some(p) = &partition.progress {
        p.set(8, 0, 0);
    }
    drop(partition);
    let result: Result<String> = (|| {
        let report = operation?;
        if volume_guid(&root)? != *guid
            || filesystem(&root)? != serial
            || inspect(&open_volume(guid, false)?)? != id
        {
            return Err("Card changed before mounted read-back".into());
        }
        if fs::read(
            folders
                .iter()
                .fold(Path::new(&root).to_path_buf(), |p, s| p.join(s))
                .join(name),
        )
        .map_err(|e| format!("Raw export verified; mounted read-back failed: {e}"))?
            != disk.bytes
        {
            return Err("Raw export verified; mounted read-back differs".into());
        }
        Ok(format!(
            "D65_DIRECT_OK {} {} {}",
            report.first_cluster, report.clusters, report.bytes
        ))
    })();
    match result {
        Ok(message) => {
            let _ = fs::remove_dir_all(&records);
            Ok(message)
        }
        Err(e) => Err(format!(
            "{e}\nExport records retained at {}",
            records.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_identity_rejects_unsafe_media() {
        let number = STORAGE_DEVICE_NUMBER {
            DeviceType: 7,
            DeviceNumber: 3,
            PartitionNumber: 1,
        };
        let extents = VOLUME_DISK_EXTENTS {
            NumberOfDiskExtents: 1,
            Extents: [DISK_EXTENT {
                DiskNumber: 3,
                StartingOffset: 1_048_576,
                ExtentLength: 268_932_096,
            }],
        };
        let mut geometry: DISK_GEOMETRY = unsafe { std::mem::zeroed() };
        geometry.BytesPerSector = 512;
        let mut descriptor: STORAGE_DEVICE_DESCRIPTOR = unsafe { std::mem::zeroed() };
        descriptor.Size = std::mem::size_of_val(&descriptor) as u32;
        descriptor.RemovableMedia = true;
        descriptor.BusType = BusTypeUsb;
        let bytes = |d: &STORAGE_DEVICE_DESCRIPTOR| unsafe {
            std::slice::from_raw_parts(
                (d as *const STORAGE_DEVICE_DESCRIPTOR).cast::<u8>(),
                std::mem::size_of_val(d),
            )
            .to_vec()
        };
        let good = identity_from_info(number, extents, geometry, bytes(&descriptor)).unwrap();
        assert_eq!(good.size, 268_932_096);
        let token = good.token("volume", 123);
        assert_ne!(token, good.token("other volume", 123));
        assert_ne!(token, good.token("volume", 124));
        for bus in [
            BusTypeVirtual,
            BusTypeFileBackedVirtual,
            BusTypeSata,
            BusTypeUnknown,
        ] {
            let mut d = descriptor;
            d.BusType = bus;
            assert!(identity_from_info(number, extents, geometry, bytes(&d)).is_err());
        }
        let mut d = descriptor;
        d.RemovableMedia = false;
        assert!(identity_from_info(number, extents, geometry, bytes(&d)).is_err());
        let mut g = geometry;
        g.BytesPerSector = 4096;
        assert!(identity_from_info(number, extents, g, bytes(&descriptor)).is_err());
        let mut n = number;
        n.PartitionNumber = 0;
        assert!(identity_from_info(n, extents, geometry, bytes(&descriptor)).is_err());
        let mut e = extents;
        e.NumberOfDiskExtents = 2;
        assert!(identity_from_info(number, e, geometry, bytes(&descriptor)).is_err());
        assert!(identity_from_info(number, extents, geometry, vec![0; 4]).is_err());
    }
    #[test]
    fn volume_root_cannot_select_whole_disks_or_subdirectories() {
        for s in [
            "C:",
            "C:\\folder",
            "\\\\.\\PhysicalDrive0",
            "\\\\server\\share",
            "\\\\?\\C:\\",
        ] {
            assert!(root(Path::new(s)).is_err());
        }
        assert_eq!(root(Path::new("e:\\")).unwrap(), "E:\\");
        assert!(!valid_guid("\\\\.\\PhysicalDrive0"));
        assert!(valid_guid(
            "\\\\?\\Volume{12345678-1234-1234-1234-123456789abc}\\"
        ));
    }
    #[test]
    fn journal_is_durable_new_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("metadata.journal");
        save_journal(
            &p,
            100,
            &[0x56; 512],
            &[Patch {
                sector: 12,
                before: [0x12; 512],
                after: [0x34; 512],
            }],
        )
        .unwrap();
        let bytes = fs::read(&p).unwrap();
        assert_eq!(&bytes[..8], b"D65FATJ1");
        assert_eq!(bytes.len(), 1568);
        assert!(save_journal(&p, 100, &[0; 512], &[]).is_err());
        assert_eq!(bytes, fs::read(p).unwrap());
    }

    #[test]
    fn destination_accepts_existing_folder_syntax_only() {
        for s in [
            "e:\\Games\\My Demos",
            "\\\\?\\e:\\Games\\My Demos\\",
            "e:/Games/My Demos",
        ] {
            assert_eq!(
                folder_parts(Path::new(s)).unwrap(),
                ("E:\\".into(), vec!["Games".into(), "My Demos".into()])
            );
        }
        assert_eq!(
            folder_parts(Path::new("e:\\")).unwrap(),
            ("E:\\".into(), vec![])
        );
        for s in [
            "E:Games",
            "E:\\..\\Games",
            "E:\\Games\\.\\Demos",
            "E:\\Games\\\\Demos",
            "E:\\Games.",
            "E:\\Games ",
            "E:\\Games:stream",
            "\\\\server\\share",
            "\\\\.\\PhysicalDrive1",
            "\\\\?\\Volume{12345678-1234-1234-1234-123456789abc}\\",
        ] {
            assert!(folder_parts(Path::new(s)).is_err(), "{s}");
        }
    }

    fn test_partition(path: &Path, sectors: u64) -> Partition {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_WRITE_THROUGH)
            .open(path)
            .unwrap();
        file.set_len(sectors * 512).unwrap();
        Partition {
            file,
            sectors,
            buffer: Box::new(AlignedTransfer([0; fat_export::TRANSFER_BYTES])),
            progress: None,
        }
    }

    #[test]
    fn native_batched_file_io_checks_bounds_and_preserves_neighbors() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = test_partition(&dir.path().join("ordinary-file.img"), 1024);
        assert_eq!(p.buffer.0.as_ptr() as usize % 4096, 0);
        let bytes: Vec<_> = (0..fat_export::TRANSFER_BYTES)
            .map(|i| (i % 251) as u8)
            .collect();
        p.write(0, &[0x12; 512]).unwrap();
        p.write(513, &[0x34; 512]).unwrap();
        p.write_sectors(1, &bytes).unwrap();
        p.sync().unwrap();
        let mut got = vec![0; bytes.len()];
        p.read_sectors(1, &mut got).unwrap();
        assert_eq!(got, bytes);
        assert_eq!(p.read(0).unwrap(), [0x12; 512]);
        assert_eq!(p.read(513).unwrap(), [0x34; 512]);
        for (sector, length) in [
            (0, 0),
            (0, 513),
            (0, fat_export::TRANSFER_BYTES + 512),
            (1024, 512),
            (u64::MAX, 512),
        ] {
            assert!(p.write_sectors(sector, &vec![0; length]).is_err());
            assert!(p.read_sectors(sector, &mut vec![0; length]).is_err());
        }
        assert_eq!(p.read(0).unwrap(), [0x12; 512]);
        // A short device/file read is an error, never an apparently valid buffer.
        p.file.set_len(512).unwrap();
        assert!(p.read_sectors(0, &mut got).is_err());
    }

    #[test]
    #[ignore = "timing experiment using regular temporary files only"]
    fn native_file_transfer_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = crate::disk::Disk::create(crate::disk::Format::D65, "BENCH", "65")
            .unwrap()
            .bytes;
        for size in [512, fat_export::TRANSFER_BYTES] {
            let mut p = test_partition(
                &dir.path().join(format!("transfer-{size}.img")),
                (bytes.len() / 512) as u64,
            );
            let start = std::time::Instant::now();
            for (n, chunk) in bytes.chunks(size).enumerate() {
                p.write_sectors((n * size / 512) as u64, chunk).unwrap();
            }
            p.sync().unwrap();
            let written = start.elapsed();
            let mut buffer = vec![0; size];
            for (n, chunk) in bytes.chunks(size).enumerate() {
                p.read_sectors((n * size / 512) as u64, &mut buffer[..chunk.len()])
                    .unwrap();
                assert_eq!(&buffer[..chunk.len()], chunk);
            }
            println!("Regular local file, FILE_FLAG_WRITE_THROUGH, {} bytes: chunk={size}, writes={}, write+flush={:.3}s, write+flush+verified-read={:.3}s", bytes.len(), bytes.len().div_ceil(size), written.as_secs_f64(), start.elapsed().as_secs_f64());
        }
    }
}
