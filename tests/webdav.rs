#![cfg(any(target_os = "macos", windows))]
use mega65_disklab::{
    disk::{Disk, Format},
    mount::bridge,
    store::Store,
};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Mutex},
};
fn request(url: &str, method: &str, suffix: &str, headers: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let rest = url.strip_prefix("http://").unwrap();
    let (addr, path) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    write!(stream,"{method} /{path}{suffix} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
    let mut all = Vec::new();
    stream.read_to_end(&mut all).unwrap();
    let end = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let code = std::str::from_utf8(&all[..end])
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = std::str::from_utf8(&all[..end])
        .unwrap()
        .to_ascii_lowercase();
    let mut body = all[end..].to_vec();
    if headers.contains("transfer-encoding: chunked") {
        let mut decoded = Vec::new();
        let mut pos = 0;
        loop {
            let line_end = body[pos..].windows(2).position(|w| w == b"\r\n").unwrap() + pos;
            let size =
                usize::from_str_radix(std::str::from_utf8(&body[pos..line_end]).unwrap(), 16)
                    .unwrap();
            if size == 0 {
                break;
            }
            pos = line_end + 2;
            decoded.extend_from_slice(&body[pos..pos + size]);
            pos += size + 2;
        }
        body = decoded;
    }
    (code, body)
}
#[test]
fn windows_probes_do_not_expose_session_contents() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Mutex::new(
        Store::create(
            &dir.path().join("probe.d65"),
            Disk::create(Format::D65, "PROBE", "65").unwrap(),
        )
        .unwrap(),
    ));
    let b = bridge(s, false).unwrap();
    let root = format!(
        "{}/",
        b.url
            .trim_start_matches("http://")
            .split_once('/')
            .unwrap()
            .0
    );
    let root = format!("http://{root}");
    assert_eq!(request(&root, "OPTIONS", "", "", b"").0, 200);
    assert_eq!(request(&root, "PROPFIND", "", "Depth: 1\r\n", b"").0, 404);
    let trimmed = b.url.trim_end_matches('/');
    assert_eq!(request(trimmed, "PROPFIND", "", "Depth: 0\r\n", b"").0, 207);
    assert_eq!(
        request(trimmed, "PROPFIND", "-wrong", "Depth: 1\r\n", b"").0,
        404
    );
}
#[test]
fn dav_roundtrip_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Mutex::new(
        Store::create(
            &dir.path().join("test.d65"),
            Disk::create(Format::D65, "TEST", "65").unwrap(),
        )
        .unwrap(),
    ));
    let b = bridge(s.clone(), false).unwrap();
    assert_eq!(request(&b.url, "OPTIONS", "", "", b"").0, 200);
    assert_eq!(request(&b.url, "PUT", "one.seq", "", b"hello").0, 201);
    assert_eq!(request(&b.url, "GET", "ONE.seq", "", b"").1, b"hello");
    assert_eq!(
        request(&b.url, "GET", "ONE.seq", "Range: bytes=1-3\r\n", b"").1,
        b"ell"
    );
    assert_eq!(
        request(&b.url, "PUT", "ONE.seq", "If-None-Match: *\r\n", b"changed").0,
        412
    );
    assert_eq!(
        request(&b.url, "PUT", "._ONE.seq", "", b"MAC METADATA").0,
        201
    );
    assert_eq!(s.lock().unwrap().disk.entries().unwrap().len(), 1);
    let (dest, _) = (format!("Destination: {}TWO.seq\r\n", b.url), 0);
    assert_eq!(request(&b.url, "MOVE", "ONE.seq", &dest, b"").0, 201);
    assert_eq!(request(&b.url, "GET", "ONE.seq", "", b"").0, 404);
    assert_eq!(request(&b.url, "GET", "TWO.seq", "", b"").1, b"hello");
    assert_eq!(request(&b.url, "DELETE", "TWO.seq", "", b"").0, 204);
    assert!(s.lock().unwrap().disk.entries().unwrap().is_empty());
}
#[test]
fn read_only_and_invalid_names() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Mutex::new(
        Store::create(
            &dir.path().join("test.d65"),
            Disk::create(Format::D65, "TEST", "65").unwrap(),
        )
        .unwrap(),
    ));
    let b = bridge(s.clone(), true).unwrap();
    assert_eq!(request(&b.url, "PUT", "NEW.prg", "", b"abc").0, 403);
    assert_eq!(
        request(&b.url, "PROPFIND", "%2e%2e%2fsecret", "", b"").0,
        404
    );
    assert!(s.lock().unwrap().disk.entries().unwrap().is_empty());
}
#[test]
fn full_disk_move_and_failed_copy() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = Disk::create(Format::D65, "TEST", "65").unwrap();
    let bytes = vec![9; 802640];
    d.put("FULL", 2, &bytes, false).unwrap();
    let s = Arc::new(Mutex::new(
        Store::create(&dir.path().join("full.d65"), d).unwrap(),
    ));
    let b = bridge(s.clone(), false).unwrap();
    let dest = format!("Destination: {}RENAMED.prg\r\n", b.url);
    assert_eq!(request(&b.url, "MOVE", "FULL.prg", &dest, b"").0, 201);
    assert_eq!(request(&b.url, "GET", "RENAMED.prg", "", b"").1, bytes);
    let dest = format!("Destination: {}COPY.prg\r\n", b.url);
    assert_eq!(request(&b.url, "COPY", "RENAMED.prg", &dest, b"").0, 507);
    s.lock().unwrap().disk.validate_writable().unwrap();
}
#[test]
fn special_names_and_replace() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(Mutex::new(
        Store::create(
            &dir.path().join("test.d65"),
            Disk::create(Format::D65, "TEST", "65").unwrap(),
        )
        .unwrap(),
    ));
    let b = bridge(s, false).unwrap();
    assert_eq!(request(&b.url, "PUT", "FILE%23.seq", "", b"first").0, 201);
    assert_eq!(request(&b.url, "PUT", "FILE%23.seq", "", b"second").0, 204);
    assert_eq!(request(&b.url, "GET", "FILE%23.seq", "", b"").1, b"second");
}

#[test]
fn merged_images_are_readable_only() {
    let dir = tempfile::tempdir().unwrap();
    let a = Disk::create(Format::D81, "ONE", "65").unwrap();
    let mut b = Disk::create(Format::D81, "TWO", "65").unwrap();
    b.put("HIGH", 2, b"second bank", false).unwrap();
    let merged = Disk::merge_d81(&[a, b]).unwrap();
    let s = Arc::new(Mutex::new(
        Store::create(&dir.path().join("merged.d65"), merged).unwrap(),
    ));
    assert!(bridge(s.clone(), false).is_err());
    let server = bridge(s.clone(), true).unwrap();
    assert_eq!(
        request(&server.url, "GET", "HIGH.prg", "", b"").1,
        b"second bank"
    );
    assert_eq!(
        request(&server.url, "PUT", "HIGH.prg", "", b"overwrite").0,
        403
    );
    assert_eq!(request(&server.url, "DELETE", "HIGH.prg", "", b"").0, 403);
}
