//! Experimental direct-controller launcher. No runtime assembler required.
use crate::disk::{Disk, Format, Result};
const COMPAT_READER: &[u8] = include_bytes!("../launcher/reader-compat.bin");
const COMPAT_GATEWAY: &[u8] = include_bytes!("../launcher/compat-gateway.bin");
const COMPAT_LOAD: &[u8] = include_bytes!("../launcher/compat-load.bin");
const READER: &[u8] = include_bytes!("../launcher/reader.bin");
pub const MAX_PRG: usize = 0xd701; // address bytes + $2001..$f6ff
const CATALOG_ENTRY: usize = 24;
const MAILBOX: usize = 0x1e00;

fn line(out: &mut Vec<u8>, number: u16, body: &[u8]) {
    let next = 0x2001 + out.len() + body.len() + 5;
    out.extend_from_slice(&(next as u16).to_le_bytes());
    out.extend_from_slice(&number.to_le_bytes());
    out.extend_from_slice(body);
    out.push(0);
}
// Only the handful of BASIC keywords used by our generated launcher.
fn token(s: &str) -> Vec<u8> {
    let words: &[(&str, &[u8])] = &[
        ("RESTORE", &[0x8c]),
        ("PEEK", &[0xc2]),
        ("SYS", &[0x9e]),
        ("PRINT", &[0x99]),
        ("INPUT", &[0x85]),
        ("THEN", &[0xa7]),
        ("GOTO", &[0x89]),
        ("POKE", &[0x97]),
        ("NEXT", &[0x82]),
        ("READ", &[0x87]),
        ("DATA", &[0x83]),
        ("BANK", &[0xfe, 0x02]),
        ("MEM", &[0xfe, 0x23]),
        ("FOR", &[0x81]),
        ("END", &[0x80]),
        ("IF", &[0x8b]),
        ("TO", &[0xa4]),
        ("=", &[0xb2]),
        ("+", &[0xaa]),
        ("-", &[0xab]),
    ];
    let mut out = Vec::new();
    let mut pos = 0;
    let mut quoted = false;
    while pos < s.len() {
        let b = s.as_bytes()[pos];
        if b == b'"' {
            quoted = !quoted;
        }
        if !quoted {
            if let Some((w, bytes)) = words.iter().find(|(w, _)| s[pos..].starts_with(w)) {
                out.extend_from_slice(bytes);
                pos += w.len();
                continue;
            }
        }
        out.push(b);
        pos += 1;
    }
    out
}
fn valid_basic(prg: &[u8]) -> bool {
    if prg.len() < 8 || prg.len() > MAX_PRG || prg[..2] != [1, 0x20] {
        return false;
    }
    let mut p = 2;
    loop {
        if p + 2 > prg.len() {
            return false;
        }
        let next = u16::from_le_bytes([prg[p], prg[p + 1]]) as usize;
        if next == 0 {
            return p > 2;
        }
        let Some(q) = next.checked_sub(0x2001).and_then(|x| x.checked_add(2)) else {
            return false;
        };
        if q <= p + 4 || q + 2 > prg.len() || prg[q - 1] != 0 {
            return false;
        }
        p = q;
    }
}

/// Build a read-only compilation within the tested logical sectors 0..119.
/// START alone uses ordinary DOS allocation; payload chains can cross banks.
pub fn build(sources: &[Disk]) -> Result<Disk> {
    build_with_compat(sources, false)
}

