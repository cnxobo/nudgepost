#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenLockState {
    Locked,
    Unlocked,
    Unknown(String),
}

pub trait ConditionEvaluator: Send + Sync {
    fn screen_locked(&self) -> ScreenLockState;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemConditionEvaluator;

impl ConditionEvaluator for SystemConditionEvaluator {
    fn screen_locked(&self) -> ScreenLockState {
        platform_screen_locked()
    }
}

#[cfg(windows)]
fn platform_screen_locked() -> ScreenLockState {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNKNOWN,
        WTS_SESSIONSTATE_UNLOCK, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSINFOEXW,
        WTSQuerySessionInformationW, WTSSessionInfoEx,
    };

    const NO_ACTIVE_CONSOLE_SESSION: u32 = u32::MAX;

    let session_id = unsafe { WTSGetActiveConsoleSessionId() };
    if session_id == NO_ACTIVE_CONSOLE_SESSION {
        return ScreenLockState::Unknown("no active console session".into());
    }

    let mut buffer = std::ptr::null_mut();
    let mut bytes_returned = 0u32;
    let ok = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session_id,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes_returned,
        )
    };
    if ok == 0 {
        return ScreenLockState::Unknown(format!(
            "WTSQuerySessionInformationW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let state = if buffer.is_null() || bytes_returned < std::mem::size_of::<WTSINFOEXW>() as u32 {
        ScreenLockState::Unknown("WTSSessionInfoEx returned incomplete data".into())
    } else {
        let info = unsafe { &*(buffer as *const WTSINFOEXW) };
        if info.Level != 1 {
            ScreenLockState::Unknown(format!("unsupported WTSSessionInfoEx level {}", info.Level))
        } else {
            let level1 = unsafe { info.Data.WTSInfoExLevel1 };
            match level1.SessionFlags as u32 {
                WTS_SESSIONSTATE_LOCK => ScreenLockState::Locked,
                WTS_SESSIONSTATE_UNLOCK => ScreenLockState::Unlocked,
                WTS_SESSIONSTATE_UNKNOWN => ScreenLockState::Unknown(
                    "WTSSessionInfoEx returned unknown session lock state".into(),
                ),
                other => ScreenLockState::Unknown(format!(
                    "WTSSessionInfoEx returned unexpected session flag {other}"
                )),
            }
        }
    };

    unsafe {
        WTSFreeMemory(buffer.cast());
    }
    state
}

#[cfg(target_os = "linux")]
fn platform_screen_locked() -> ScreenLockState {
    match linux_screen_locked() {
        Ok(state) => state,
        Err(reason) => ScreenLockState::Unknown(reason),
    }
}

#[cfg(target_os = "linux")]
fn linux_screen_locked() -> Result<ScreenLockState, String> {
    let sessions = run_command_with_timeout(
        "loginctl",
        &["list-sessions", "--no-legend", "--no-pager"],
        std::time::Duration::from_millis(750),
    )?;
    for session_id in sessions
        .lines()
        .filter_map(|line| line.split_whitespace().next())
    {
        let output = run_command_with_timeout(
            "loginctl",
            &[
                "show-session",
                session_id,
                "-p",
                "Type",
                "-p",
                "State",
                "-p",
                "LockedHint",
                "--no-pager",
            ],
            std::time::Duration::from_millis(750),
        )?;
        if linux_session_is_active_graphical(&output) {
            if let Some(state) = parse_locked_hint(
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("LockedHint="))
                    .unwrap_or_default(),
            ) {
                return Ok(state);
            }
        }
    }
    Err("no active graphical login session with readable LockedHint".into())
}

#[cfg(target_os = "linux")]
fn linux_session_is_active_graphical(output: &str) -> bool {
    let mut session_type = "";
    let mut state = "";
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("Type=") {
            session_type = value.trim();
        } else if let Some(value) = line.strip_prefix("State=") {
            state = value.trim();
        }
    }
    state == "active" && matches!(session_type, "x11" | "wayland" | "mir")
}

