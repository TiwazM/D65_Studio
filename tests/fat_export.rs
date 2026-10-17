use mega65_disklab::{
    disk::{Disk, Format, Result},
    fat_export::{self, Device, Sector},
};
use std::collections::BTreeMap;
const RESERVED: u64 = 32;
const FAT: u64 = 513;
const DATA: u64 = RESERVED + FAT * 2;
const CLUSTERS: u64 = 65525;
#[derive(Clone)]
struct Card {
    sectors: BTreeMap<u64, Sector>,
    writes: usize,
    fail_at: Option<usize>,
    persistent: bool,
    corrupt_at: Option<usize>,
    syncs: usize,
    fail_sync: Option<usize>,
}
impl Device for Card {
    fn sectors(&self) -> u64 {
        DATA + CLUSTERS * 8
    }
    fn read(&mut self, s: u64) -> Result<Sector> {
        assert!(s < self.sectors());
        Ok(*self.sectors.get(&s).unwrap_or(&[0; 512]))
    }
    fn write(&mut self, s: u64, b: &Sector) -> Result<()> {
        assert!(s < self.sectors());
        self.writes += 1;
        if self
            .fail_at
            .is_some_and(|n| self.writes == n || self.persistent && self.writes >= n)
        {
            return Err("injected write failure".into());
        }
        let mut b = *b;
        if self.corrupt_at == Some(self.writes) {
            b[0] ^= 1;
        }
        self.sectors.insert(s, b);
        Ok(())
    }
    fn sync(&mut self) -> Result<()> {
        self.syncs += 1;
        if self.fail_sync == Some(self.syncs) {
            Err("injected flush failure".into())
        } else {
            Ok(())
        }
    }
}
fn set16(b: &mut [u8], p: usize, n: u16) {
    b[p..p + 2].copy_from_slice(&n.to_le_bytes());
}
fn set32(b: &mut [u8], p: usize, n: u32) {
    b[p..p + 4].copy_from_slice(&n.to_le_bytes());
}
fn get32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn fat(card: &mut Card, c: u32, value: u32) {
    for base in [RESERVED, RESERVED + FAT] {
        let s = card
            .sectors
            .entry(base + c as u64 / 128)
            .or_insert([0; 512]);
        set32(s, (c % 128) as usize * 4, value);
    }
}
fn fresh() -> Card {
    let mut c = Card {
        sectors: BTreeMap::new(),
        writes: 0,
        fail_at: None,
        persistent: false,
        corrupt_at: None,
        syncs: 0,
        fail_sync: None,
    };
    let mut b = [0; 512];
    b[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"MSDOS5.0");
    set16(&mut b, 11, 512);
    b[13] = 8;
    set16(&mut b, 14, RESERVED as u16);
    b[16] = 2;
    b[21] = 0xf8;
    set32(&mut b, 32, c.sectors() as u32);
    set32(&mut b, 36, FAT as u32);
    set32(&mut b, 44, 2);
    set16(&mut b, 48, 1);
    set16(&mut b, 50, 6);
    b[64] = 0x80;
    b[66] = 0x29;
    set32(&mut b, 67, 123456);
    b[71..82].copy_from_slice(b"D65 TEST   ");
    b[82..90].copy_from_slice(b"FAT32   ");
    b[510..].copy_from_slice(&[0x55, 0xaa]);
    c.sectors.insert(0, b);
    c.sectors.insert(6, b);
    let mut info = [0; 512];
    set32(&mut info, 0, 0x41615252);
    set32(&mut info, 484, 0x61417272);
    set32(&mut info, 488, u32::MAX);
    set32(&mut info, 492, u32::MAX);
    set32(&mut info, 508, 0xaa550000);
    c.sectors.insert(1, info);
    c.sectors.insert(7, info);
    fat(&mut c, 0, 0x0ffffff8);
    fat(&mut c, 1, 0x0fffffff);
    fat(&mut c, 2, 0x0fffffff);
    c
}
fn disk() -> Disk {
    Disk::create(Format::D65, "DIRECT TEST", "65").unwrap()
}
fn add_file(c: &mut Card, slot: usize, name: &[u8; 11], cluster: u32, size: u32, attr: u8) {
    let b = c
        .sectors
        .entry(DATA + (slot / 16) as u64)
        .or_insert([0; 512]);
    let p = slot % 16 * 32;
    b[p..p + 11].copy_from_slice(name);
    b[p + 11] = attr;
    set16(b, p + 20, (cluster >> 16) as u16);
    set16(b, p + 26, cluster as u16);
    set32(b, p + 28, size);
}
#[test]
fn chooses_a_consecutive_run_and_preserves_existing_files() {
    let mut card = fresh();
    // Existing fragmented file, a directory, and scattered allocated clusters.
    fat(&mut card, 3, 7000);
    fat(&mut card, 7000, 0x0fffffff);
    fat(&mut card, 20, 0x0fffffff);
    fat(&mut card, 500, 0x0fffffff);
    add_file(&mut card, 0, b"OLD     D65", 3, 8000, 0x20);
    add_file(&mut card, 1, b"SUBDIR     ", 20, 0, 0x10);
    card.sectors.insert(DATA + 8, [0x42; 512]);
    let before = card.clone();
    let image = disk();
    let mut journal = Vec::new();
    let receipt = fat_export::export(&mut card, "NEW.D65", &image, |patches| {
        journal = patches.to_vec();
        Ok(())
    })
    .unwrap();
    assert_eq!(receipt.first_cluster, 501);
    assert_eq!(receipt.clusters, 1360);
    let mut actual = Vec::new();
    for cluster in receipt.first_cluster..receipt.first_cluster + receipt.clusters {
        for base in [RESERVED, RESERVED + FAT] {
            let sector = card.read(base + cluster as u64 / 128).unwrap();
            let link = get32(&sector, (cluster % 128) as usize * 4) & 0x0fffffff;
            assert_eq!(
                link,
                if cluster + 1 == receipt.first_cluster + receipt.clusters {
                    0x0fffffff
                } else {
                    cluster + 1
                }
            );
        }
        for n in 0..8 {
            actual.extend(card.read(DATA + (cluster as u64 - 2) * 8 + n).unwrap());
        }
    }
    assert_eq!(&actual[..image.bytes.len()], &image.bytes);
    let dir = card.read(DATA).unwrap();
    assert_eq!(&dir[64..75], b"NEW     D65");
    assert_eq!(get32(&dir, 64 + 28), 5570560);
    for (s, b) in before.sectors {
        if !journal.iter().any(|p| p.sector == s) {
            assert_eq!(card.read(s).unwrap(), b);
        }
    }
    assert_eq!(&dir[..64], &before_root(&journal)[..64]);
}
fn before_root(p: &[fat_export::Patch]) -> Sector {
    p.iter().find(|p| p.sector == DATA).unwrap().before
}

