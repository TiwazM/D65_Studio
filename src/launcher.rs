//! Experimental direct-controller launcher. No runtime assembler required.
use crate::disk::{display_name, encode_name, Disk, Entry, Format, Result};
const COMPAT_READER: &[u8] = include_bytes!("../launcher/reader-compat.bin");
const COMPAT_GATEWAY: &[u8] = include_bytes!("../launcher/compat-gateway.bin");
const COMPAT_LOAD: &[u8] = include_bytes!("../launcher/compat-load.bin");
const READER: &[u8] = include_bytes!("../launcher/reader.bin");
const FLOPPY_READER: &[u8] = include_bytes!("../launcher/reader-floppy.bin");
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
        ("RUN", &[0x8a]),
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

/// Build a read-only compilation using every logical sector (0..255) of every
/// track except the directory track. START alone uses ordinary DOS allocation;
/// payload chains can start anywhere because the resident reader reads the
/// mounted image's SD sectors directly instead of F011 track/sector addresses.
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
            files.push((entry, data));
            if files.len() > 295 {
                return Err("Too many files: at most 295 files plus START.".into());
            }
        }
    }
    let programs = files.iter().filter(|(e, d)| is_menu_program(e, d)).count();
    if programs == 0 || programs > 99 {
        return Err("The menu needs 1–99 MEGA65 BASIC-entry PRGs at $2001, at most 55,041 bytes each. Other files are retained for the resident reader.".into());
    }
    pack(&files, "D65 RESIDENT", "65", compatibility, Format::D65)
}

