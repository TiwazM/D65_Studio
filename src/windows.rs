//! Small native Windows helpers; no shell scripts or external runtime.
use crate::disk::Result;
use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
use windows_sys::Win32::{
    Foundation::*,
    System::Threading::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};

pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub fn error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}
// CommandLineToArgvW / Rust CRT quoting, without a command interpreter.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in s.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}
pub fn elevated(args: &[String], mut poll: impl FnMut()) -> Result<()> {
    let exe = wide(std::env::current_exe().map_err(|e| e.to_string())?);
    let verb = wide("runas");
    let parameters = wide(args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" "));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of_val(&info) as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            return Err(error("Administrator authorization cancelled or failed"));
        }
        let wait = loop {
            poll();
            let result = WaitForSingleObject(info.hProcess, 200);
            if result != WAIT_TIMEOUT {
                break result;
            }
        };
        let mut code = 1;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if wait != WAIT_OBJECT_0 || got == 0 {
            return Err(error("Cannot wait for SD export helper"));
        }
        if code != 0 {
            return Err("SD export failed. The administrator helper displayed details and retained any recovery records. No success is claimed.".into());
        }
    }
    Ok(())
}

// One atomic word in a random, session-local named mapping. It carries only
// progress, never file paths or write commands, across the UAC process boundary.
pub struct ExportProgress {
    handle: HANDLE,
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    pub name: String,
}
impl ExportProgress {
    pub fn create() -> Result<Self> {
        use windows_sys::Win32::System::Memory::*;
        let name = format!("Local\\D65Progress-{}", uuid::Uuid::new_v4());
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                4,
                wide(&name).as_ptr(),
            )
        };
        Self::map(handle, name)
    }
    pub fn open(name: &str) -> Result<Self> {
        use windows_sys::Win32::System::Memory::*;
        let token = name
            .strip_prefix("Local\\D65Progress-")
            .ok_or("Invalid progress channel")?;
        uuid::Uuid::parse_str(token).map_err(|_| "Invalid progress channel")?;
        Self::map(
            unsafe { OpenFileMappingW(FILE_MAP_WRITE | FILE_MAP_READ, 0, wide(name).as_ptr()) },
            name.into(),
        )
    }
    fn map(handle: HANDLE, name: String) -> Result<Self> {
        use windows_sys::Win32::System::Memory::*;
        if handle.is_null() {
            return Err(error("Cannot open export progress channel"));
        }
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 4) };
        if view.Value.is_null() {
            unsafe {
                CloseHandle(handle);
            }
            return Err(error("Cannot map export progress channel"));
        }
        Ok(Self { handle, view, name })
    }
    fn word(&self) -> &std::sync::atomic::AtomicU32 {
        unsafe { &*self.view.Value.cast() }
    }
    pub fn set(&self, phase: u32, done: u64, total: u64) {
        let percent = if total == 0 {
            101
        } else {
            done.saturating_mul(100)
                .checked_div(total)
                .unwrap_or(0)
                .min(100) as u32
        };
        self.word().store(
            (phase << 16) | percent,
            std::sync::atomic::Ordering::Release,
        );
    }
    pub fn message(&self) -> String {
        let value = self.word().load(std::sync::atomic::Ordering::Acquire);
        let phase = match value >> 16 {
            1 => "Checking both allocation tables",
            2 => "Checking existing file and directory chains",
            3 => "Saving recovery journal",
            4 => "Writing image in consecutive clusters",
            5 => "Reading back and comparing image bytes",
            6 => "Publishing and verifying filesystem metadata",
            7 => "Restoring original metadata after a failure",
            8 => "Verifying the remounted file",
            _ => "Requesting administrator access and locking the selected card",
        };
        let percent = value & 0xffff;
        if (1..=6).contains(&(value >> 16)) && percent <= 100 {
            format!("{phase} — {percent}%")
        } else {
            phase.into()
        }
    }
}
impl Drop for ExportProgress {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Memory::UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}
pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide("D65 Studio — SD export").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_mapping_shares_only_phase_and_percent() {
        let owner = ExportProgress::create().unwrap();
        let name = owner.name.clone();
        let peer = ExportProgress::open(&name).unwrap();
        assert!(owner.message().contains("administrator"));
        peer.set(4, 1, 4);
        assert_eq!(
            owner.message(),
            "Writing image in consecutive clusters — 25%"
        );
        peer.set(2, 17, 0);
        assert_eq!(
            owner.message(),
            "Checking existing file and directory chains"
        );
        peer.set(5, 200, 100);
        assert!(owner.message().ends_with("100%"));
        drop(peer);
        drop(owner);
        assert!(ExportProgress::open(&name).is_err());
        assert!(ExportProgress::open("Local\\unrelated").is_err());
    }
    #[test]
    fn quotes_arguments_without_shell_expansion() {
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("x\\"), "\"x\\\\\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("$()&%"), "\"$()&%\"");
    }
}