#[test]
fn preflight_failures_never_write() {
    let image = disk();
    let mut cases = Vec::new();
    let mut c = fresh();
    fat(&mut c, 1, 0x07ffffff);
    cases.push(c);
    let mut c = fresh();
    c.sectors.get_mut(&RESERVED).unwrap()[20] ^= 1;
    cases.push(c);
    let mut c = fresh();
    add_file(&mut c, 0, b"NEW     D65", 0, 0, 0x20);
    cases.push(c);
    let mut c = fresh();
    add_file(&mut c, 0, b"BROKEN  PRG", 4, 12, 0x20);
    cases.push(c); // referenced free cluster
    let mut c = fresh();
    fat(&mut c, 3, 3);
    add_file(&mut c, 0, b"CYCLE   PRG", 3, 12, 0x20);
    cases.push(c);
    let mut c = fresh();
    fat(&mut c, 3, 0x0fffffff);
    add_file(&mut c, 0, b"A       PRG", 3, 12, 0x20);
    add_file(&mut c, 1, b"B       PRG", 3, 12, 0x20);
    cases.push(c);
    let mut c = fresh();
    for n in (3..CLUSTERS as u32 + 2).step_by(100) {
        fat(&mut c, n, 0x0fffffff);
    }
    cases.push(c);
    let mut c = fresh();
    for n in 0..128 {
        add_file(&mut c, n, b"EMPTY   TXT", 0, 0, 0x20);
    }
    cases.push(c);
    let mut c = fresh();
    set16(c.sectors.get_mut(&0).unwrap(), 48, RESERVED as u16);
    cases.push(c);
    let mut c = fresh();
    c.sectors.get_mut(&0).unwrap()[13] = 16;
    cases.push(c);
    for mut c in cases {
        let before = c.sectors.clone();
        assert!(fat_export::export(&mut c, "NEW.D65", &image, |_| Ok(())).is_err());
        assert_eq!(c.writes, 0);
        assert_eq!(c.sectors, before);
    }
}
#[test]
fn journal_failure_prevents_all_writes() {
    let mut c = fresh();
    assert!(
        fat_export::export(&mut c, "NEW.D65", &disk(), |_| Err("journal failed".into())).is_err()
    );
    assert_eq!(c.writes, 0);
}
#[test]
fn interrupted_writes_restore_metadata_and_do_not_publish() {
    let image = disk();
    let mut good = fresh();
    let mut journal = Vec::new();
    fat_export::export(&mut good, "NEW.D65", &image, |p| {
        journal = p.to_vec();
        Ok(())
    })
    .unwrap();
    let steps = good.writes;
    // Every metadata write boundary plus beginning/middle/end of data writes.
    let points = (1..=3).chain([20, 5000, 10882]).chain(10883..=steps);
    for at in points {
        let mut c = fresh();
        c.fail_at = Some(at);
        let error = fat_export::export(&mut c, "NEW.D65", &image, |_| Ok(())).unwrap_err();
        assert!(error.contains("metadata restored"), "{at}: {error}");
        for p in &journal {
            assert_eq!(
                c.read(p.sector).unwrap(),
                p.before,
                "write {at}, sector {}",
                p.sector
            );
        }
    }
}
#[test]
fn corruption_is_detected_and_persistent_failures_require_recovery() {
    let image = disk();
    let mut c = fresh();
    c.corrupt_at = Some(3);
    let err = fat_export::export(&mut c, "NEW.D65", &image, |_| Ok(())).unwrap_err();
    assert!(err.contains("read-back"));
    assert_eq!(c.read(DATA).unwrap(), [0; 512]);
    let mut c = fresh();
    c.fail_at = Some(3);
    c.persistent = true;
    assert!(fat_export::export(&mut c, "NEW.D65", &image, |_| Ok(()))
        .unwrap_err()
        .starts_with("RECOVERY REQUIRED:"));
    assert_eq!(get32(&c.read(RESERVED).unwrap(), 4) & 0x08000000, 0);
}
#[test]
fn flush_failures_restore_metadata() {
    let mut good = fresh();
    let mut journal = Vec::new();
    fat_export::export(&mut good, "NEW.D65", &disk(), |p| {
        journal = p.to_vec();
        Ok(())
    })
    .unwrap();
    for at in 1..=good.syncs {
        let mut c = fresh();
        c.fail_sync = Some(at);
        assert!(fat_export::export(&mut c, "NEW.D65", &disk(), |_| Ok(()))
            .unwrap_err()
            .contains("metadata restored"));
        for p in &journal {
            assert_eq!(c.read(p.sector).unwrap(), p.before);
        }
    }
}
#[test]
fn handles_directory_sector_boundary_and_rejects_long_name_collision() {
    let mut c = fresh();
    for n in 0..15 {
        add_file(&mut c, n, b"EMPTY   TXT", 0, 0, 0x20);
    }
    // Bytes beyond a directory end may contain old entries and must stay hidden.
    c.sectors.insert(DATA + 1, [0x41; 512]);
    fat_export::export(&mut c, "NEW.D65", &disk(), |_| Ok(())).unwrap();
    assert_eq!(c.read(DATA + 1).unwrap()[0], 0);
    assert_eq!(c.read(DATA + 1).unwrap()[1], 0x41);
    let mut c = fresh();
    let b = c.sectors.entry(DATA).or_insert([0; 512]);
    b[0] = 0x41;
    b[11] = 0x0f;
    let mut chars: Vec<u16> = "NEW.D65".encode_utf16().collect();
    chars.push(0);
    chars.resize(13, 0xffff);
    for (p, ch) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
        .iter()
        .zip(chars)
    {
        set16(b, *p, ch);
    }
    add_file(&mut c, 1, b"ALIAS~1 D65", 0, 0, 0x20);
    assert!(fat_export::export(&mut c, "NEW.D65", &disk(), |_| Ok(()))
        .unwrap_err()
        .contains("long filename"));
    assert_eq!(c.writes, 0);
}