#[cfg(target_os = "linux")]
fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<String, String> {
    use std::{
        process::{Command, Stdio},
        time::Instant,
    };

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("{program} failed to start: {err}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|err| format!("{program} failed to read output: {err}"))?;
                if status.success() {
                    return String::from_utf8(output.stdout)
                        .map_err(|err| format!("{program} returned non-UTF-8 output: {err}"));
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("{program} exited with {status}: {}", stderr.trim()));
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} timed out after {} ms",
                    timeout.as_millis()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(err) => return Err(format!("{program} status check failed: {err}")),
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_screen_locked() -> ScreenLockState {
    match unsafe { macos_screen_locked() } {
        Ok(state) => state,
        Err(reason) => ScreenLockState::Unknown(reason),
    }
}

#[cfg(target_os = "macos")]
unsafe fn macos_screen_locked() -> Result<ScreenLockState, String> {
    use core_foundation_sys::{
        base::{CFRelease, CFTypeRef},
        boolean::{CFBooleanGetValue, CFBooleanRef},
        dictionary::{CFDictionaryGetValue, CFDictionaryRef},
        string::{CFStringCreateWithCString, kCFStringEncodingUTF8},
    };
    use std::{ffi::CString, os::raw::c_void, ptr};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    }

    let key_text = CString::new("CGSSessionScreenIsLocked")
        .map_err(|err| format!("failed to build macOS lock-state key: {err}"))?;
    let dictionary = unsafe { CGSessionCopyCurrentDictionary() };
    if dictionary.is_null() {
        return Err("CGSessionCopyCurrentDictionary returned null".into());
    }

    let key =
        unsafe { CFStringCreateWithCString(ptr::null(), key_text.as_ptr(), kCFStringEncodingUTF8) };
    if key.is_null() {
        unsafe {
            CFRelease(dictionary as CFTypeRef);
        }
        return Err("failed to create CGSSessionScreenIsLocked key".into());
    }

    let value = unsafe { CFDictionaryGetValue(dictionary, key as *const c_void) };
    let state = if value.is_null() {
        Err("CGSSessionScreenIsLocked is missing from session dictionary".into())
    } else if unsafe { CFBooleanGetValue(value as CFBooleanRef) } != 0 {
        Ok(ScreenLockState::Locked)
    } else {
        Ok(ScreenLockState::Unlocked)
    };

    unsafe {
        CFRelease(key as CFTypeRef);
        CFRelease(dictionary as CFTypeRef);
    }
    state
}

#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
fn platform_screen_locked() -> ScreenLockState {
    ScreenLockState::Unknown("screen_locked is not implemented on this platform".into())
}

#[cfg(any(test, target_os = "linux"))]
fn parse_locked_hint(value: &str) -> Option<ScreenLockState> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => Some(ScreenLockState::Locked),
        "no" | "false" | "0" => Some(ScreenLockState::Unlocked),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_locked_hint_values() {
        assert_eq!(parse_locked_hint("yes"), Some(ScreenLockState::Locked));
        assert_eq!(parse_locked_hint("true"), Some(ScreenLockState::Locked));
        assert_eq!(parse_locked_hint("1"), Some(ScreenLockState::Locked));
        assert_eq!(parse_locked_hint("no"), Some(ScreenLockState::Unlocked));
        assert_eq!(parse_locked_hint("false"), Some(ScreenLockState::Unlocked));
        assert_eq!(parse_locked_hint("0"), Some(ScreenLockState::Unlocked));
        assert_eq!(parse_locked_hint("maybe"), None);
        assert_eq!(parse_locked_hint(""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detects_active_graphical_linux_session() {
        assert!(linux_session_is_active_graphical(
            "Type=wayland\nState=active\nLockedHint=yes"
        ));
        assert!(!linux_session_is_active_graphical(
            "Type=tty\nState=active\nLockedHint=yes"
        ));
    }
}
