//! Build original test programs supplied by path into a LOAD-hook fixture.
use d65_studio::{
    disk::{Disk, Format},
    launcher,
};
use std::{fs, path::PathBuf};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let out = PathBuf::from(&args[1]);
    fs::create_dir_all(&out).unwrap();
    let mut demo = Disk::create(Format::D81, "LOAD HOOK TEST", "65").unwrap();
    demo.put("DEMO", 2, &fs::read(&args[2]).unwrap(), false)
        .unwrap();
    let mut level = Disk::create(Format::D81, "LEVEL DATA", "65").unwrap();
    level
        .put("LEVEL", 2, &[0, 0xc0, 0x11, 0x22, 0x33, 0x44], false)
        .unwrap();
    let light = launcher::build_with_compat(&[demo.clone(), level.clone()], true).unwrap();
    fs::write(out.join("HOOKLITE.D65"), &light.bytes).unwrap();
    let mut pad = Disk::create(Format::D81, "PADDING", "65").unwrap();
    pad.put("PAD", 3, &vec![0x55; 790000], false).unwrap();
    let full = launcher::build_with_compat(&[demo, pad, level], true).unwrap();
    fs::write(out.join("HOOKTEST.D65"), &full.bytes).unwrap();
    for disk in [&light, &full] {
        for e in disk.entries().unwrap() {
            println!(
                "{} T{} S{} {} bytes",
                e.name,
                e.start.0,
                e.start.1,
                disk.read(&e).unwrap().len()
            );
        }
    }
}