#[test]
fn missing_or_invalid_fsinfo_is_optional_and_never_repaired() {
    for absent in [0, 0xffff] {
        let mut c = fresh();
        for s in [0, 6] {
            set16(c.sectors.get_mut(&s).unwrap(), 48, absent);
        }
        let before = [c.read(1).unwrap(), c.read(7).unwrap()];
        fat_export::export(&mut c, "NEW.D65", &disk(), |_| Ok(())).unwrap();
        assert_eq!(before, [c.read(1).unwrap(), c.read(7).unwrap()]);
    }
    for invalid in [vec![1], vec![7], vec![1, 7]] {
        let mut c = fresh();
        for &s in &invalid {
            c.sectors.insert(s, [0xa5; 512]);
        }
        let mut patches = Vec::new();
        fat_export::export(&mut c, "NEW.D65", &disk(), |p| {
            patches = p.to_vec();
            Ok(())
        })
        .unwrap();
        for s in invalid {
            assert_eq!(c.read(s).unwrap(), [0xa5; 512]);
            assert!(!patches.iter().any(|p| p.sector == s));
        }
    }
    // Bad hints must never override occupied clusters in the actual FAT.
    let mut c = fresh();
    set32(c.sectors.get_mut(&1).unwrap(), 488, CLUSTERS as u32);
    set32(c.sectors.get_mut(&1).unwrap(), 492, 3);
    fat(&mut c, 3, 0x0fffffff);
    let r = fat_export::export(&mut c, "NEW.D65", &disk(), |_| Ok(())).unwrap();
    assert_eq!(r.first_cluster, 4);
}

