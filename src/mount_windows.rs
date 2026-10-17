//! Built-in WebClient redirector. No credentials, registry changes or drivers.
use super::{bridge, Shared};
use crate::{
    disk::Result,
    windows::{error, wide},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    thread::JoinHandle,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*, NetworkManagement::WNet::*, Storage::FileSystem::GetLogicalDrives,
    System::Services::*,
};

struct Service(SC_HANDLE);
fn owned() -> &'static Mutex<HashMap<String, String>> {
    static MAPPINGS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAPPINGS.get_or_init(|| Mutex::new(HashMap::new()))
}
impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
fn service_error(code: u32) -> String {
    match code {
        ERROR_SERVICE_DOES_NOT_EXIST => "Windows WebClient is not installed or is unavailable in this Windows edition. Explorer mounting requires the built-in WebDAV redirector. Use the app's import/extract controls instead; no third-party driver is required for editing.".into(),
        ERROR_SERVICE_DISABLED => "Windows WebClient is disabled. Explorer mounting is unavailable until your administrator enables the built-in WebClient service. MEGA65 DiskLab does not change service or security settings. Import/extract still works.".into(),
        _ => format!("Cannot inspect Windows WebClient: {}. Explorer mounting requires the built-in WebClient service. Import/extract remains available.", std::io::Error::from_raw_os_error(code as i32)),
    }
}
pub fn available() -> Result<()> {
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return Err(service_error(GetLastError()));
        }
        let manager = Service(manager);
        let service = OpenServiceW(
            manager.0,
            wide("WebClient").as_ptr(),
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        );
        if service.is_null() {
            return Err(service_error(GetLastError()));
        }
        let service = Service(service);
        // u64 storage keeps QUERY_SERVICE_CONFIGW correctly aligned.
        let mut buffer = [0u64; 1024];
        let mut needed = 0;
        if QueryServiceConfigW(
            service.0,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as u32,
            &mut needed,
        ) == 0
        {
            return Err(service_error(GetLastError()));
        }
        let config = &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>();
        if config.dwStartType == SERVICE_DISABLED {
            return Err(service_error(ERROR_SERVICE_DISABLED));
        }
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        if QueryServiceStatus(service.0, &mut status) == 0 {
            return Err(service_error(GetLastError()));
        }
        // Manual/trigger-start is normal. WNet may start it on demand. If policy
        // prevents that, the actual mapping error tells the user what to do.
    }
    Ok(())
}
fn letter(path: &Path) -> Result<String> {
    let s = path.to_str().ok_or("Invalid drive letter")?;
    let s = s.trim_end_matches(['\\', '/']);
    let b = s.as_bytes();
    if b.len() != 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return Err("Choose a free drive letter, for example R:".into());
    }
    Ok(s.to_ascii_uppercase())
}
fn connection(drive: &str) -> Result<Option<String>> {
    let mut buffer = [0u16; 2048];
    let mut size = buffer.len() as u32;
    let code = unsafe { WNetGetConnectionW(wide(drive).as_ptr(), buffer.as_mut_ptr(), &mut size) };
    if code == ERROR_NOT_CONNECTED {
        return Ok(None);
    }
    if code != NO_ERROR {
        return Err(format!(
            "Cannot inspect drive mapping: {}",
            std::io::Error::from_raw_os_error(code as i32)
        ));
    }
    Ok(Some(String::from_utf16_lossy(
        &buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
    )))
}
pub fn free_drive() -> Result<PathBuf> {
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(error("Cannot enumerate drive letters"));
    }
    for ch in (b'D'..=b'Z').rev() {
        let drive = format!("{}:", ch as char);
        if mask & (1 << (ch - b'A')) == 0 && connection(&drive)?.is_none() {
            return Ok(PathBuf::from(drive));
        }
    }
    Err("No unused drive letter is available.".into())
}
fn unc(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .ok_or("Bridge is not loopback")?;
    let (port, token) = rest.split_once('/').ok_or("Invalid bridge URL")?;
    let port = port.parse::<u16>().map_err(|_| "Invalid bridge port")?;
    let token = token.trim_end_matches('/');
    uuid::Uuid::parse_str(token).map_err(|_| "Invalid bridge token")?;
    Ok(format!("\\\\127.0.0.1@{port}\\DavWWWRoot\\{token}"))
}
pub fn start(store: Shared, mountpoint: &Path, readonly: bool) -> Result<JoinHandle<Result<()>>> {
    available()?;
    let drive = letter(mountpoint)?;
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 || mask & (1 << (drive.as_bytes()[0] - b'A')) != 0 || connection(&drive)?.is_some()
    {
        return Err("The selected drive letter is in use. Choose another.".into());
    }
    let bridge = bridge(store, readonly)?;
    let remote = unc(&bridge.url)?;
    Ok(std::thread::spawn(move || {
        let mut local = wide(&drive);
        let mut remote_w = wide(&remote);
        // Ask MPR for the installed provider name; display names vary by OS
        // language. WNNC_NET_DAV is 0x002e0000 (winnetwk.h).
        let mut provider = [0u16; 1024];
        let mut provider_len = provider.len() as u32;
        let provider_status =
            unsafe { WNetGetProviderNameW(0x002e0000, provider.as_mut_ptr(), &mut provider_len) };
        if provider_status != NO_ERROR {
            return Err(format!("The built-in Windows WebDAV network provider is unavailable (code {provider_status}). Use import/extract in the app."));
        }
        let resource = NETRESOURCEW {
            dwType: RESOURCETYPE_DISK,
            lpLocalName: local.as_mut_ptr(),
            lpRemoteName: remote_w.as_mut_ptr(),
            lpProvider: provider.as_mut_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        let code = unsafe {
            WNetAddConnection2W(
                &resource,
                std::ptr::null(),
                std::ptr::null(),
                CONNECT_TEMPORARY,
            )
        };
        if code != NO_ERROR {
            return Err(format!("Explorer mount failed: {} (code {code}). Windows WebClient must be available and running, and local WebDAV must be permitted by Windows policy. Ask your administrator to start WebClient if it is stopped. MEGA65 DiskLab does not install drivers or change authentication/registry policy. Use import/extract in the app instead.", std::io::Error::from_raw_os_error(code as i32)));
        }
        owned()
            .lock()
            .unwrap()
            .insert(drive.clone(), remote.clone());
        loop {
            match connection(&drive) {
                Ok(Some(current)) if current.eq_ignore_ascii_case(&remote) => {}
                Ok(_) => break,
                // Do not release the image's writer lock on a transient query
                // failure while Explorer might still have this mapping.
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        owned().lock().unwrap().remove(&drive);
        drop(bridge);
        Ok(())
    }))
}
pub fn unmount(path: &Path) -> Result<()> {
    let drive = letter(path)?;
    let Some(remote) = connection(&drive)? else {
        return Ok(());
    };
    if !owned()
        .lock()
        .unwrap()
        .get(&drive)
        .is_some_and(|expected| expected.eq_ignore_ascii_case(&remote))
    {
        return Err("This process does not own that drive mapping, or it is still connecting. Refusing to disconnect it.".into());
    }
    let code = unsafe { WNetCancelConnection2W(wide(&drive).as_ptr(), 0, 0) };
    if code == NO_ERROR {
        Ok(())
    } else {
        Err(format!(
            "Close files and Explorer windows on the mapped drive, then retry eject: {}",
            std::io::Error::from_raw_os_error(code as i32)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drive_and_url_validation() {
        assert_eq!(letter(Path::new("r:\\")).unwrap(), "R:");
        for s in ["C:\\folder", "\\\\server\\share", "R:foo"] {
            assert!(letter(Path::new(s)).is_err());
        }
        assert_eq!(
            unc("http://127.0.0.1:1234/12345678-1234-1234-1234-123456789abc/").unwrap(),
            "\\\\127.0.0.1@1234\\DavWWWRoot\\12345678-1234-1234-1234-123456789abc"
        );
        assert!(unc("http://example.com:80/foo/").is_err());
        assert!(service_error(ERROR_SERVICE_DISABLED).contains("disabled"));
        assert!(service_error(ERROR_SERVICE_DOES_NOT_EXIST).contains("not installed"));
    }
}
