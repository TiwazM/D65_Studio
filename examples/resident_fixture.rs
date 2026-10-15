//! Developer-only reproducible fixtures; no ROM or external game data bundled.
use d65_studio::{
    disk::{Disk, Format},
    launcher,
};
use std::{fs, path::PathBuf};
fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    fs::create_dir_all(&out).unwrap();
    let mut sources = Vec::new();
    for i in 0..42 {
        let mut d = Disk::create(Format::D81, "RESIDENT TEST", "65").unwrap();
        let body = format!("RESIDENT PROGRAM {i:02} OK");
        let mut b = vec![0x99, b'"'];
        b.extend(body.bytes());
        b.extend([b'"', b':', 0x80, 0]);
        let next = 0x2001 + 4 + b.len();
        let mut p = vec![1, 32];
        p.extend((next as u16).to_le_bytes());
        p.extend(10u16.to_le_bytes());
        p.extend(b);
        p.extend([0, 0]);
        p.resize(if i == 0 { launcher::MAX_PRG } else { 55_000 }, 0x55);
        d.put(&format!("GAME{i:02}"), 2, &p, false).unwrap();
        fs::write(out.join(format!("GAME{i:02}.prg")), &p).unwrap();
        sources.push(d);
    }
    sources[0].put("EMPTY", 1, &[], false).unwrap();
    sources[0]
        .put(
            "MESSAGE",
            1,
            b"THE RESIDENT DRIVER READ THIS AFTER LAUNCH",
            false,
        )
        .unwrap();
    let disk = launcher::build(&sources).unwrap();
    fs::write(out.join("RESCAP.D65"), &disk.bytes).unwrap();
    let entries = disk.entries().unwrap();
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
    println!(
        "{} files; START {} bytes; payload {} bytes",
        entries.len(),
        start.len(),
        entries
            .iter()
            .skip(1)
            .map(|e| disk.read(e).unwrap().len())
            .sum::<usize>()
    );
    for (name, program) in [
        (
            "RESDRV",
            include_bytes!("../launcher/Resident-control.prg").to_vec(),
        ),
        ("RESMAX", fs::read(out.join("GAME00.prg")).unwrap()),
    ] {
        let mut d = Disk::create(Format::D81, "RESIDENT CONTROL", "65").unwrap();
        d.put("DEMO", 2, &program, false).unwrap();
        d.put(
            "MESSAGE",
            1,
            b"THE RESIDENT DRIVER READ THIS AFTER LAUNCH",
            false,
        )
        .unwrap();
        let compiled = launcher::build(&[d]).unwrap();
        fs::write(out.join(format!("{name}.D65")), &compiled.bytes).unwrap();
    }
}