/// Lay out START, the catalogue and every file in a fresh resident image.
/// Resident images are changed by repacking, never by DOS-style allocation.
fn pack(
    files: &[(Entry, Vec<u8>)],
    label: &str,
    id: &str,
    compatibility: bool,
    format: Format,
) -> Result<Disk> {
    if format == Format::D81 {
        return Err("Resident compilations are D65 images or HD floppies.".into());
    }
    let floppy = format == Format::Hd;
    if floppy && compatibility {
        return Err("LOAD compatibility is not available on floppies.".into());
    }
    if files.len() > 295 {
        return Err("Too many files: at most 295 files plus START.".into());
    }
    let menu: Vec<_> = files
        .iter()
        .enumerate()
        .filter(|(_, (e, d))| is_menu_program(e, d))
        .map(|(i, (e, _))| (e.name.clone(), i))
        .collect();
    if menu.len() > 99 {
        return Err("The START menu holds at most 99 MEGA65 BASIC programs.".into());
    }
    let mut catalog = vec![0; 8 + CATALOG_ENTRY * files.len()];
    catalog[..5].copy_from_slice(b"D65R\x02");
    catalog[5..7].copy_from_slice(&(files.len() as u16).to_le_bytes());
    // DATA values use three decimal digits, so substituting final addresses
    // cannot change START's length or allocation (no layout fixed-point guess).
    let placeholder = make_start(&menu, &catalog, compatibility, floppy)?;
    let mut disk = Disk::create(format, label, id)?;
    disk.put("START", 2, &placeholder, false)?;
    let start = disk.entries()?.remove(0);
    let start_chain = disk.chain(&start)?;
    let mut occupied: std::collections::HashSet<_> = start_chain.iter().copied().collect();
    // AUTOBOOT.C65 is loaded by the ROM, so like START it must sit on the
    // DOS-allocated sectors 0..39 rather than wherever the packer puts files.
    let mut autoboot_chain = Vec::new();
    if let Some((_, data)) = files.iter().find(|(e, _)| e.name == AUTOBOOT) {
        disk.put(AUTOBOOT, 2, data, false)?;
        let e = disk
            .entries()?
            .into_iter()
            .find(|e| e.name == AUTOBOOT)
            .ok_or("AUTOBOOT.C65 allocation failed.")?;
        autoboot_chain = disk.chain(&e)?;
        occupied.extend(autoboot_chain.iter().copied());
    }
    let free = free_blocks(format, &occupied);
    // Hidden files are usually helpers a program loads by itself through the
    // ROM, so they take the ROM-readable blocks before anything else.
    let mut chains: Vec<Vec<(u8, u8)>> = vec![Vec::new(); files.len()];
    let mut cursor = 0;
    for hidden_pass in [true, false] {
        for (index, (entry, data)) in files.iter().enumerate() {
            if entry.hidden != hidden_pass || entry.name == AUTOBOOT {
                continue;
            }
            let blocks = data.len().div_ceil(254).max(1);
            if cursor + blocks > free.len() {
                break;
            }
            chains[index] = free[cursor..cursor + blocks].to_vec();
            cursor += blocks;
        }
    }
    debug_assert_eq!(
        free.len() + start_chain.len() + autoboot_chain.len(),
        format.payload_blocks()
    );
    if floppy {
        // The ROM loads START and AUTOBOOT.C65, and only reads 1581 tracks.
        let rom = |chain: &[(u8, u8)]| {
            chain
                .iter()
                .all(|(t, _)| crate::disk::HD_DD_TRACKS.contains(t))
        };
        if !rom(&start_chain) || !rom(&autoboot_chain) {
            return Err("START is too large for the floppy's 1581 boot tracks.".into());
        }
    }
    let needed: usize = files
        .iter()
        .filter(|(e, _)| e.name != AUTOBOOT)
        .map(|(_, d)| d.len().div_ceil(254).max(1))
        .sum();
    if needed > free.len() {
        return Err(format!(
            "These files exceed the {} capacity: {} blocks needed, {} available after START.",
            if floppy { "HD floppy" } else { "launch disk" },
            needed,
            free.len()
        ));
    }
    let mut slots = vec![disk.bytes[start.offset + 2..start.offset + 32].to_vec()];
    for (index, (entry, data)) in files.iter().enumerate() {
        let blocks = data.len().div_ceil(254).max(1);
        let chain: &[(u8, u8)] = if entry.name == AUTOBOOT {
            &autoboot_chain
        } else {
            write_chain(&mut disk, &chains[index], data)?;
            &chains[index]
        };
        let mut slot = vec![0; 30];
        slot[0] = entry.flags;
        slot[22] = u8::from(entry.hidden);
        slot[1] = chain[0].0;
        slot[2] = chain[0].1;
        slot[3..19].copy_from_slice(&entry.raw_name);
        slot[28..30].copy_from_slice(&(blocks as u16).to_le_bytes());
        slots.push(slot);
        let c = &mut catalog[8 + index * CATALOG_ENTRY..8 + (index + 1) * CATALOG_ENTRY];
        c[..16].copy_from_slice(&entry.raw_name);
        c[16..18].copy_from_slice(&[chain[0].0, chain[0].1]);
        // ABI 2. Below 64 KiB: length, byte sum, type, 0. From 64 KiB on the
        // length field holds $FFFF (whole-file reads refuse it, never a
        // wrap), the sum field length bits 0-15 and byte 23 bits 16-23, for
        // the streaming commands.
        let checksum = data.iter().fold(0u16, |sum, b| sum.wrapping_add(*b as u16));
        let (length, sum, high) = match u16::try_from(data.len()) {
            Ok(n) => (n, checksum, 0),
            Err(_) => (u16::MAX, data.len() as u16, (data.len() >> 16) as u8),
        };
        c[18..20].copy_from_slice(&length.to_le_bytes());
        c[20..22].copy_from_slice(&sum.to_le_bytes());
        c[22] = entry.kind;
        c[23] = high;
    }
    let starter = make_start(&menu, &catalog, compatibility, floppy)?;
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
            if ts.0 == 40 || !used.insert(ts) {
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

fn is_menu_program(entry: &Entry, data: &[u8]) -> bool {
    entry.kind == 2 && valid_basic(data) && entry.name != AUTOBOOT && !entry.hidden
}

/// Keep a file out of (or put it back into) the START menu; it stays on the
/// disk and readable, only the menu changes.
pub fn set_hidden(disk: &mut Disk, entry: &Entry, hidden: bool) -> Result<()> {
    let mut files = contents(disk)?;
    let (e, _) = files
        .iter_mut()
        .find(|(e, _)| e.raw_name == entry.raw_name)
        .ok_or("File not found; START itself cannot be hidden.")?;
    e.hidden = hidden;
    repack(disk, &files)
}

/// The file BASIC 65 runs by itself from drive 8 after a reset. Ours is a
/// one-line program that runs START, so the menu appears whenever the image
/// is mounted and the MEGA65 restarts.
pub const AUTOBOOT: &str = "AUTOBOOT.C65";
pub fn autoboot_program() -> Vec<u8> {
    let mut basic = Vec::new();
    line(&mut basic, 10, &token("RUN\"START\""));
    basic.extend_from_slice(&[0, 0]);
    let mut prg = vec![1, 0x20];
    prg.extend(basic);
    prg
}
pub fn has_autoboot(disk: &Disk) -> bool {
    disk.entries()
        .map(|e| e.iter().any(|e| e.name == AUTOBOOT))
        .unwrap_or(false)
}
/// Add or remove AUTOBOOT.C65 on a resident image.
pub fn set_autoboot(disk: &mut Disk, on: bool) -> Result<()> {
    if !is_editable(disk) {
        return Err("Only resident-reader images have a START for AUTOBOOT.C65 to run.".into());
    }
    match (on, disk.entries()?.into_iter().find(|e| e.name == AUTOBOOT)) {
        (true, None) => disk.put(AUTOBOOT, 2, &autoboot_program(), false),
        (true, Some(e)) => disk.put(&e.name, 2, &autoboot_program(), true),
        (false, Some(e)) => disk.remove(&e),
        (false, None) => Ok(()),
    }
}

/// An empty resident-reader D65: every change repacks the files and
/// regenerates START and its catalogue.
pub fn create_editable(label: &str, id: &str) -> Result<Disk> {
    if encode_name(id, 2)?.len() != 2 {
        return Err("Disk ID must be exactly two characters.".into());
    }
    pack(&[], label, id, false, Format::D65)
}

/// An empty DiskLab HD floppy (about 1.66 MB) with a floppy START.
pub fn create_floppy(label: &str, id: &str) -> Result<Disk> {
    if encode_name(id, 2)?.len() != 2 {
        return Err("Disk ID must be exactly two characters.".into());
    }
    pack(&[], label, id, false, Format::Hd)
}

/// The same files packed for an HD floppy (or back into a D65).
pub fn convert_resident(disk: &Disk, format: Format) -> Result<Disk> {
    if !is_editable(disk) {
        return Err("Only resident-reader compilations can be converted this way.".into());
    }
    let compat = compatibility(disk)? && format == Format::D65;
    pack(
        &contents(disk)?,
        &disk.label(),
        &disk.disk_id(),
        compat,
        format,
    )
}

/// Any resident-reader compilation can be repacked from its own directory,
/// whether it was merged from D81s or created empty.
pub fn is_editable(disk: &Disk) -> bool {
    let Ok(h) = disk.offset(40, 0) else {
        return false;
    };
    disk.format != Format::D81 && disk.bytes[h + 48..h + 56] == *b"D65RES01"
}

fn compatibility(disk: &Disk) -> Result<bool> {
    Ok(disk.bytes[disk.offset(40, 0)? + 60] == 1)
}

/// The same files laid out again by this version of MEGA65 DiskLab, with a
/// freshly generated START.
fn regenerate(disk: &Disk) -> Result<Disk> {
    pack(
        &contents(disk)?,
        &disk.label(),
        &disk.disk_id(),
        compatibility(disk)?,
        disk.format,
    )
}

/// Replace START (and its embedded reader and catalogue) with the current
/// one. Files are kept byte for byte; only their placement may move.
pub fn update_start(disk: &mut Disk) -> Result<()> {
    if !is_editable(disk) {
        return Err("Only resident-reader compilations have a START to update.".into());
    }
    *disk = regenerate(disk)?;
    Ok(())
}

/// True when START differs from what this version of MEGA65 DiskLab generates,
/// for example after a reader fix.
pub fn start_outdated(disk: &Disk) -> Result<bool> {
    let fresh = regenerate(disk)?;
    Ok(disk.read(&disk.entries()?[0])? != fresh.read(&fresh.entries()?[0])?)
}

/// Every file except START, with its bytes, in directory order.
fn contents(disk: &Disk) -> Result<Vec<(Entry, Vec<u8>)>> {
    let mut entries = disk.entries()?.into_iter();
    if entries
        .next()
        .is_none_or(|e| e.raw_name[..5] != *b"START" || e.raw_name[5..].iter().any(|b| *b != 0xa0))
    {
        return Err("Resident image is missing its START launcher.".into());
    }
    entries
        .map(|e| {
            let data = disk.read(&e)?;
            Ok((e, data))
        })
        .collect()
}

fn repack(disk: &mut Disk, files: &[(Entry, Vec<u8>)]) -> Result<()> {
    if !is_editable(disk) {
        return Err("Only resident-reader compilations can be repacked.".into());
    }
    *disk = pack(
        files,
        &disk.label(),
        &disk.disk_id(),
        compatibility(disk)?,
        disk.format,
    )?;
    Ok(())
}

fn raw_name(name: &str) -> Result<[u8; 16]> {
    let name = encode_name(name, 16)?;
    if name == b"START" {
        return Err("START is reserved for the launcher; choose another name.".into());
    }
    let mut raw = [0xa0; 16];
    raw[..name.len()].copy_from_slice(&name);
    Ok(raw)
}

pub fn put(disk: &mut Disk, name: &str, kind: u8, data: &[u8], replace: bool) -> Result<()> {
    if !(1..=3).contains(&kind) {
        return Err("Choose SEQ, PRG or USR.".into());
    }
    let raw = raw_name(name)?;
    let mut files = contents(disk)?;
    if let Some((entry, bytes)) = files.iter_mut().find(|(e, _)| e.raw_name == raw) {
        if !replace {
            return Err("A file with this name already exists.".into());
        }
        if entry.flags & 0x40 != 0 {
            return Err("File is locked.".into());
        }
        entry.kind = kind;
        entry.flags = (entry.flags & !7) | kind;
        *bytes = data.to_vec();
    } else {
        files.push((
            Entry {
                offset: 0,
                name: display_name(&raw),
                raw_name: raw,
                kind,
                flags: 0x80 | kind,
                hidden: false,
                blocks: 0,
                start: (0, 0),
            },
            data.to_vec(),
        ));
    }
    repack(disk, &files)
}

pub fn remove(disk: &mut Disk, entry: &Entry) -> Result<()> {
    if entry.flags & 0x40 != 0 {
        return Err("File is locked.".into());
    }
    let mut files = contents(disk)?;
    let before = files.len();
    files.retain(|(e, _)| e.raw_name != entry.raw_name);
    if files.len() == before {
        return Err("File not found; it cannot be START.".into());
    }
    repack(disk, &files)
}

pub fn rename(disk: &mut Disk, entry: &Entry, name: &str) -> Result<()> {
    if entry.flags & 0x40 != 0 {
        return Err("File is locked.".into());
    }
    let raw = raw_name(name)?;
    let mut files = contents(disk)?;
    if raw != entry.raw_name && files.iter().any(|(e, _)| e.raw_name == raw) {
        return Err("That filename already exists.".into());
    }
    let (e, _) = files
        .iter_mut()
        .find(|(e, _)| e.raw_name == entry.raw_name)
        .ok_or("File not found; START cannot be renamed.")?;
    e.raw_name = raw;
    e.name = display_name(&raw);
    repack(disk, &files)
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

/// Free blocks in packing order. D65: the D81-shaped area first (tracks 1..80,
/// sectors 0..39, which the ROM can load from), then the rest of each
/// 40-sector group. HD floppy: the 1581 tracks first (ROM-loadable), then the
/// HD tracks.
fn free_blocks(format: Format, occupied: &std::collections::HashSet<(u8, u8)>) -> Vec<(u8, u8)> {
    let mut free = Vec::new();
    let mut take = |t: u8, s: usize| {
        if !occupied.contains(&(t, s as u8)) {
            free.push((t, s as u8));
        }
    };
    if format == Format::Hd {
        for t in crate::disk::HD_DD_TRACKS.filter(|t| *t != 40) {
            (0..40).for_each(|s| take(t, s));
        }
        for t in (1..=format.tracks()).filter(|t| !crate::disk::HD_DD_TRACKS.contains(t)) {
            (0..format.sectors(t)).for_each(|s| take(t, s));
        }
        return free;
    }
    for group in (0..256).step_by(40) {
        for t in 1..=85u8 {
            if t == 40 || (group == 0 && t > 80) {
                continue;
            }
            (group..(group + 40).min(256)).for_each(|s| take(t, s));
        }
    }
    for t in 81..=85 {
        (0..40).for_each(|s| take(t, s));
    }
    free
}

fn make_start(
    menu: &[(String, usize)],
    catalog: &[u8],
    compatibility: bool,
    floppy: bool,
) -> Result<Vec<u8>> {
    let reader = if floppy {
        FLOPPY_READER
    } else if compatibility {
        COMPAT_READER
    } else {
        READER
    };
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
        &token("PRINT\"MEGA65 DISKLAB - RESIDENT READER\""),
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
    if menu.is_empty() {
        line(
            &mut basic,
            100,
            &token("PRINT\"NO PROGRAMS ON THIS DISK YET\":END"),
        );
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
        assert_eq!(total, 84 * 256);
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
    fn uses_extended_sectors_and_high_tracks() {
        // About 3.3 MB of data lands beyond the old 120-sector limit: on
        // logical sectors 128..255 and on tracks 81..85.
        let sources: Vec<_> = (0..4)
            .map(|i| {
                let mut d = disk(&format!("GAME{i}"), &program());
                d.put(&format!("DATA{i}"), 1, &vec![i as u8 + 1; 800_000], false)
                    .unwrap();
                d
            })
            .collect();
        let result = build(&sources).unwrap();
        let entries = result.entries().unwrap();
        let mut high_sector = false;
        let mut high_track = false;
        for e in &entries {
            for (t, s) in result.chain(e).unwrap() {
                assert!((1..=85).contains(&t) && t != 40);
                high_sector |= s >= 128;
                high_track |= t > 80;
            }
        }
        assert!(high_sector && high_track);
        for (i, e) in entries
            .iter()
            .filter(|e| e.name.starts_with("DATA"))
            .enumerate()
        {
            assert_eq!(result.read(e).unwrap(), vec![i as u8 + 1; 800_000]);
        }
        let (used, total) = result.resident_space().unwrap();
        assert_eq!(total, 84 * 256);
        assert_eq!(used, entries.iter().map(|e| e.blocks).sum::<usize>());
        // Every block maps to one half of one 512-byte image sector, exactly
        // as the reader computes it: (track-1)*128 + sector/2, half = sector&1.
        for e in &entries {
            for (t, s) in result.chain(e).unwrap() {
                let sd = (t as usize - 1) * 128 + (s as usize >> 1);
                assert_eq!(
                    result.offset(t, s).unwrap(),
                    sd * 512 + (s as usize & 1) * 256
                );
            }
        }
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
        // Seven D81s each nearly full of data: about 5.6 MB, past the 5.46 MB payload space.
        let sources: Vec<_> = (0..7)
            .map(|i| {
                let mut d = disk(&format!("GAME{i}"), &program());
                d.put(&format!("DATA{i}"), 1, &vec![0x42; 800_000], false)
                    .unwrap();
                d
            })
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

#[cfg(test)]
mod editable_tests {
    use super::*;
    const PROGRAM: [u8; 12] = [1, 32, 9, 32, 10, 0, 0x99, 0x22, 0x22, 0, 0, 0];
    #[test]
    fn empty_large_disk_has_a_start_menu_and_is_editable() {
        let disk = create_editable("BIG DISK", "65").unwrap();
        assert!(is_editable(&disk));
        assert_eq!(disk.label(), "BIG DISK");
        assert_eq!(disk.bytes.len(), crate::disk::D65_SIZE);
        let entries = disk.entries().unwrap();
        assert_eq!(entries.len(), 1);
        let start = disk.read(&entries[0]).unwrap();
        assert!(valid_basic(&start));
        assert!(start.windows(11).any(|b| b == b"NO PROGRAMS"));
        assert!(disk.validate_writable().is_err());
        assert!(is_editable(
            &build(&[{
                let mut d = Disk::create(Format::D81, "SRC", "65").unwrap();
                d.put("DEMO", 2, &PROGRAM, false).unwrap();
                d
            }])
            .unwrap()
        ));
    }
    #[test]
    fn add_rename_and_delete_rebuild_start_and_catalogue() {
        let mut disk = create_editable("BIG DISK", "AB").unwrap();
        disk.put("GAME", 2, &PROGRAM, false).unwrap();
        disk.put("DATA", 1, &vec![0x42; 900_000], false).unwrap();
        assert!(disk.put("GAME", 2, &PROGRAM, false).is_err());
        assert!(disk.put("START", 2, &PROGRAM, false).is_err());
        let es = disk.entries().unwrap();
        assert_eq!(
            es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["START", "GAME", "DATA"]
        );
        assert_eq!(disk.read(&es[2]).unwrap(), vec![0x42; 900_000]);
        assert!(disk.read(&es[0]).unwrap().windows(4).any(|b| b == b"GAME"));
        assert_eq!(disk.disk_id(), "AB");
        disk.rename(&es[1], "PLAY ME").unwrap();
        let es = disk.entries().unwrap();
        assert_eq!(es[1].name, "PLAY ME");
        let start = disk.read(&es[0]).unwrap();
        assert!(start.windows(7).any(|b| b == b"PLAY ME"));
        assert!(!start.windows(4).any(|b| b == b"GAME"));
        assert!(disk.rename(&es[1], "DATA").is_err());
        assert!(disk.rename(&es[0], "OTHER").is_err());
        disk.put("DATA", 1, b"short", true).unwrap();
        disk.remove(&disk.entries().unwrap()[1]).unwrap();
        let es = disk.entries().unwrap();
        assert_eq!(es.len(), 2);
        assert_eq!(disk.read(&es[1]).unwrap(), b"short");
        assert!(is_editable(&disk));
        let (used, total) = disk.resident_space().unwrap();
        assert_eq!(total, crate::disk::PAYLOAD_BLOCKS);
        assert_eq!(used, es.iter().map(|e| e.blocks).sum::<usize>());
    }
    #[test]
    fn merged_compilations_are_editable_and_start_can_be_updated() {
        let mut source = Disk::create(Format::D81, "SRC", "65").unwrap();
        source.put("DEMO", 2, &PROGRAM, false).unwrap();
        source.put("DATA", 1, &vec![0x31; 70_000], false).unwrap();
        let mut disk = build_with_compat(&[source], true).unwrap();
        assert!(is_editable(&disk));
        assert!(!start_outdated(&disk).unwrap());
        disk.put("EXTRA", 2, &PROGRAM, false).unwrap();
        let es = disk.entries().unwrap();
        assert_eq!(
            es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["START", "DEMO", "DATA", "EXTRA"]
        );
        assert!(disk.read(&es[0]).unwrap().windows(5).any(|b| b == b"EXTRA"));
        // An older START (here: one byte of its DATA altered) is detected and replaced.
        let p = disk.offset(es[0].start.0, es[0].start.1).unwrap() + 100;
        disk.bytes[p] ^= 1;
        assert!(start_outdated(&disk).unwrap());
        update_start(&mut disk).unwrap();
        assert!(!start_outdated(&disk).unwrap());
        let es = disk.entries().unwrap();
        assert_eq!(es.len(), 4);
        assert_eq!(disk.read(&es[2]).unwrap(), vec![0x31; 70_000]);
        assert_eq!(disk.read(&es[3]).unwrap(), PROGRAM);
        assert_eq!(
            disk.bytes[disk.offset(40, 0).unwrap() + 60],
            1,
            "compatibility flag kept"
        );
        assert!(valid_basic(&disk.read(&es[0]).unwrap()));
        let mut plain = Disk::create(Format::D65, "PLAIN", "65").unwrap();
        assert!(update_start(&mut plain).is_err());
    }
    #[test]
    fn autoboot_runs_start_without_joining_the_menu() {
        let mut disk = create_editable("BOOT", "65").unwrap();
        disk.put("GAME", 2, &PROGRAM, false).unwrap();
        assert!(!has_autoboot(&disk));
        set_autoboot(&mut disk, true).unwrap();
        assert!(has_autoboot(&disk));
        let es = disk.entries().unwrap();
        let boot = disk
            .read(es.iter().find(|e| e.name == AUTOBOOT).unwrap())
            .unwrap();
        assert!(valid_basic(&boot));
        assert_eq!(
            boot,
            [1, 0x20, 0x0e, 0x20, 10, 0, 0x8a, b'"', b'S', b'T', b'A', b'R', b'T', b'"', 0, 0, 0]
        );
        let start = disk.read(&es[0]).unwrap();
        assert!(start.windows(4).any(|b| b == b"GAME"));
        assert!(!start.windows(8).any(|b| b == b"AUTOBOOT"));
        // The ROM loads it, so it must stay on sectors the ROM can read, even
        // on a full disk where ordinary files spill into the high sectors.
        disk.put("BIG", 1, &vec![0x11; 5_000_000], false).unwrap();
        let es = disk.entries().unwrap();
        let boot = es.iter().find(|e| e.name == AUTOBOOT).unwrap();
        assert!(disk
            .chain(boot)
            .unwrap()
            .iter()
            .all(|(t, s)| *t != 40 && *s < 40));
        assert_eq!(disk.read(boot).unwrap(), autoboot_program());
        assert!(disk.chain(&es[0]).unwrap().iter().all(|(_, s)| *s < 40));
        disk.remove(es.iter().find(|e| e.name == "BIG").unwrap())
            .unwrap();
        set_autoboot(&mut disk, true).unwrap();
        assert_eq!(disk.entries().unwrap().len(), 3);
        set_autoboot(&mut disk, false).unwrap();
        assert!(!has_autoboot(&disk));
        let mut plain = Disk::create(Format::D81, "P", "65").unwrap();
        assert!(set_autoboot(&mut plain, true).is_err());
    }
    #[test]
    fn hidden_files_stay_on_disk_but_leave_the_menu() {
        let mut disk = create_editable("MENU", "65").unwrap();
        disk.put("GAME", 2, &PROGRAM, false).unwrap();
        disk.put("TOOL", 2, &PROGRAM, false).unwrap();
        let tool = disk
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "TOOL")
            .unwrap();
        set_hidden(&mut disk, &tool, true).unwrap();
        let es = disk.entries().unwrap();
        assert!(es.iter().find(|e| e.name == "TOOL").unwrap().hidden);
        assert!(!es.iter().find(|e| e.name == "GAME").unwrap().hidden);
        let start = disk.read(&es[0]).unwrap();
        assert!(start.windows(4).any(|b| b == b"GAME") && !start.windows(4).any(|b| b == b"TOOL"));
        // The flag survives later repacks and reopening.
        disk.put("DATA", 1, b"x", false).unwrap();
        let again = Disk::parse(disk.bytes.clone()).unwrap();
        let tool = again
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "TOOL")
            .unwrap();
        assert!(tool.hidden && again.read(&tool).unwrap() == PROGRAM);
        set_hidden(&mut disk, &tool, false).unwrap();
        assert!(disk
            .read(&disk.entries().unwrap()[0])
            .unwrap()
            .windows(4)
            .any(|b| b == b"TOOL"));
        let start = disk.entries().unwrap()[0].clone();
        assert!(set_hidden(&mut disk, &start, true).is_err());
        // A hidden helper is loaded by the program through the ROM, so it
        // stays on D81-shaped blocks even when the disk is nearly full.
        disk.put("FILL", 1, &vec![0x22; 5_200_000], false).unwrap();
        let helper = disk
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "TOOL")
            .unwrap();
        set_hidden(&mut disk, &helper, true).unwrap();
        let helper = disk
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "TOOL")
            .unwrap();
        assert!(disk
            .chain(&helper)
            .unwrap()
            .iter()
            .all(|(t, s)| (1..=80).contains(t) && *t != 40 && *s < 40));
        assert_eq!(disk.read(&helper).unwrap(), PROGRAM);
    }
    #[test]
    fn hd_floppy_packs_into_its_geometry() {
        use crate::disk::{HD_DD_TRACKS, HD_SIZE};
        let mut disk = create_floppy("FLOPPY", "65").unwrap();
        assert_eq!(disk.format, Format::Hd);
        assert_eq!(disk.bytes.len(), HD_SIZE);
        assert!(is_editable(&disk));
        set_autoboot(&mut disk, true).unwrap();
        disk.put("GAME", 2, &PROGRAM, false).unwrap();
        disk.put("HELPER", 2, &PROGRAM, false).unwrap();
        let helper = disk
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "HELPER")
            .unwrap();
        set_hidden(&mut disk, &helper, true).unwrap();
        disk.put("DATA", 1, &vec![0x5a; 1_500_000], false).unwrap();
        let again = Disk::parse(disk.bytes.clone()).unwrap();
        assert_eq!(again.format, Format::Hd);
        let es = again.entries().unwrap();
        let rom = |name: &str| {
            let e = es.iter().find(|e| e.name == name).unwrap();
            again
                .chain(e)
                .unwrap()
                .iter()
                .all(|(t, _)| HD_DD_TRACKS.contains(t))
        };
        assert!(rom("START") && rom("AUTOBOOT.C65") && rom("HELPER"));
        for e in &es {
            for (t, s) in again.chain(e).unwrap() {
                assert!((1..=83).contains(&t) && t != 40);
                assert!((s as usize) < Format::Hd.sectors(t));
            }
        }
        let data = es.iter().find(|e| e.name == "DATA").unwrap();
        assert_eq!(again.read(data).unwrap(), vec![0x5a; 1_500_000]);
        let start = again.read(&es[0]).unwrap();
        assert!(start.windows(4).any(|b| b == b"GAME"));
        let (used, total) = again.resident_space().unwrap();
        assert_eq!(total, 8 * 40 + 74 * 84);
        assert!(used < total);
        // A D65 compilation converts to a floppy and back, files unchanged.
        let mut d65 = create_editable("BIG", "65").unwrap();
        d65.put("GAME", 2, &PROGRAM, false).unwrap();
        d65.put("BLOB", 1, &vec![7; 300_000], false).unwrap();
        let fl = convert_resident(&d65, Format::Hd).unwrap();
        let back = convert_resident(&fl, Format::D65).unwrap();
        let blob = |d: &Disk| {
            let e = d
                .entries()
                .unwrap()
                .into_iter()
                .find(|e| e.name == "BLOB")
                .unwrap();
            d.read(&e).unwrap()
        };
        assert_eq!(blob(&fl), vec![7; 300_000]);
        assert_eq!(blob(&back), vec![7; 300_000]);
        assert!(disk.put("HUGE", 1, &vec![1; 1_800_000], false).is_err());
    }
    #[test]
    fn store_edits_and_reopens_large_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("BIG.d65");
        let mut store =
            crate::store::Store::create(&path, create_editable("BIG", "65").unwrap()).unwrap();
        store.change(|d| d.put("GAME", 2, &PROGRAM, false)).unwrap();
        drop(store);
        let store = crate::store::Store::open(&path).unwrap();
        assert!(is_editable(&store.disk));
        assert_eq!(store.disk.entries().unwrap()[1].name, "GAME");
        assert!(store.backup.is_none());
    }
}
