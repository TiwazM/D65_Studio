use mega65_disklab::{
    disk::{Disk, Format, D65_SIZE},
    store::Store,
};
fn disk() -> Disk {
    Disk::create(Format::D65, "TEST DISK", "65").unwrap()
}
#[test]
fn geometry_and_bam() {
    let d = disk();
    assert_eq!(d.bytes.len(), D65_SIZE);
    assert_eq!(d.offset(40, 0).unwrap(), 39 * 65536);
    assert_eq!(d.free_blocks(), 3160);
    assert_eq!(
        &d.bytes[d.offset(40, 1).unwrap() + 250..][..6],
        &[36, 0xf0, 255, 255, 255, 255]
    );
    d.validate_writable().unwrap();
}
#[test]
fn block_boundaries_roundtrip() {
    for n in [0, 1, 2, 253, 254, 255, 508, 509, 65536, 802640] {
        let mut d = disk();
        let data = (0..n).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        d.put("PAYLOAD", 2, &data, false).unwrap();
        let reopened = Disk::parse(d.bytes).unwrap();
        let e = &reopened.entries().unwrap()[0];
        assert_eq!(reopened.read(e).unwrap(), data);
        reopened.validate_writable().unwrap();
    }
}
#[test]
fn disk_full_is_transactional() {
    let mut d = disk();
    d.put("FULL", 2, &vec![42; 802640], false).unwrap();
    let before = d.bytes.clone();
    assert!(d.put("EXTRA", 2, b"X", false).is_err());
    assert_eq!(d.bytes, before);
    assert_eq!(d.free_blocks(), 0);
}
#[test]
fn failed_replacement_keeps_original() {
    let mut d = disk();
    d.put("KEEP", 2, b"ORIGINAL", false).unwrap();
    let before = d.bytes.clone();
    assert!(d.put("KEEP", 2, &vec![0; 802641], true).is_err());
    assert_eq!(d.bytes, before);
}
#[test]
fn grows_directory_and_preserves_links() {
    let mut d = disk();
    for n in 0..296 {
        d.put(&format!("FILE{n:03}"), 1, b"abc", false).unwrap();
    }
    assert_eq!(d.directory_chain().unwrap().len(), 37);
    let before = d.bytes.clone();
    assert!(d.put("TOO MANY", 2, b"", false).is_err());
    assert_eq!(before, d.bytes);
    for e in d.entries().unwrap() {
        assert_eq!(d.read(&e).unwrap(), b"abc");
    }
    d.validate_writable().unwrap();
}
#[test]
fn rename_delete_and_reuse() {
    let mut d = disk();
    d.put("ORIGINAL", 1, b"hello", false).unwrap();
    let e = d.entries().unwrap().remove(0);
    d.rename(&e, "RENAMED").unwrap();
    let e = d.entries().unwrap().remove(0);
    assert_eq!(e.name, "RENAMED");
    d.remove(&e).unwrap();
    assert!(d.entries().unwrap().is_empty());
    assert_eq!(d.free_blocks(), 3160);
    d.put("NEW", 3, b"new", false).unwrap();
}
#[test]
fn reject_duplicate_and_invalid_names() {
    let mut d = disk();
    d.put("ONE", 1, b"", false).unwrap();
    assert!(d.put("one", 1, b"", false).is_err());
    for name in ["", "OVER SIXTEEN CHARACTERS", "A/B", "A*B", "ä", "X%41"] {
        assert!(d.put(name, 1, b"", false).is_err());
    }
}
#[test]
fn full_image_conversion_is_lossless() {
    let mut d = disk();
    for i in 0..15 {
        d.put(&format!("FILE{i}"), 2, &vec![i; 1000], false)
            .unwrap();
    }
    let d81 = d.convert(Format::D81).unwrap();
    assert_eq!(d81.bytes.len(), 819200);
    assert_eq!(d81.convert(Format::D65).unwrap().bytes, d.bytes);
}
#[test]
fn conversion_does_not_discard_unknown_data() {
    let mut d = disk();
    let o = d.offset(83, 55).unwrap();
    d.bytes[o] = 0x77;
    assert!(d.convert(Format::D81).is_err());
    d.put("FILE", 2, b"abc", false).unwrap();
    assert_eq!(d.bytes[o], 0x77);
}
#[test]
fn cycle_detection() {
    let mut d = disk();
    d.put("LOOP", 2, b"abc", false).unwrap();
    let e = d.entries().unwrap().remove(0);
    let o = d.offset(e.start.0, e.start.1).unwrap();
    d.bytes[o] = e.start.0;
    d.bytes[o + 1] = e.start.1;
    assert!(d.read(&e).is_err());
    assert!(d.validate_writable().is_err());
}
#[test]
fn invalid_terminal_count_rejected() {
    let mut d = disk();
    d.put("FILE", 2, b"abc", false).unwrap();
    let e = d.entries().unwrap().remove(0);
    let o = d.offset(e.start.0, e.start.1).unwrap();
    d.bytes[o + 1] = 0;
    assert!(d.read(&e).is_err());
}
#[test]
fn directory_cycle_detection() {
    let mut d = disk();
    let o = d.offset(40, 3).unwrap();
    d.bytes[o] = 40;
    d.bytes[o + 1] = 3;
    assert!(Disk::parse(d.bytes).is_err());
}
#[test]
fn crosslinks_block_edits() {
    let mut d = disk();
    d.put("ONE", 2, b"1", false).unwrap();
    d.put("TWO", 2, b"2", false).unwrap();
    let es = d.entries().unwrap();
    d.bytes[es[1].offset + 3] = es[0].start.0;
    d.bytes[es[1].offset + 4] = es[0].start.1;
    assert!(d.validate_writable().is_err());
}
#[test]
fn unsupported_types_block_edits() {
    let mut d = disk();
    d.put("RELFILE", 2, b"1", false).unwrap();
    let e = d.entries().unwrap().remove(0);
    d.bytes[e.offset + 2] = 0x84;
    assert!(d.validate_writable().is_err());
    assert!(d.read(&d.entries().unwrap()[0]).is_err());
}
#[test]
fn bam_corruption_blocks_edits() {
    let mut d = disk();
    let o = d.offset(40, 1).unwrap();
    d.bytes[o + 16] = 39;
    assert!(d.validate_writable().is_err());
}
#[test]
fn extended_chains_are_readable_but_not_writable() {
    let mut d = disk();
    d.put("HIGH", 2, b"test", false).unwrap();
    let e = d.entries().unwrap().remove(0);
    let orig = d.offset(e.start.0, e.start.1).unwrap();
    let target = d.offset(1, 80).unwrap();
    let b = d.bytes[orig..orig + 256].to_vec();
    d.bytes[target..target + 256].copy_from_slice(&b);
    d.bytes[e.offset + 3] = 1;
    d.bytes[e.offset + 4] = 80;
    assert_eq!(d.read(&d.entries().unwrap()[0]).unwrap(), b"test");
    assert!(d.validate_writable().is_err());
}
#[test]
fn malformed_size_never_guessed() {
    for n in [0, 10, 2785280, 6553600, 819201] {
        assert!(Disk::parse(vec![0; n]).is_err());
    }
}
#[test]
fn lock_backup_external_change() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.d65");
    let original = disk();
    let mut s = Store::create(&path, original.clone()).unwrap();
    assert!(Store::open(&path).is_err());
    s.change(|d| d.put("A", 2, b"abc", false)).unwrap();
    assert_eq!(
        std::fs::read(s.backup.as_ref().unwrap()).unwrap(),
        original.bytes
    );
    let mut outside = std::fs::read(&path).unwrap();
    outside[123] = 42;
    std::fs::write(&path, &outside).unwrap();
    assert!(s.change(|d| d.put("B", 2, b"def", false)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), outside);
}
#[test]
fn new_never_overwrites() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("keep.d65");
    std::fs::write(&path, b"keep me").unwrap();
    assert!(Store::create(&path, disk()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"keep me");
}
#[test]
fn independent_freezer_fixture() {
    let reference = include_bytes!("fixtures/freezer-reference.d65");
    assert_eq!(disk().bytes.as_slice(), reference.as_slice());
}

