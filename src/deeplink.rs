//! Jam deep links: `ytm-rs://jam/CODE` and `https://grapedjuice.github.io/ytm-rs/jam/?c=CODE`.
//!
//! Clicking a link launches a new process with the URL as its argument. If another
//! instance is already running, the new one hands the code over a named pipe and
//! exits; the running instance queues it here and wakes the UI, which drains the
//! queue with `take()` (the same pattern as `tray.rs`).

use std::sync::Mutex;

use crate::jam::normalize_code as normalize;

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// A plain later launch asked for the window (see `forward_show`).
static SHOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// What a plain later launch sends instead of a code.
#[cfg_attr(not(windows), allow(dead_code))]
const SHOW_MSG: &str = "show";
/// The updater's relaunch passes this: the old copy may still be exiting, so don't hand over to it.
pub const RELAUNCH_FLAG: &str = "--relaunch";

/// The room code in a launch argument, if it is a jam link:
/// `ytm-rs://jam/CODE[/]` (any case) or the https share link with `?c=CODE`.
pub fn parse(arg: &str) -> Option<String> {
    let arg = arg.trim().trim_matches('"');
    let (scheme, rest) = arg.split_once("://")?;
    let rest = rest.split('#').next().unwrap_or("");
    if scheme.eq_ignore_ascii_case("ytm-rs") {
        // `jam/CODE`, `jam/CODE/`, or `jam/?c=CODE`.
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let path = path.trim_end_matches('/');
        let (host, code) = path.split_once('/').unwrap_or((path, ""));
        if !host.eq_ignore_ascii_case("jam") {
            return None;
        }
        if !code.is_empty() {
            return normalize(code);
        }
        return query_param(query, "c").and_then(normalize);
    }
    if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") {
        let (path, query) = rest.split_once('?')?;
        let lower = path.to_ascii_lowercase();
        let path = lower.trim_end_matches('/').trim_end_matches("/index.html");
        if path != "grapedjuice.github.io/ytm-rs/jam" {
            return None;
        }
        return query_param(query, "c").and_then(normalize);
    }
    None
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == name).then_some(v)
    })
}

/// The first jam link among the process arguments.
pub fn from_args() -> Option<String> {
    std::env::args_os().skip(1).find_map(|a| parse(&a.to_string_lossy()))
}

/// Queue a code for the UI (e.g. from the launch arguments).
pub fn push(code: String) {
    PENDING.lock().unwrap().push(code);
}

/// Codes received since the last call, oldest first.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// Whether a later launch asked for the window since the last call.
pub fn take_show() -> bool {
    SHOW.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// A plain launch while another copy runs: bring that one's window up instead, so only
/// one copy uses (and refreshes) the Google session. Two copies refreshing it apart
/// leave one with old cookies, which Google treats as stolen and signs out. True if
/// a running copy took it.
pub fn forward_show() -> bool {
    if std::env::args_os().any(|a| a == RELAUNCH_FLAG) {
        return false;
    }
    // Windows only lets a background process take the foreground when the one the user
    // just started allows it.
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    forward_to_running(SHOW_MSG)
}

/// Per-user pipe name. A dev profile (`YTM_DATA_DIR`) gets its own, so test runs
/// never talk to the installed copy.
#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_name() -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let mut name = format!(r"\\.\pipe\ytm-rs-jam-{user}");
    if let Some(dir) = std::env::var_os("YTM_DATA_DIR") {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dir.hash(&mut h);
        name.push_str(&format!("-{:016x}", h.finish()));
    }
    name
}

#[cfg(windows)]
pub use win::{forward_to_running, listen, register_protocol};

#[cfg(not(windows))]
pub fn forward_to_running(_code: &str) -> bool {
    false
}

