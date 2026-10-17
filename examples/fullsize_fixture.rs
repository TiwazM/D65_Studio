//! Developer-only full-capacity fixture: 95 menu programs plus MESSAGE, filling
//! the D65 to its 5.46 MB payload limit so the last entries sit on tracks 81-85
//! and logical sectors above 127. No ROM or third-party data is bundled.
use mega65_disklab::{
    disk::{Disk, Format},
    launcher,
};
use std::{fs, path::PathBuf};
fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    fs::create_dir_all(&out).unwrap();
    let mut sources = Vec::new();
    for i in 0..95 {
        let mut d = Disk::create(Format::D81, "FULL SIZE TEST", "65").unwrap();
        let body = format!("RESIDENT PROGRAM {i:02} OK (FULL-SIZE IMAGE)");
        let mut b = vec![0x99, b'"'];
        b.extend(body.bytes());
        b.extend([b'"', b':', 0x80, 0]);
        let next = 0x2001 + 4 + b.len();
        let mut p = vec![1, 32];
        p.extend((next as u16).to_le_bytes());
        p.extend(10u16.to_le_bytes());
        p.extend(b);
        p.extend([0, 0]);
        p.resize(if i == 94 { launcher::MAX_PRG } else { 55_000 }, 0x55);
        d.put(&format!("GAME{i:02}"), 2, &p, false).unwrap();
        fs::write(out.join(format!("GAME{i:02}.prg")), &p).unwrap();
        sources.push(d);
    }
    // Pad with data files so the image is as full as the builder allows.
    let mut filler = Disk::create(Format::D81, "FILLER", "65").unwrap();
    filler
        .put("FILLER", 1, &vec![0x37; 198_000], false)
        .unwrap();
    filler
        .put(
            "MESSAGE",
            1,
            b"THE RESIDENT DRIVER READ THIS FROM THE END OF A FULL D65",
            false,
        )
        .unwrap();
    sources.push(filler);
    let disk = launcher::build(&sources).unwrap();
    fs::write(out.join("FULLSIZE.D65"), &disk.bytes).unwrap();
    let entries = disk.entries().unwrap();
    for e in entries.iter().skip(1) {
        if e.kind == 1 {
            fs::write(out.join(format!("{}.prg", e.name)), disk.read(e).unwrap()).unwrap();
        }
    }
    // Same layout as resident_fixture: START.prg, catalog.bin and files.txt
    // ("index name track sector"), so the CPU harness scripts can reuse it.
    let start = disk.read(&entries[0]).unwrap();
    fs::write(out.join("START.prg"), &start).unwrap();
    let mut data = Vec::new();
    let mut p = 2;
    while start[p..p + 2] != [0, 0] {
        let n = u16::from_le_bytes([start[p], start[p + 1]]) as usize - 0x2001 + 2;
        if start[p + 4] == 0x83 {
            for n in std::str::from_utf8(&start[p + 5..n - 1])
                .unwrap()
                .split(',')
            {
                data.push(n.parse::<u8>().unwrap());
            }
        }
        p = n;
    }
    let reader = include_bytes!("../launcher/reader.bin");
    assert_eq!(&data[..reader.len()], reader);
    fs::write(out.join("catalog.bin"), &data[reader.len()..]).unwrap();
    let mut listing = String::new();
    for (i, e) in entries.iter().skip(1).enumerate() {
        listing.push_str(&format!("{i} {} {} {}\n", e.name, e.start.0, e.start.1));
    }
    fs::write(out.join("files.txt"), listing).unwrap();
    let (used, total) = disk.resident_space().unwrap();
    println!(
        "{used} of {total} blocks used ({} payload bytes)",
        used * 254
    );
    for e in &entries {
        let chain = disk.chain(e).unwrap();
        let (lt, ls) = *chain.last().unwrap();
        println!(
            "{:16} start T{}/S{} end T{}/S{} blocks {}",
            e.name,
            e.start.0,
            e.start.1,
            lt,
            ls,
            chain.len()
        );
    }
}
