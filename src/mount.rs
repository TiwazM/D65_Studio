//! Local WebDAV bridge for macOS's built-in Finder filesystem, no third-party driver.
#[cfg(windows)]
#[path = "mount_windows.rs"]
mod platform;
use crate::{
    disk::{encode_name, Entry, Result},
    store::Store,
};
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
#[cfg(windows)]
pub use platform::{available, free_drive, start, unmount};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::{
    collections::HashMap,
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tiny_http::{Header, Request, Response, Server, StatusCode};
type Shared = Arc<Mutex<Store>>;
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn encoded(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}
fn find(s: &Store, name: &str) -> Result<Entry> {
    s.disk
        .entries()?
        .into_iter()
        .find(|e| e.host_name().eq_ignore_ascii_case(name))
        .ok_or_else(|| "Not found".into())
}
fn name_type(name: &str) -> Result<(String, u8)> {
    let (n, e) = name.rsplit_once('.').ok_or("Use .prg, .seq or .usr")?;
    let kind = match e.to_ascii_lowercase().as_str() {
        "prg" => 2,
        "seq" => 1,
        "usr" => 3,
        _ => return Err("Unsupported file extension".into()),
    };
    encode_name(n, 16)?;
    Ok((n.to_ascii_uppercase(), kind))
}
fn metadata(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._")
}
fn header(req: &Request, key: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(key))
        .map(|h| h.value.to_string())
}
fn respond(req: Request, status: u16, body: Vec<u8>, headers: &[(&str, String)]) {
    let mut r = Response::from_data(body).with_status_code(StatusCode(status));
    for (k, v) in headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            r.add_header(h)
        }
    }
    let _ = req.respond(r);
}
fn status(e: &str) -> u16 {
    if e == "Not found" {
        404
    } else if e.contains("full") {
        507
    } else if e.contains("already exists") {
        412
    } else if e.contains("read-only") || e.contains("locked") {
        403
    } else {
        409
    }
}
struct Dav {
    store: Shared,
    base: String,
    image_path: std::path::PathBuf,
    readonly: bool,
    aux: HashMap<String, Vec<u8>>,
    locks: HashMap<String, (String, Instant)>,
}
impl Dav {
    fn decode(&self, url: &str) -> Result<String> {
        let path = if url.starts_with("http://") {
            url.splitn(4, '/')
                .nth(3)
                .map(|v| format!("/{v}"))
                .ok_or("Invalid URL")?
        } else {
            url.to_string()
        };
        let raw = if path == self.base.trim_end_matches('/') {
            ""
        } else {
            path.strip_prefix(&self.base).ok_or("Not found")?
        };
        let name = percent_decode_str(raw.trim_end_matches('/'))
            .decode_utf8()
            .map_err(|_| "Invalid filename")?
            .to_string();
        if name.contains('/') || name.contains('\0') || name == ".." {
            return Err("Not found".into());
        }
        Ok(name)
    }
    fn data(&self, name: &str) -> Result<Vec<u8>> {
        if let Some(b) = self.aux.get(name) {
            return Ok(b.clone());
        }
        let s = self.store.lock().map_err(|_| "Image unavailable")?;
        s.disk.read(&find(&s, name)?)
    }
    fn props(&self, name: &str, size: usize, dir: bool) -> String {
        let modified = std::fs::metadata(&self.image_path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let modified = httpdate::fmt_http_date(modified);
        let href = format!("{}{}", self.base, encoded(name));
        let res = if dir { "<d:collection/>" } else { "" };
        let lock=self.locks.get(name).map(|(t,_)|format!("<d:activelock><d:locktype><d:write/></d:locktype><d:lockscope><d:exclusive/></d:lockscope><d:depth>0</d:depth><d:timeout>Second-600</d:timeout><d:locktoken><d:href>{t}</d:href></d:locktoken></d:activelock>")).unwrap_or_default();
        format!("<d:response><d:href>{}</d:href><d:propstat><d:prop><d:displayname>{}</d:displayname><d:resourcetype>{res}</d:resourcetype><d:getcontentlength>{size}</d:getcontentlength><d:getcontenttype>application/octet-stream</d:getcontenttype><d:getlastmodified>{modified}</d:getlastmodified><d:supportedlock><d:lockentry><d:lockscope><d:exclusive/></d:lockscope><d:locktype><d:write/></d:locktype></d:lockentry></d:supportedlock><d:lockdiscovery>{lock}</d:lockdiscovery></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",xml(&href),xml(if dir{"MEGA65 DiskLab"}else{name}))
    }
    fn serve(&mut self, mut req: Request) {
        if std::env::var_os("D65_DAV_TRACE").is_some() {
            eprintln!("DAV {} {}", req.method(), req.url());
        }
        // Windows probes server capabilities before it visits the secret
        // collection. Reveal capabilities only, never the token or file list.
        if req.method().as_str() == "OPTIONS" && req.url() == "/" {
            respond(req, 200, vec![], &[("DAV", "1, 2".into()), ("MS-Author-Via", "DAV".into()), ("Allow", "OPTIONS, PROPFIND, GET, HEAD, PUT, DELETE, MOVE, COPY, LOCK, UNLOCK, PROPPATCH".into())]);
            return;
        }
        let name = match self.decode(req.url()) {
            Ok(n) => n,
            Err(_) => {
                respond(req, 404, vec![], &[]);
                return;
            }
        };
        let method = req.method().as_str().to_string();
        let mut body = Vec::new();
        if req
            .as_reader()
            .take(1_048_577)
            .read_to_end(&mut body)
            .is_err()
            || body.len() > 1_048_576
        {
            respond(req, 413, vec![], &[]);
            return;
        }
        let mut headers = vec![
            ("DAV", "1, 2".into()),
            ("MS-Author-Via", "DAV".into()),
            ("Cache-Control", "no-store".into()),
        ];
        let result: Result<(u16, Vec<u8>)> = (|| match method.as_str() {
            "OPTIONS" => {
                headers.push(("Allow","OPTIONS, PROPFIND, GET, HEAD, PUT, DELETE, MOVE, COPY, LOCK, UNLOCK, PROPPATCH".into()));
                Ok((200, vec![]))
            }
            "PROPFIND" => {
                let dir = name.is_empty();
                let size = if dir { 0 } else { self.data(&name)?.len() };
                let mut text = self.props(&name, size, dir);
                if dir && header(&req, "Depth").as_deref() != Some("0") {
                    let s = self.store.lock().map_err(|_| "Image unavailable")?;
                    for e in s.disk.entries()? {
                        let data = s.disk.read(&e)?;
                        text += &self.props(&e.host_name(), data.len(), false);
                    }
                    for (n, b) in &self.aux {
                        text += &self.props(n, b.len(), false);
                    }
                }
                headers.push(("Content-Type", "application/xml; charset=utf-8".into()));
                Ok((207,format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><d:multistatus xmlns:d=\"DAV:\">{text}</d:multistatus>").into_bytes()))
            }
            "GET" | "HEAD" => {
                if name.is_empty() {
                    return Ok((405, vec![]));
                }
                let data = self.data(&name)?;
                headers.push(("Content-Type", "application/octet-stream".into()));
                headers.push(("Accept-Ranges", "bytes".into()));
                if let Some(range) = header(&req, "Range") {
                    if let Some(r) = range.strip_prefix("bytes=") {
                        let (a, b) = r.split_once('-').ok_or("Invalid range")?;
                        let start = a.parse::<usize>().map_err(|_| "Invalid range")?;
                        let end = if b.is_empty() {
                            data.len().saturating_sub(1)
                        } else {
                            b.parse::<usize>().map_err(|_| "Invalid range")?
                        };
                        if start >= data.len() || end < start {
                            return Ok((416, vec![]));
                        }
                        let end = end.min(data.len() - 1);
                        headers.push((
                            "Content-Range",
                            format!("bytes {start}-{end}/{}", data.len()),
                        ));
                        return Ok((206, data[start..=end].to_vec()));
                    }
                }
                Ok((200, data))
            }
            "PUT" => {
                if self.readonly {
                    return Err("Image is read-only".into());
                }
                if metadata(&name) {
                    if self.aux.len() > 128 {
                        return Err("Metadata cache full".into());
                    }
                    self.aux.insert(name.clone(), body.clone());
                    return Ok((201, vec![]));
                }
                let (n, k) = name_type(&name)?;
                let mut s = self.store.lock().map_err(|_| "Image unavailable")?;
                let exists = find(&s, &name).is_ok();
                if header(&req, "If-None-Match").as_deref() == Some("*") && exists {
                    return Ok((412, vec![]));
                }
                s.change(|d| d.put(&n, k, &body, exists))?;
                Ok((if exists { 204 } else { 201 }, vec![]))
            }
            "DELETE" => {
                if self.readonly {
                    return Err("Image is read-only".into());
                }
                if self.aux.remove(&name).is_some() {
                    return Ok((204, vec![]));
                }
                let mut s = self.store.lock().map_err(|_| "Image unavailable")?;
                let e = find(&s, &name)?;
                s.change(|d| d.remove(&e))?;
                Ok((204, vec![]))
            }
            "MOVE" | "COPY" => {
                if self.readonly {
                    return Err("Image is read-only".into());
                }
                let dest =
                    self.decode(&header(&req, "Destination").ok_or("Missing destination")?)?;
                let overwrite = header(&req, "Overwrite").as_deref() != Some("F");
                if metadata(&name) && metadata(&dest) {
                    let data = self.aux.get(&name).ok_or("Not found")?.clone();
                    self.aux.insert(dest, data);
                    if method == "MOVE" {
                        self.aux.remove(&name);
                    }
                    return Ok((201, vec![]));
                }
                let (n, k) = name_type(&dest)?;
                let mut s = self.store.lock().map_err(|_| "Image unavailable")?;
                let source = find(&s, &name)?;
                let target = find(&s, &dest).ok();
                if target.as_ref().is_some_and(|t| t.offset == source.offset) {
                    return Ok((204, vec![]));
                }
                if target.is_some() && !overwrite {
                    return Ok((412, vec![]));
                }
                let data = s.disk.read(&source)?;
                s.change(|d| {
                    if let Some(t) = &target {
                        d.remove(t)?;
                    }
                    if method == "MOVE" {
                        d.remove(&source)?;
                    }
                    d.put(&n, k, &data, false)?;
                    Ok(())
                })?;
                Ok((201, vec![]))
            }
            "LOCK" => {
                if self.readonly {
                    return Err("Image is read-only".into());
                }
                self.locks
                    .retain(|_, (_, t)| t.elapsed() < Duration::from_secs(600));
                let refresh = body.is_empty();
                let token = if refresh {
                    self.locks
                        .get(&name)
                        .map(|(v, _)| v.clone())
                        .ok_or("No lock to refresh")?
                } else {
                    if self.locks.contains_key(&name) {
                        return Ok((423, vec![]));
                    }
                    format!("opaquelocktoken:{}", uuid::Uuid::new_v4())
                };
                self.locks
                    .insert(name.clone(), (token.clone(), Instant::now()));
                headers.push(("Lock-Token", format!("<{token}>")));
                headers.push(("Content-Type", "application/xml; charset=utf-8".into()));
                Ok((200,format!("<?xml version=\"1.0\"?><d:prop xmlns:d=\"DAV:\"><d:lockdiscovery><d:activelock><d:locktype><d:write/></d:locktype><d:lockscope><d:exclusive/></d:lockscope><d:depth>0</d:depth><d:timeout>Second-600</d:timeout><d:locktoken><d:href>{token}</d:href></d:locktoken><d:lockroot><d:href>{}{}</d:href></d:lockroot></d:activelock></d:lockdiscovery></d:prop>",self.base,encoded(&name)).into_bytes()))
            }
            "UNLOCK" => {
                let supplied = header(&req, "Lock-Token").unwrap_or_default();
                if self
                    .locks
                    .get(&name)
                    .is_some_and(|(v, _)| supplied == format!("<{v}>"))
                {
                    self.locks.remove(&name);
                    Ok((204, vec![]))
                } else {
                    Ok((409, vec![]))
                }
            }
            "PROPPATCH" => {
                headers.push(("Content-Type", "application/xml; charset=utf-8".into()));
                Ok((
                    207,
                    format!(
                        "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">{}</d:multistatus>",
                        self.props(&name, 0, false)
                    )
                    .into_bytes(),
                ))
            }
            _ => Ok((405, vec![])),
        })();
        match result {
            Ok((status, bytes)) => respond(req, status, bytes, &headers),
            Err(e) => respond(req, status(&e), e.into_bytes(), &headers),
        }
    }
}
pub struct Bridge {
    pub url: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
pub fn bridge(store: Shared, readonly: bool) -> Result<Bridge> {
    {
        let s = store.lock().map_err(|_| "Image unavailable")?;
        if readonly {
            let mut names = std::collections::HashSet::new();
            for e in s.disk.entries()? {
                s.disk.read(&e)?;
                if !names.insert(e.host_name().to_ascii_lowercase()) {
                    return Err(
                        "Ambiguous Finder filenames; extract these files in the app.".into(),
                    );
                }
            }
        }
        if !readonly {
            s.disk.validate_writable()?;
            for e in s.disk.entries()? {
                let normalized = encode_name(&e.name, 16)?;
                if normalized.as_slice()
                    != e.raw_name
                        .iter()
                        .copied()
                        .take_while(|b| *b != 0xa0)
                        .collect::<Vec<_>>()
                {
                    return Err("Use read-only mode for non-ASCII filenames.".into());
                }
            }
        }
    }
    let image_path = store.lock().map_err(|_| "Image unavailable")?.path.clone();
    let server = Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let addr = server.server_addr();
    let base = format!("/{}/", uuid::Uuid::new_v4());
    let url = format!("http://{addr}{base}");
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        let mut dav = Dav {
            store,
            base,
            image_path,
            readonly,
            aux: HashMap::new(),
            locks: HashMap::new(),
        };
        while !flag.load(Ordering::SeqCst) {
            if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(100)) {
                dav.serve(req)
            }
        }
    });
    Ok(Bridge {
        url,
        stop,
        thread: Some(thread),
    })
}
#[cfg(target_os = "macos")]
pub fn available() -> Result<()> {
    if Path::new("/sbin/mount_webdav").exists() {
        Ok(())
    } else {
        Err("The built-in macOS WebDAV filesystem is unavailable.".into())
    }
}
/// A fresh, empty mount point made by MEGA65 DiskLab, so mounting works like a
/// disk image: no folder to choose. mount_webdav needs an existing directory.
#[cfg(target_os = "macos")]
pub fn new_mountpoint(label: &str) -> Result<std::path::PathBuf> {
    let name: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let p = std::env::temp_dir().join("MEGA65 DiskLab").join(format!(
        "{} {}",
        name.trim(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
    Ok(p)
}
/// Remove a mount point made by `new_mountpoint` once the volume is gone.
#[cfg(target_os = "macos")]
pub fn remove_mountpoint(p: &Path) {
    if p.starts_with(std::env::temp_dir().join("MEGA65 DiskLab")) {
        let _ = std::fs::remove_dir(p);
    }
}
#[cfg(target_os = "macos")]
pub fn start(store: Shared, mountpoint: &Path, readonly: bool) -> Result<JoinHandle<Result<()>>> {
    available()?;
    if !mountpoint.is_dir()
        || std::fs::read_dir(mountpoint)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err("Choose an empty folder as the mount point.".into());
    }
    // The mount table lists resolved paths (/private/var, not /var).
    let p = std::fs::canonicalize(mountpoint).map_err(|e| e.to_string())?;
    let bridge = bridge(store, readonly)?;
    Ok(std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("/sbin/mount_webdav");
        cmd.arg("-S").arg("-v").arg("MEGA65 DiskLab");
        if readonly {
            cmd.arg("-o").arg("rdonly");
        }
        let out = cmd
            .arg(&bridge.url)
            .arg(&p)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "Finder mount failed: {} {}",
                String::from_utf8_lossy(&out.stderr),
                out.status
            ));
        }
        let _ = std::process::Command::new("/usr/bin/open").arg(&p).spawn();
        loop {
            let mounts = std::process::Command::new("/sbin/mount")
                .output()
                .map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&mounts.stdout);
            if !text.contains(&format!(" on {} (webdav", p.display())) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        drop(bridge);
        Ok(())
    }))
}
#[cfg(target_os = "macos")]
pub fn unmount(path: &Path) -> Result<()> {
    let out = std::process::Command::new("/usr/sbin/diskutil")
        .arg("unmount")
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}