#[cfg(not(windows))]
pub fn listen(_wake: impl Fn() + Send + 'static) {}

#[cfg(not(windows))]
pub fn register_protocol() {}

#[cfg(windows)]
mod win {
    use std::io::Write;

    use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_WAIT,
    };
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::core::HSTRING;

    use super::{normalize, pipe_name, push};

    /// Hand `code` to a running instance. True if one took it.
    pub fn forward_to_running(code: &str) -> bool {
        let name = pipe_name();
        // The server has one pipe instance; between clients it is briefly busy.
        for _ in 0..20 {
            match std::fs::OpenOptions::new().write(true).open(&name) {
                Ok(mut pipe) => {
                    return pipe.write_all(format!("{code}\n").as_bytes()).and_then(|_| pipe.flush()).is_ok();
                }
                // ERROR_PIPE_BUSY: another client is mid-handoff.
                Err(e) if e.raw_os_error() == Some(231) => std::thread::sleep(std::time::Duration::from_millis(50)),
                // No server (ERROR_FILE_NOT_FOUND) or anything else: nobody to hand to.
                Err(_) => return false,
            }
        }
        false
    }

    struct Pipe(HANDLE);
    // SAFETY: a pipe handle may be used from any thread.
    unsafe impl Send for Pipe {}
    impl Drop for Pipe {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Serve the pipe on a background thread: each code received is queued and
    /// `wake` called. Only the first instance gets the pipe; any later one (a plain
    /// second launch) does nothing here.
    pub fn listen(wake: impl Fn() + Send + 'static) {
        let name = pipe_name();
        let handle = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(name.as_str()),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                512,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            // ERROR_ACCESS_DENIED here means another instance already serves it.
            log::info!("jam link pipe not served by this instance: {}", windows::core::Error::from_thread());
            return;
        }
        let pipe = Pipe(handle);
        let spawned = std::thread::Builder::new().name("deeplink".into()).spawn(move || serve(pipe, wake));
        if let Err(e) = spawned {
            log::warn!("jam link listener: {e}");
        }
    }

    fn serve(pipe: Pipe, wake: impl Fn()) {
        loop {
            let connected = unsafe { ConnectNamedPipe(pipe.0, None) };
            if let Err(e) = connected {
                // A client that connected between create/disconnect and this call.
                if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                    log::warn!("jam link pipe: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
            }
            let mut msg = Vec::new();
            let mut buf = [0u8; 64];
            loop {
                let mut n = 0u32;
                let ok = unsafe { ReadFile(pipe.0, Some(&mut buf), Some(&mut n), None) };
                if ok.is_err() || n == 0 {
                    break; // ERROR_BROKEN_PIPE: the client wrote and closed.
                }
                msg.extend_from_slice(&buf[..n as usize]);
                if msg.len() > 256 {
                    break;
                }
            }
            unsafe {
                let _ = DisconnectNamedPipe(pipe.0);
            }
            if String::from_utf8_lossy(&msg).trim() == super::SHOW_MSG {
                log::info!("another launch asked for the window");
                super::SHOW.store(true, std::sync::atomic::Ordering::Relaxed);
                wake();
                continue;
            }
            match normalize(String::from_utf8_lossy(&msg).trim()) {
                Some(code) => {
                    log::info!("jam link received from another launch: {code}");
                    push(code);
                    wake();
                }
                None => log::warn!("jam link pipe: ignored an invalid message"),
            }
        }
    }

    /// Register the `ytm-rs:` URL protocol for this user, pointing at this exe.
    /// Call only from an installed copy, so dev builds don't take the protocol over.
    pub fn register_protocol() {
        match std::env::current_exe() {
            Ok(exe) => {
                if let Err(e) = register_at(HKEY_CURRENT_USER, r"Software\Classes\ytm-rs", &exe.to_string_lossy()) {
                    log::warn!("registering the ytm-rs: protocol: {e}");
                }
            }
            Err(e) => log::warn!("registering the ytm-rs: protocol: {e}"),
        }
    }

    pub(super) fn register_at(root: HKEY, key: &str, exe: &str) -> windows::core::Result<()> {
        let set = |sub: &str, name: &str, value: &str| -> windows::core::Result<()> {
            let path = if sub.is_empty() { key.to_string() } else { format!(r"{key}\{sub}") };
            let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
            let name = HSTRING::from(name);
            unsafe {
                RegSetKeyValueW(
                    root,
                    &HSTRING::from(path),
                    if name.is_empty() { windows::core::PCWSTR::null() } else { windows::core::PCWSTR(name.as_ptr()) },
                    REG_SZ.0,
                    Some(data.as_ptr().cast()),
                    (data.len() * 2) as u32,
                )
            }
            .ok()
        };
        set("", "", "URL:ytm-rs Protocol")?;
        set("", "URL Protocol", "")?;
        set("DefaultIcon", "", &format!("\"{exe}\",0"))?;
        set(r"shell\open\command", "", &format!("\"{exe}\" \"%1\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_protocol() {
        assert_eq!(parse("ytm-rs://jam/ABC234").as_deref(), Some("ABC234"));
        assert_eq!(parse("ytm-rs://jam/abc234/").as_deref(), Some("ABC234"));
        assert_eq!(parse("YTM-RS://JAM/aBc234").as_deref(), Some("ABC234"));
        assert_eq!(parse("\"ytm-rs://jam/ABC234\"").as_deref(), Some("ABC234"));
        assert_eq!(parse("ytm-rs://jam/?c=xyz789").as_deref(), Some("XYZ789"));
    }

    #[test]
    fn share_link() {
        assert_eq!(parse("https://grapedjuice.github.io/ytm-rs/jam/?c=ABC234").as_deref(), Some("ABC234"));
        assert_eq!(parse("https://grapedjuice.github.io/ytm-rs/jam?c=abc234").as_deref(), Some("ABC234"));
        assert_eq!(parse("https://grapedjuice.github.io/ytm-rs/jam/index.html?x=1&c=abc234#top").as_deref(), Some("ABC234"));
    }

    #[test]
    fn rejects() {
        for bad in [
            "",
            "%1",
            "ABC234",
            "ytm-rs://jam/",
            "ytm-rs://jam/ABC23",      // too short
            "ytm-rs://jam/ABC2345",    // too long
            "ytm-rs://jam/ABC0I1",     // 0, I, 1 are not in the alphabet
            "ytm-rs://jam/ABC234/xyz", // extra path
            "ytm-rs://room/ABC234",
            "https://example.com/ytm-rs/jam/?c=ABC234",
            "https://grapedjuice.github.io/ytm-rs/?c=ABC234",
            "https://grapedjuice.github.io/ytm-rs/jam/",
            "--login",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn queue() {
        push("ABC234".into());
        push("XYZ789".into());
        assert_eq!(take(), ["ABC234", "XYZ789"]);
        assert!(take().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn register_under_scratch_key() {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW};
        use windows::core::HSTRING;
        let key = format!(r"Software\ytm-rs-deeplink-test-{}", std::process::id());
        win::register_at(HKEY_CURRENT_USER, &key, r"C:\x\ytm-rs.exe").unwrap();
        let read = |sub: &str, name: &str| -> String {
            let mut buf = [0u16; 256];
            let mut len = (buf.len() * 2) as u32;
            let path = if sub.is_empty() { key.clone() } else { format!(r"{key}\{sub}") };
            let r = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(path),
                    &HSTRING::from(name),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr().cast()),
                    Some(&mut len),
                )
            };
            assert!(r.is_ok(), "{sub}\\{name}: {r:?}");
            String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)])
        };
        let results = (read("", ""), read("", "URL Protocol"), read("DefaultIcon", ""), read(r"shell\open\command", ""));
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key.as_str()));
            let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key.as_str()));
        }
        assert_eq!(results.0, "URL:ytm-rs Protocol");
        assert_eq!(results.1, "");
        assert_eq!(results.2, r#""C:\x\ytm-rs.exe",0"#);
        assert_eq!(results.3, r#""C:\x\ytm-rs.exe" "%1""#);
    }
}