/// Optional ROM-920413 LOAD hook. Disabled by default.
pub fn build_with_compat(sources: &[Disk], compatibility: bool) -> Result<Disk> {
    if sources.is_empty() {
        return Err("Choose at least one D81 image.".into());
    }
    let mut files = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut menu = Vec::new();
    for (source_index, source) in sources.iter().enumerate() {
        if source.format != Format::D81 {
            return Err("Launcher sources must be D81 images.".into());
        }
        source
            .validate_writable()
            .map_err(|e| format!("D81 {}: {e}", source_index + 1))?;
        for entry in source.entries()? {
            if entry.raw_name[..5] == *b"START" && entry.raw_name[5..].iter().all(|b| *b == 0xa0) {
                return Err("START is reserved for the launcher; rename that source file.".into());
            }
            if !names.insert(entry.raw_name) {
                return Err(format!(
                    "Duplicate filename {} in D81 {}. Rename one copy before combining images.",
                    entry.name,
                    source_index + 1
                ));
            }
            let data = source.read(&entry)?;
            if entry.kind == 2 && valid_basic(&data) {
                menu.push((entry.name.clone(), files.len()));
            }
            files.push((entry, data));
            if files.len() > 295 {
                return Err("Too many files: at most 295 files plus START.".into());
            }
        }
    }
    if menu.is_empty() || menu.len() > 99 {
        return Err("The menu needs 1–99 MEGA65 BASIC-entry PRGs at $2001, at most 55,041 bytes each. Other files are retained for the resident reader.".into());
    }
    let mut catalog = vec![0; 8 + CATALOG_ENTRY * files.len()];
    catalog[..5].copy_from_slice(b"D65R\x01");
    catalog[5..7].copy_from_slice(&(files.len() as u16).to_le_bytes());
    // DATA values use three decimal digits, so substituting final addresses
    // cannot change START's length or allocation (no layout fixed-point guess).
    let placeholder = make_start(&menu, &catalog, compatibility)?;
    let mut disk = Disk::create(Format::D65, "D65 RESIDENT", "65")?;
    disk.put("START", 2, &placeholder, false)?;
    let start = disk.entries()?.remove(0);
    let start_chain = disk.chain(&start)?;
    let occupied: std::collections::HashSet<_> = start_chain.iter().copied().collect();
    let mut free = Vec::new();
    for bank in 0..3 {
        for t in 1..=80 {
            if t == 40 {
                continue;
            }
            for s in bank * 40..bank * 40 + 40 {
                if !occupied.contains(&(t, s)) {
                    free.push((t, s));
                }
            }
        }
    }
    let needed: usize = files
        .iter()
        .map(|(_, d)| d.len().div_ceil(254).max(1))
        .sum();
    if needed > free.len() {
        return Err(format!("These files exceed the launch disk capacity: {} blocks needed, {} available after START (about 2.4 MB before the menu).", needed, free.len()));
    }
    let mut slots = vec![disk.bytes[start.offset + 2..start.offset + 32].to_vec()];
    let mut cursor = 0;
    for (index, (entry, data)) in files.iter().enumerate() {
        let blocks = data.len().div_ceil(254).max(1);
        let chain = &free[cursor..cursor + blocks];
        cursor += blocks;
        write_chain(&mut disk, chain, data)?;
        let mut slot = vec![0; 30];
        slot[0] = entry.flags;
        slot[1] = chain[0].0;
        slot[2] = chain[0].1;
        slot[3..19].copy_from_slice(&entry.raw_name);
        slot[28..30].copy_from_slice(&(blocks as u16).to_le_bytes());
        slots.push(slot);
        let c = &mut catalog[8 + index * CATALOG_ENTRY..8 + (index + 1) * CATALOG_ENTRY];
        c[..16].copy_from_slice(&entry.raw_name);
        c[16..18].copy_from_slice(&[chain[0].0, chain[0].1]);
        // Files larger than the current whole-file reader window remain
        // extractable. $FFFF is an unsupported-length sentinel, never a wrap.
        c[18..20].copy_from_slice(&(u16::try_from(data.len()).unwrap_or(u16::MAX)).to_le_bytes());
        let checksum = data.iter().fold(0u16, |sum, b| sum.wrapping_add(*b as u16));
        c[20..22].copy_from_slice(&checksum.to_le_bytes());
        c[22] = entry.kind;
        c[23] = entry.flags;
    }
    let starter = make_start(&menu, &catalog, compatibility)?;
    if starter.len() != placeholder.len() {
        return Err("Internal START layout mismatch.".into());
    }
    write_chain(&mut disk, &start_chain, &starter)?;
    let directory_blocks = slots.len().div_ceil(8);
    for i in 0..directory_blocks {
        let p = disk.offset(40, 3 + i as u8)?;
        disk.bytes[p..p + 256].fill(0);
        disk.bytes[p..p + 2].copy_from_slice(&if i + 1 == directory_blocks {
            [0, 255]
        } else {
            [40, 4 + i as u8]
        });
        for j in 0..8 {
            if let Some(slot) = slots.get(i * 8 + j) {
                disk.bytes[p + j * 32 + 2..p + j * 32 + 32].copy_from_slice(slot);
            }
        }
    }
    // A D81 BAM cannot describe this allocator. Mark standard space unavailable
    // to ordinary DOS writes, and identify this read-only layout explicitly.
    for bs in 1..=2 {
        let p = disk.offset(40, bs)? + 16;
        disk.bytes[p..p + 240].fill(0);
    }
    let h = disk.offset(40, 0)?;
    disk.bytes[h + 48..h + 56].copy_from_slice(b"D65RES01");
    disk.bytes[h + 56..h + 58].copy_from_slice(&(needed as u16).to_le_bytes());
    disk.bytes[h + 58..h + 60].copy_from_slice(&(start_chain.len() as u16).to_le_bytes());
    disk.bytes[h + 60] = u8::from(compatibility);
    let entries = disk.entries()?;
    if entries.len() != files.len() + 1 || disk.read(&entries[0])? != starter {
        return Err("Launch directory verification failed.".into());
    }
    let mut used = std::collections::HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        for ts in disk.chain(entry)? {
            if ts.0 == 40 || ts.1 >= 120 || !used.insert(ts) {
                return Err("Launch allocation verification failed.".into());
            }
        }
        if index > 0 {
            let (original, bytes) = &files[index - 1];
            if entry.raw_name != original.raw_name
                || entry.flags != original.flags
                || disk.read(entry)? != *bytes
            {
                return Err(format!("Import verification failed for {}.", original.name));
            }
        }
    }
    Ok(disk)
}

