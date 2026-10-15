#![cfg(windows)]
use d65_studio::{
    disk::{Disk, Format},
    mount,
    store::Store,
};
use std::{
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

// Explicit opt-in: maps a temporary drive using only an image in a temp folder.
// No card/device is opened. Does not enable or reconfigure Windows services.
#[test]
#[ignore = "requires built-in WebClient and a permitted local drive mapping"]
fn native_webclient_roundtrip() {
    mount::available().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("webclient.d65");
    let store = Arc::new(Mutex::new(
        Store::create(
            &image,
            Disk::create(Format::D65, "WEBDAV TEST", "65").unwrap(),
        )
        .unwrap(),
    ));
    store
        .lock()
        .unwrap()
        .change(|d| d.put("ORIGINAL", 2, b"\x01\x20native Windows read", false))
        .unwrap();
    let drive = mount::free_drive().unwrap();
    let mapped_root = std::path::PathBuf::from(format!("{}\\", drive.display()));
    let worker = mount::start(store.clone(), &drive, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if worker.is_finished() {
            panic!("Mount worker: {:?}", worker.join().unwrap());
        }
        if fs::read(mapped_root.join("ORIGINAL.prg")).is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = mount::unmount(&drive);
            panic!("Timed out waiting for native WebClient mapping");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let operation = (|| -> std::io::Result<()> {
        assert_eq!(
            fs::read(mapped_root.join("ORIGINAL.prg"))?,
            b"\x01\x20native Windows read"
        );
        fs::write(
            mapped_root.join("NEW.seq"),
            b"created via Windows WebClient",
        )?;
        assert_eq!(
            fs::read(mapped_root.join("NEW.seq"))?,
            b"created via Windows WebClient"
        );
        fs::write(mapped_root.join("NEW.seq"), b"replacement")?;
        fs::rename(mapped_root.join("NEW.seq"), mapped_root.join("RENAMED.seq"))?;
        assert_eq!(fs::read(mapped_root.join("RENAMED.seq"))?, b"replacement");
        fs::remove_file(mapped_root.join("ORIGINAL.prg"))?;
        Ok(())
    })();
    mount::unmount(&drive).unwrap();
    worker.join().unwrap().unwrap();
    operation.unwrap();
    drop(store);
    let disk = Disk::parse(fs::read(&image).unwrap()).unwrap();
    disk.validate_writable().unwrap();
    let entries = disk.entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "RENAMED");
    assert_eq!(disk.read(&entries[0]).unwrap(), b"replacement");
    let before = fs::read(&image).unwrap();
    let store = Arc::new(Mutex::new(Store::open(&image).unwrap()));
    let worker = mount::start(store.clone(), &drive, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if worker.is_finished() {
            panic!("Read-only mount: {:?}", worker.join().unwrap());
        }
        if fs::read(mapped_root.join("RENAMED.seq")).is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = mount::unmount(&drive);
            panic!("Read-only mount timed out");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let write = fs::write(mapped_root.join("BLOCKED.seq"), b"must not persist");
    mount::unmount(&drive).unwrap();
    worker.join().unwrap().unwrap();
    drop(store);
    assert!(write.is_err());
    assert_eq!(fs::read(image).unwrap(), before);
}