fn add_in(c: &mut Card, dir: u32, slot: usize, name: &[u8; 11], first: u32, size: u32, attr: u8) {
    let mut temp = fresh();
    add_file(&mut temp, 0, name, first, size, attr);
    let e = temp.read(DATA).unwrap();
    let sector = DATA + (dir as u64 - 2) * 8 + slot as u64 / 16;
    let offset = slot % 16 * 32;
    c.sectors.entry(sector).or_insert([0; 512])[offset..offset + 32].copy_from_slice(&e[..32]);
}
fn nested_card() -> Card {
    let mut c = fresh();
    fat(&mut c, 20, 0x0fffffff);
    fat(&mut c, 21, 0x0fffffff);
    add_in(&mut c, 2, 0, b"GAMES      ", 20, 0, 0x10);
    add_in(&mut c, 20, 0, b".          ", 20, 0, 0x10);
    add_in(&mut c, 20, 1, b"..         ", 0, 0, 0x10);
    add_in(&mut c, 20, 2, b"DEMOS      ", 21, 0, 0x10);
    add_in(&mut c, 21, 0, b".          ", 21, 0, 0x10);
    add_in(&mut c, 21, 1, b"..         ", 20, 0, 0x10);
    c
}
#[test]
fn exports_to_nested_existing_folders_and_keeps_other_directories() {
    let mut c = nested_card();
    add_in(&mut c, 2, 1, b"NEW     D65", 0, 0, 0x20);
    let original_root = c.read(DATA).unwrap();
    let original_parent = c.read(DATA + 18 * 8).unwrap();
    fat_export::export_to(
        &mut c,
        &["games".into(), "Demos".into()],
        "NEW.D65",
        &disk(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(c.read(DATA).unwrap(), original_root);
    assert_eq!(c.read(DATA + 18 * 8).unwrap(), original_parent);
    assert_eq!(&c.read(DATA + 19 * 8).unwrap()[64..75], b"NEW     D65");
    let writes = c.writes;
    assert!(fat_export::export_to(
        &mut c,
        &["GAMES".into(), "DEMOS".into()],
        "NEW.D65",
        &disk(),
        |_| Ok(())
    )
    .unwrap_err()
    .contains("already exists"));
    assert_eq!(c.writes, writes);
}
#[test]
fn existing_fragmented_subdirectory_chain_is_used_but_never_expanded() {
    let mut c = nested_card();
    // Fill the last folder's first cluster. No end slot means a safe refusal.
    for slot in 2..128 {
        add_in(&mut c, 21, slot, b"EMPTY   TXT", 0, 0, 0x20);
    }
    let folders = ["GAMES".into(), "DEMOS".into()];
    assert!(
        fat_export::export_to(&mut c, &folders, "NEW.D65", &disk(), |_| Ok(()))
            .unwrap_err()
            .contains("no unused end slot")
    );
    assert_eq!(c.writes, 0);
    // Add an already allocated, nonadjacent second directory cluster.
    fat(&mut c, 21, 30);
    fat(&mut c, 30, 0x0fffffff);
    let original_first = (0..8)
        .map(|i| c.read(DATA + 19 * 8 + i).unwrap())
        .collect::<Vec<_>>();
    fat_export::export_to(&mut c, &folders, "NEW.D65", &disk(), |_| Ok(())).unwrap();
    assert_eq!(&c.read(DATA + 28 * 8).unwrap()[..11], b"NEW     D65");
    for (i, before) in original_first.into_iter().enumerate() {
        assert_eq!(c.read(DATA + 19 * 8 + i as u64).unwrap(), before);
    }
    assert_eq!(get32(&c.read(RESERVED).unwrap(), 21 * 4), 30);
    assert_eq!(get32(&c.read(RESERVED).unwrap(), 30 * 4), 0x0fffffff);
}
#[test]
fn folder_resolution_refuses_missing_ambiguous_file_and_corrupt_chains() {
    for folders in [
        vec!["MISSING".into()],
        vec!["..".into()],
        vec!["GAMES".into(), ".".into()],
    ] {
        let mut c = nested_card();
        assert!(fat_export::export_to(&mut c, &folders, "NEW.D65", &disk(), |_| Ok(())).is_err());
        assert_eq!(c.writes, 0);
    }
    let mut cases = Vec::new();
    let mut c = nested_card();
    add_in(&mut c, 2, 1, b"GAMES      ", 21, 0, 0x10);
    cases.push(c);
    let mut c = nested_card();
    c.sectors.get_mut(&DATA).unwrap()[11] = 0x20;
    cases.push(c);
    let mut c = nested_card();
    fat(&mut c, 20, 20);
    cases.push(c);
    let mut c = nested_card();
    add_in(&mut c, 2, 1, b"BROKEN  PRG", 200, 10, 0x20);
    cases.push(c);
    for mut c in cases {
        assert!(
            fat_export::export_to(&mut c, &["GAMES".into()], "NEW.D65", &disk(), |_| Ok(()))
                .is_err()
        );
        assert_eq!(c.writes, 0);
    }
}
#[test]
fn resolves_valid_long_folder_names_and_refuses_bad_lfn_checksums() {
    let mut c = fresh();
    fat(&mut c, 20, 0x0fffffff);
    let alias = b"MYGREA~1   ";
    let name = "My Great Games";
    let checksum = alias
        .iter()
        .fold(0u8, |sum, b| sum.rotate_right(1).wrapping_add(*b));
    let mut chars: Vec<u16> = name.encode_utf16().collect();
    chars.push(0);
    chars.resize(26, 0xffff);
    for (slot, ordinal) in [(0, 2usize), (1, 1)] {
        let b = c.sectors.entry(DATA).or_insert([0; 512]);
        let e = &mut b[slot * 32..slot * 32 + 32];
        e[0] = ordinal as u8 | if slot == 0 { 0x40 } else { 0 };
        e[11] = 0x0f;
        e[13] = checksum;
        for (p, ch) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
            .into_iter()
            .zip(&chars[(ordinal - 1) * 13..ordinal * 13])
        {
            set16(e, p, *ch);
        }
    }
    add_in(&mut c, 2, 2, alias, 20, 0, 0x10);
    let mut bad = c.clone();
    bad.sectors.get_mut(&DATA).unwrap()[13] ^= 1;
    assert!(
        fat_export::export_to(&mut bad, &[name.into()], "NEW.D65", &disk(), |_| Ok(())).is_err()
    );
    assert_eq!(bad.writes, 0);
    fat_export::export_to(&mut c, &[name.into()], "NEW.D65", &disk(), |_| Ok(())).unwrap();
    assert_eq!(&c.read(DATA + 18 * 8).unwrap()[..11], b"NEW     D65");
}
#[test]
fn subfolder_failures_restore_the_original_directory_and_fats() {
    let mut good = nested_card();
    let mut patches = Vec::new();
    fat_export::export_to(
        &mut good,
        &["GAMES".into(), "DEMOS".into()],
        "NEW.D65",
        &disk(),
        |p| {
            patches = p.to_vec();
            Ok(())
        },
    )
    .unwrap();
    for at in (1..=3).chain([5000, 10882]).chain(10883..=good.writes) {
        let mut c = nested_card();
        c.fail_at = Some(at);
        let error = fat_export::export_to(
            &mut c,
            &["GAMES".into(), "DEMOS".into()],
            "NEW.D65",
            &disk(),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("metadata restored"), "{at}: {error}");
        for p in &patches {
            assert_eq!(c.read(p.sector).unwrap(), p.before);
        }
    }
}
#[test]
fn bulk_io_reduces_requests_and_preserves_every_byte() {
    struct Bulk {
        c: Card,
        reads: usize,
        writes: usize,
    }
    impl Device for Bulk {
        fn sectors(&self) -> u64 {
            self.c.sectors()
        }
        fn read(&mut self, s: u64) -> Result<Sector> {
            self.c.read(s)
        }
        fn write(&mut self, s: u64, b: &Sector) -> Result<()> {
            self.c.write(s, b)
        }
        fn sync(&mut self) -> Result<()> {
            self.c.sync()
        }
        fn read_sectors(&mut self, s: u64, b: &mut [u8]) -> Result<()> {
            self.reads += 1;
            for (i, chunk) in b.chunks_exact_mut(512).enumerate() {
                chunk.copy_from_slice(&self.c.read(s + i as u64)?);
            }
            Ok(())
        }
        fn write_sectors(&mut self, s: u64, b: &[u8]) -> Result<()> {
            self.writes += 1;
            for (i, chunk) in b.chunks_exact(512).enumerate() {
                self.c.write(s + i as u64, chunk.try_into().unwrap())?;
            }
            Ok(())
        }
    }
    let mut serial = fresh();
    let mut bulk = Bulk {
        c: fresh(),
        reads: 0,
        writes: 0,
    };
    fat_export::export(&mut serial, "NEW.D65", &disk(), |_| Ok(())).unwrap();
    fat_export::export(&mut bulk, "NEW.D65", &disk(), |_| Ok(())).unwrap();
    assert_eq!(serial.sectors, bulk.c.sectors);
    assert_eq!(bulk.writes, 22);
    assert_eq!(bulk.reads, 27);
    println!(
        "D65 data: 10880 sector writes -> {} bulk writes; identical final media",
        bulk.writes
    );
}

#[test]
fn writes_a_sparse_fixture_for_external_filesystem_check() {
    // Optional development fixture. Never names a device or card path.
    let Ok(path) = std::env::var("D65_FAT_FIXTURE") else {
        return;
    };
    assert!(path.ends_with(".img"));
    let c = fresh();
    let sectors = c.sectors();
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    f.set_len(sectors * 512).unwrap();
    for (s, b) in c.sectors {
        f.seek(SeekFrom::Start(s * 512)).unwrap();
        f.write_all(&b).unwrap();
    }
    f.sync_all().unwrap();
    struct RealFile(std::fs::File, u64);
    impl Device for RealFile {
        fn sectors(&self) -> u64 {
            self.1
        }
        fn read(&mut self, s: u64) -> Result<Sector> {
            let mut b = [0; 512];
            self.0
                .seek(SeekFrom::Start(s * 512))
                .and_then(|_| self.0.read_exact(&mut b))
                .map_err(|e| e.to_string())?;
            Ok(b)
        }
        fn write(&mut self, s: u64, b: &Sector) -> Result<()> {
            self.0
                .seek(SeekFrom::Start(s * 512))
                .and_then(|_| self.0.write_all(b))
                .map_err(|e| e.to_string())
        }
        fn sync(&mut self) -> Result<()> {
            self.0.sync_all().map_err(|e| e.to_string())
        }
        fn read_sectors(&mut self, s: u64, bytes: &mut [u8]) -> Result<()> {
            self.0
                .seek(SeekFrom::Start(s * 512))
                .and_then(|_| self.0.read_exact(bytes))
                .map_err(|e| e.to_string())
        }
        fn write_sectors(&mut self, s: u64, bytes: &[u8]) -> Result<()> {
            self.0
                .seek(SeekFrom::Start(s * 512))
                .and_then(|_| self.0.write_all(bytes))
                .map_err(|e| e.to_string())
        }
    }
    let mut device = RealFile(f, sectors);
    let one = fat_export::export(&mut device, "NEW.D65", &disk(), |_| Ok(())).unwrap();
    let two = fat_export::export(
        &mut device,
        "CHECK.D81",
        &Disk::create(Format::D81, "CONTROL", "65").unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(two.first_cluster, one.first_cluster + one.clusters);
}
