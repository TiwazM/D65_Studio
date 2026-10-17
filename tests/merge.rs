use mega65_disklab::disk::{Disk, Format};
fn d81(label: &str) -> Disk {
    Disk::create(Format::D81, label, "65").unwrap()
}
// Independent offset/chain reader: deliberately does not use Disk::offset/read.
fn raw_read(image: &[u8], mut t: u8, mut s: u8) -> Vec<u8> {
    let mut data = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while t != 0 {
        assert!(seen.insert((t, s)));
        let offset = (t as usize - 1) * 65536 + s as usize * 256;
        t = image[offset];
        s = image[offset + 1];
        let count = if t == 0 { s as usize - 1 } else { 254 };
        data.extend_from_slice(&image[offset + 2..offset + 2 + count]);
    }
    data
}
#[test]
fn three_full_disks_independent_roundtrip() {
    let mut sources = Vec::new();
    let mut data = Vec::new();
    for n in 0..3 {
        let mut d = d81(&format!("BANK{n}"));
        let payload = (0..802640)
            .map(|i| ((i * 17 + n * 31) % 251) as u8)
            .collect::<Vec<_>>();
        d.put(&format!("FULL{n}"), 2, &payload, false).unwrap();
        sources.push(d);
        data.push(payload);
    }
    let snapshots = sources.iter().map(|d| d.bytes.clone()).collect::<Vec<_>>();
    let mut merged = Disk::merge_d81(&sources).unwrap();
    for (n, e) in merged.entries().unwrap().iter().enumerate() {
        assert_eq!(raw_read(&merged.bytes, e.start.0, e.start.1), data[n]);
        assert!(e.start.1 >= n as u8 * 40 && e.start.1 < (n as u8 + 1) * 40);
    }
    assert_eq!(
        merged.directory_chain().unwrap(),
        vec![(40, 3), (40, 43), (40, 83)]
    );
    assert!(merged.put("NO WRITES", 2, b"x", false).is_err());
    assert_eq!(merged.convert(Format::D65).unwrap().bytes, merged.bytes);
    assert!(merged.convert(Format::D81).is_err());
    assert!(merged.validate_writable().is_err());
    for (s, before) in sources.iter().zip(snapshots) {
        assert_eq!(s.bytes, before);
    }
}
#[test]
fn multi_sector_directories_and_terminal_counts() {
    let mut sources = Vec::new();
    for n in 0..3 {
        let mut d = d81("BANK");
        for i in 0..24 {
            let len = [0, 1, 253, 254, 255, 508, 509, 10300][i % 8];
            d.put(&format!("B{n}F{i}"), 1, &vec![i as u8; len], false)
                .unwrap();
        }
        sources.push(d);
    }
    let d = Disk::merge_d81(&sources).unwrap();
    assert_eq!(d.directory_chain().unwrap().len(), 9);
    let entries = d.entries().unwrap();
    assert_eq!(entries.len(), 72);
    for (i, e) in entries.iter().enumerate() {
        let j = i % 24;
        let len = [0, 1, 253, 254, 255, 508, 509, 10300][j % 8];
        assert_eq!(raw_read(&d.bytes, e.start.0, e.start.1), vec![j as u8; len]);
    }
}
#[test]
fn reject_conflicts_corruption_types_and_out_of_bounds_bank_count() {
    let mut a = d81("A");
    a.put("SAME", 2, b"a", false).unwrap();
    let mut b = d81("B");
    b.put("SAME", 2, b"b", false).unwrap();
    assert!(Disk::merge_d81(&[a.clone(), b.clone()]).is_err());
    assert!(Disk::merge_d81(&[a.clone()]).is_err());
    assert!(Disk::merge_d81(&[a.clone(), b.clone(), d81("C"), d81("D")]).is_err());
    let e = b.entries().unwrap().remove(0);
    b.rename(&e, "OTHER").unwrap();
    b.bytes[e.offset + 2] = 0x84;
    assert!(Disk::merge_d81(&[a.clone(), b]).is_err());
    assert!(Disk::merge_d81(&[a.clone(), a.convert(Format::D65).unwrap()]).is_err());
    let mut b = d81("B");
    let p = b.offset(40, 1).unwrap();
    b.bytes[p + 16] ^= 1;
    assert!(Disk::merge_d81(&[a, b]).is_err());
}
#[test]
fn empty_source_banks_keep_directory_connected() {
    let d = Disk::merge_d81(&[d81("A"), d81("B"), d81("C")]).unwrap();
    assert!(d.entries().unwrap().is_empty());
    assert_eq!(
        d.directory_chain().unwrap(),
        vec![(40, 3), (40, 43), (40, 83)]
    );
    assert!(d.validate_writable().is_err());
}