#[test]
fn host_files_map_to_names_and_types() {
    use mega65_disklab::disk::import_name;
    use std::path::Path;
    assert_eq!(
        import_name(Path::new("/x/game.prg")).unwrap(),
        ("GAME".into(), 2)
    );
    assert_eq!(
        import_name(Path::new("notes.seq")).unwrap(),
        ("NOTES".into(), 1)
    );
    assert_eq!(
        import_name(Path::new("data.usr")).unwrap(),
        ("DATA".into(), 3)
    );
    assert_eq!(
        import_name(Path::new("SPACE.SCP")).unwrap(),
        ("SPACE.SCP".into(), 2)
    );
    assert_eq!(
        import_name(Path::new("readme.txt")).unwrap(),
        ("README.TXT".into(), 1)
    );
    assert_eq!(
        import_name(Path::new("space_commander_level_data.bin")).unwrap(),
        ("SPACE_COMMANDER_".into(), 2)
    );
    assert_eq!(
        import_name(Path::new("a,b\"c\"é.dat")).unwrap(),
        ("A-B-C-\u{2d}.DAT".replace("\u{2d}", "-").into(), 2)
    );
    assert!(import_name(Path::new("game.d81")).is_err());
    assert!(import_name(Path::new("....")).is_ok());
}

#[test]
fn disk_images_import_all_of_their_files() {
    use mega65_disklab::disk::{import_files, Disk, Format};
    let dir = tempfile::tempdir().unwrap();
    let mut d81 = Disk::create(Format::D81, "SRC", "65").unwrap();
    d81.put(
        "GAME",
        2,
        &[1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0],
        false,
    )
    .unwrap();
    d81.put("LEVELS", 1, &vec![7; 70_000], false).unwrap();
    let p = dir.path().join("src.d81");
    std::fs::write(&p, &d81.bytes).unwrap();
    let files = import_files(&p).unwrap();
    assert_eq!(
        files
            .iter()
            .map(|(n, k, d)| (n.as_str(), *k, d.len()))
            .collect::<Vec<_>>(),
        [("GAME", 2, 12), ("LEVELS", 1, 70_000)]
    );
    let resident = mega65_disklab::launcher::build(&[d81]).unwrap();
    let p = dir.path().join("comp.d65");
    std::fs::write(&p, &resident.bytes).unwrap();
    assert_eq!(
        import_files(&p)
            .unwrap()
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["GAME", "LEVELS"]
    );
    let empty = dir.path().join("empty.d81");
    std::fs::write(
        &empty,
        Disk::create(Format::D81, "EMPTY", "65").unwrap().bytes,
    )
    .unwrap();
    assert!(import_files(&empty).unwrap_err().contains("no PRG"));
    // A REL entry is reported, not silently dropped.
    let mut rel = Disk::parse(std::fs::read(dir.path().join("src.d81")).unwrap()).unwrap();
    let e = rel.entries().unwrap()[0].clone();
    rel.bytes[e.offset + 2] = 0x84;
    let p = dir.path().join("rel.d81");
    std::fs::write(&p, &rel.bytes).unwrap();
    let r = mega65_disklab::disk::import_from(&p).unwrap();
    assert_eq!(r.files.len(), 1);
    assert_eq!(r.skipped, ["GAME (REL)"]);
    let prg = dir.path().join("demo.prg");
    std::fs::write(&prg, [1, 8]).unwrap();
    assert_eq!(
        import_files(&prg).unwrap(),
        vec![("DEMO".to_string(), 2, vec![1, 8])]
    );
    let mut target = mega65_disklab::launcher::create_editable("BIG", "65").unwrap();
    for (n, k, d) in import_files(&dir.path().join("src.d81")).unwrap() {
        target.put(&n, k, &d, false).unwrap();
    }
    assert_eq!(target.entries().unwrap().len(), 3);
}

#[test]
fn lock_file_disappears_when_the_image_is_closed() {
    use mega65_disklab::{
        disk::{Disk, Format},
        store::Store,
    };
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.d81");
    let s = Store::create(&p, Disk::create(Format::D81, "A", "65").unwrap()).unwrap();
    let lock = dir.path().join("a.d81.lock");
    assert!(lock.exists());
    assert!(
        Store::open(&p).is_err(),
        "second open must be refused while held"
    );
    drop(s);
    assert!(!lock.exists());
    let s = Store::open(&p).unwrap();
    assert!(lock.exists());
    drop(s);
    assert!(!lock.exists());
}