fn write_chain(disk: &mut Disk, chain: &[(u8, u8)], data: &[u8]) -> Result<()> {
    if chain.len() != data.len().div_ceil(254).max(1) {
        return Err("Internal allocation length mismatch.".into());
    }
    for (i, &(t, s)) in chain.iter().enumerate() {
        let begin = (i * 254).min(data.len());
        let end = (begin + 254).min(data.len());
        let p = disk.offset(t, s)?;
        disk.bytes[p..p + 256].fill(0);
        disk.bytes[p..p + 2].copy_from_slice(&if i + 1 < chain.len() {
            [chain[i + 1].0, chain[i + 1].1]
        } else {
            [0, (end - begin + 1) as u8]
        });
        disk.bytes[p + 2..p + 2 + end - begin].copy_from_slice(&data[begin..end]);
    }
    Ok(())
}

fn make_start(menu: &[(String, usize)], catalog: &[u8], compatibility: bool) -> Result<Vec<u8>> {
    let reader = if compatibility { COMPAT_READER } else { READER };
    let mut basic = Vec::new();
    line(&mut basic, 10, &token("BANK0"));
    line(
        &mut basic,
        20,
        &token(&format!(
            "FORI=0TO{}:READA:POKE5632+I,A:NEXT",
            reader.len() - 1
        )),
    );
    line(
        &mut basic,
        25,
        &token(&format!(
            "FORI=0TO{}:READA:POKE142598144+I,A:NEXT",
            catalog.len() - 1
        )),
    );
    if compatibility {
        line(
            &mut basic,
            26,
            &token(&format!(
                "FORI=0TO{}:READA:POKE7232+I,A:NEXT",
                COMPAT_GATEWAY.len() - 1
            )),
        );
        line(
            &mut basic,
            27,
            &token(&format!(
                "FORI=0TO{}:READA:POKE{}+I,A:NEXT",
                COMPAT_LOAD.len() - 1,
                0x87ee000u32
            )),
        );
    }
    line(
        &mut basic,
        30,
        &token("PRINT\"D65 STUDIO - RESIDENT READER\""),
    );
    line(
        &mut basic,
        40,
        &token("PRINT\"DRIVE 8 / RESIDENT READER / READ-ONLY\""),
    );
    // Initialize mailbox: default image mode, no stale caller fields.
    line(&mut basic, 50, &token("FORI=7680TO7807:POKEI,0:NEXT"));
    if compatibility {
        line(&mut basic, 51, &token("SYS7235:IFPEEK(7821)=0THENGOTO60"));
        line(
            &mut basic,
            52,
            &token("PRINT\"LOAD HOOK INSTALL FAILED\";PEEK(7817)"),
        );
        line(&mut basic, 53, &token("END"));
        line(
            &mut basic,
            60,
            &token("PRINT\"EXPERIMENTAL LOAD HOOK ACTIVE\""),
        );
    }
    for (i, (name, _)) in menu.iter().enumerate() {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || " -_().".contains(c) {
                    c
                } else {
                    '?'
                }
            })
            .collect();
        let row = 100 + i as u16 * 3;
        line(
            &mut basic,
            row,
            &token(&format!("PRINT\"{}  {}\"", i + 1, safe)),
        );
        if (i + 1) % 16 == 0 || i + 1 == menu.len() {
            let next = if i + 1 == menu.len() { 100 } else { row + 3 };
            line(
                &mut basic,
                row + 1,
                &token(&format!(
                    "INPUT\"NUMBER (0 NEXT PAGE, -1 QUIT)\";N:IFN=0THEN{next}"
                )),
            );
            line(&mut basic, row + 2, &token("GOTO400"));
        }
    }
    line(&mut basic, 400, &token("IFN=-1THENEND"));
    for i in 0..menu.len() {
        line(
            &mut basic,
            401 + i as u16,
            &token(&format!("IFN={}THEN{}", i + 1, 1000 + i)),
        );
    }
    line(&mut basic, 600, &token("GOTO100"));
    for (i, (_, index)) in menu.iter().enumerate() {
        line(
            &mut basic,
            1000 + i as u16,
            &token(&format!(
                "POKE{},0:POKE{},{}:POKE{},{}:GOTO2000",
                MAILBOX + 14,
                MAILBOX + 18,
                index & 255,
                MAILBOX + 19,
                index >> 8
            )),
        );
    }
    line(&mut basic, 2000, &token("PRINT\"LOADING...\""));
    let keys = b"SYS5632\r";
    let mut text = keys
        .iter()
        .enumerate()
        .map(|(i, b)| format!("POKE{},{}:", 0x2b0 + i, b))
        .collect::<String>();
    text.push_str("POKE208,8:END");
    line(&mut basic, 2010, &token(&text));
    let mut data: Vec<u8> = reader.iter().chain(catalog).copied().collect();
    if compatibility {
        data.extend_from_slice(COMPAT_GATEWAY);
        data.extend_from_slice(COMPAT_LOAD);
    }
    for (i, chunk) in data.chunks(16).enumerate() {
        line(
            &mut basic,
            3000 + i as u16,
            &token(&format!(
                "DATA{}",
                chunk
                    .iter()
                    .map(|b| format!("{b:03}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )),
        );
    }
    basic.extend_from_slice(&[0, 0]);
    if basic.len() > MAX_PRG - 2 {
        return Err("Launcher menu is too large.".into());
    }
    let mut prg = vec![1, 0x20];
    prg.extend(basic);
    Ok(prg)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_fits_reserved_ram() {
        assert!(!READER.is_empty() && READER.len() > 512 && READER.len() <= 0x700);
    }
    #[test]
    fn compatibility_is_optional_and_fits_reserved_memory() {
        assert!(COMPAT_READER.len() <= 0x640);
        assert!(COMPAT_GATEWAY.len() <= 0xc0);
        assert!(COMPAT_LOAD.len() <= 0x2000);
        let mut source = Disk::create(Format::D81, "HOOK", "65").unwrap();
        source
            .put(
                "DEMO",
                2,
                &[1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0],
                false,
            )
            .unwrap();
        let plain = build(&[source.clone()]).unwrap();
        assert_eq!(
            plain.bytes,
            build_with_compat(&[source.clone()], false).unwrap().bytes
        );
        let hooked = build_with_compat(&[source], true).unwrap();
        let h = hooked.offset(40, 0).unwrap();
        assert_eq!(plain.bytes[h + 60], 0);
        assert_eq!(hooked.bytes[h + 60], 1);
        let entries = hooked.entries().unwrap();
        let start = hooked.read(&entries[0]).unwrap();
        assert!(valid_basic(&start));
        assert!(start.windows(13).any(|b| b == b"LOAD HOOK INS"));
        assert_eq!(
            plain.read(&plain.entries().unwrap()[1]).unwrap(),
            hooked.read(&entries[1]).unwrap()
        );
        assert!(hooked
            .chain(&entries[0])
            .unwrap()
            .iter()
            .all(|ts| ts.1 < 40));
    }
    #[test]
    fn compatibility_supports_largest_menu_and_catalogue() {
        let mut source = Disk::create(Format::D81, "MAX HOOK", "65").unwrap();
        let program = [1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0];
        for i in 0..295 {
            source
                .put(
                    &format!("FILE{i:012}"),
                    if i < 99 { 2 } else { 1 },
                    if i < 99 { &program } else { b"data" },
                    false,
                )
                .unwrap();
        }
        let disk = build_with_compat(&[source], true).unwrap();
        let entries = disk.entries().unwrap();
        assert_eq!(entries.len(), 296);
        let start = disk.read(&entries[0]).unwrap();
        assert!(valid_basic(&start));
        assert!(start.len() <= MAX_PRG);
        assert!(disk.chain(&entries[0]).unwrap().iter().all(|ts| ts.1 < 40));
        assert_eq!(disk.read(&entries[295]).unwrap(), b"data");
    }
    #[test]
    fn rejects_bad_basic_pointers() {
        assert!(!valid_basic(&[1, 32, 1, 32, 10, 0, 0, 0]));
    }
    #[test]
    fn launcher_reclaims_standard_space() {
        let mut d = Disk::create(Format::D81, "TEST", "65").unwrap();
        let p = [1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0];
        assert!(valid_basic(&p));
        d.put("DEMO", 2, &p, false).unwrap();
        let merged = build(&[d]).unwrap();
        let entries = merged.entries().unwrap();
        assert_eq!(entries[0].name, "START");
        assert!(entries[0].start.1 < 40);
        assert!(entries[1].start.1 < 40);
        assert_eq!(merged.read(&entries[1]).unwrap(), p);
        assert!(merged.validate_writable().is_err());
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    fn program() -> Vec<u8> {
        vec![1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0]
    }
    fn disk(name: &str, data: &[u8]) -> Disk {
        let mut d = Disk::create(Format::D81, "TEST", "65").unwrap();
        d.put(name, 2, data, false).unwrap();
        d
    }
    #[test]
    fn rejects_unsupported_programs_and_reserved_name() {
        let mut c64 = program();
        c64[0] = 1;
        c64[1] = 8;
        assert!(build(&[disk("C64", &c64)]).is_err());
        assert!(build(&[disk("START", &program())]).is_err());
        let mut large = program();
        large.resize(MAX_PRG + 1, 0);
        assert!(build(&[disk("BIG", &large)]).is_err());
        assert!(build(&[]).is_err());
    }
    #[test]
    fn preserves_files_and_accepts_maximum_prg() {
        let mut max = program();
        max.resize(MAX_PRG, 0x55);
        let a = disk("FIRST", &max);
        let b = disk("SECOND", &program());
        let result = build(&[a, b]).unwrap();
        let es = result.entries().unwrap();
        assert_eq!(es.len(), 3);
        assert!(es[1].start.1 < 40);
        assert!(es[2].start.1 < 40);
        assert_eq!(result.read(&es[1]).unwrap(), max);
        let start = result.read(&es[0]).unwrap();
        assert!(valid_basic(&start));
    }
    #[test]
    fn imports_many_disks_and_preserves_every_file_and_source() {
        let mut payload = program();
        payload.resize(40_000, 0x55);
        let sources: Vec<_> = (0..58)
            .map(|i| disk(&format!("GAME{i:02}"), &payload))
            .collect();
        let originals: Vec<_> = sources.iter().map(|d| d.bytes.clone()).collect();
        let result = build(&sources).unwrap();
        let entries = result.entries().unwrap();
        assert_eq!(entries.len(), 59);
        assert!(entries.iter().any(|e| e.start.1 >= 80));
        for i in 0..58 {
            let e = entries
                .iter()
                .find(|e| e.name == format!("GAME{i:02}"))
                .unwrap();
            assert_eq!(result.read(e).unwrap(), payload);
            assert_eq!(sources[i].bytes, originals[i]);
        }
        assert!(result
            .directory_chain()
            .unwrap()
            .iter()
            .all(|&(t, s)| t == 40 && s < 40));
        assert!(result.validate_writable().is_err());
        assert!(
            entries.iter().skip(1).any(|e| {
                let chain = result.chain(e).unwrap();
                chain.windows(2).any(|p| p[0].1 / 40 != p[1].1 / 40)
            }),
            "at least one file crosses a sector-group boundary"
        );
        let (used, total) = result.resident_space().unwrap();
        assert_eq!(total, 9480);
        assert_eq!(used, entries.iter().map(|e| e.blocks).sum::<usize>());
        assert!(result.read(&entries[0]).unwrap().len() < 30_000);
    }
    #[test]
    fn packing_preserves_raw_names_flags_and_data_files() {
        let mut a = disk("ORIGINAL", &program());
        let e = a.entries().unwrap().remove(0);
        a.bytes[e.offset + 5] = 0xc1;
        a.bytes[e.offset + 2] |= 0x40;
        let raw = a.entries().unwrap()[0].raw_name;
        let b = disk("TMP000000", &program());
        let mut c = disk("THIRD", &program());
        c.put("DATA", 1, b"hello", false).unwrap();
        let result = build(&[a, b, c]).unwrap();
        let es = result.entries().unwrap();
        let e = es.iter().find(|e| e.raw_name == raw).unwrap();
        assert_eq!(e.flags, 0xc2);
        assert_eq!(result.read(e).unwrap(), program());
        assert_eq!(
            result
                .read(es.iter().find(|e| e.name == "DATA").unwrap())
                .unwrap(),
            b"hello"
        );
    }
    #[test]
    fn supports_full_catalogue_and_retains_large_data() {
        let mut source = disk("DEMO", &program());
        for i in 1..295 {
            source
                .put(
                    &format!("FILE{i:03}"),
                    1,
                    if i == 294 { b"last" } else { b"x" },
                    false,
                )
                .unwrap();
        }
        let result = build(&[source.clone()]).unwrap();
        let entries = result.entries().unwrap();
        assert_eq!(entries.len(), 296);
        assert_eq!(result.directory_chain().unwrap().last(), Some(&(40, 39)));
        assert_eq!(result.read(&entries[295]).unwrap(), b"last");
        let start = result.read(&entries[0]).unwrap();
        assert!(valid_basic(&start));
        source.put("OVER", 1, b"x", false).unwrap();
        assert!(build(&[source]).err().unwrap().contains("295"));
        let mut large = disk("DEMO", &program());
        large
            .put("LARGE DATA", 1, &vec![0x73; 70000], false)
            .unwrap();
        let result = build(&[large]).unwrap();
        assert_eq!(
            result.read(&result.entries().unwrap()[2]).unwrap(),
            vec![0x73; 70000]
        );
    }
    #[test]
    fn packing_rejects_conflicts_and_over_capacity() {
        assert!(build(&[
            disk("A", &program()),
            disk("B", &program()),
            disk("A", &program())
        ])
        .err()
        .unwrap()
        .contains("Duplicate"));
        let mut payload = program();
        payload.resize(40_000, 0);
        let sources: Vec<_> = (0..61)
            .map(|i| disk(&format!("GAME{i}"), &payload))
            .collect();
        assert!(build(&sources).err().unwrap().contains("exceed"));
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    #[test]
    fn directory_stays_in_rom_geometry_and_payloads_reclaim_standard_space() {
        let mut d = Disk::create(Format::D81, "DIRECTORY TEST", "65").unwrap();
        let p = [1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0];
        for i in 0..58 {
            d.put(&format!("ITEM{i:02}"), 2, &p, false).unwrap();
        }
        let built = build(&[d]).unwrap();
        let dirs = built.directory_chain().unwrap();
        assert_eq!(dirs, (3..=10).map(|s| (40, s)).collect::<Vec<_>>());
        let entries = built.entries().unwrap();
        assert_eq!(entries.len(), 59);
        assert_eq!(entries[0].name, "START");
        for e in entries.iter().skip(1) {
            assert!(e.start.1 < 40);
            assert_eq!(built.read(e).unwrap(), p);
        }
        let bam = built.offset(40, 1).unwrap() + 16 + 39 * 6;
        assert_eq!(built.bytes[bam], 0);
        assert_eq!(&built.bytes[bam + 1..bam + 6], &[0, 0, 0, 0, 0]);
        assert!(built.validate_writable().is_err());
    }
}
