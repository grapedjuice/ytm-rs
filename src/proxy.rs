//! Settings › Network › Proxy. Everything ytm-rs fetches goes through one setting:
//! the API and song streams, lyrics, translation, updates, the Spotify transfer,
//! yt-dlp, the sign-in windows and Listen together.
//!
//! - System: whatever Windows is set to (Settings › Network › Proxy), else the
//!   `HTTPS_PROXY`/`ALL_PROXY`/`HTTP_PROXY` environment variables, else direct. On
//!   Linux and macOS, just those variables.
//! - Off: always direct.
//! - Custom: `http://host:port` or `socks5://host:port`, optionally `user:pass@`.
//!
//! Streams and the requests that resolve them must leave from the same address (a
//! stream URL is tied to the IP that asked for it), which one setting for everything
//! guarantees. The setting is read once at launch; a change applies after a restart.
//! The mode lives in `<data dir>/proxy`; a custom URL can carry a password, so it's
//! kept in the credential store.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Setting {
    #[default]
    System,
    Off,
    Custom(String),
}

impl Setting {
    pub fn label(&self) -> &'static str {
        match self {
            Setting::System if cfg!(windows) => "Use Windows settings",
            Setting::System => "Use system settings",
            Setting::Off => "No proxy",
            Setting::Custom(_) => "Custom",
        }
    }

    /// The proxy this setting sends traffic through right now, if any.
    pub fn resolve(&self) -> Option<url::Url> {
        match self {
            Setting::Off => None,
            Setting::Custom(u) => parse(u).ok(),
            Setting::System => system(),
        }
    }
}

/// Environment variable that hands the setting to child processes (sign-in windows).
const CHILD_ENV: &str = "YTM_PROXY_CHILD";
const VAULT: &str = "proxy";

static ACTIVE: OnceLock<Setting> = OnceLock::new();

/// Read the setting once at launch (child processes take their parent's).
pub fn init(data_dir: &Path) {
    let s = match std::env::var(CHILD_ENV) {
        Ok(v) if v == "off" => Setting::Off,
        Ok(v) if v == "system" => Setting::System,
        Ok(v) => Setting::Custom(v),
        Err(_) => load(data_dir),
    };
    if let Some(p) = s.resolve() {
        log::info!("proxy: {} ({})", s.label(), redact(&p));
    }
    let _ = ACTIVE.set(s);
}

/// The setting in effect for this run.
pub fn active() -> &'static Setting {
    ACTIVE.get_or_init(Setting::default)
}

pub fn load(data_dir: &Path) -> Setting {
    match std::fs::read_to_string(data_dir.join("proxy")).unwrap_or_default().trim() {
        "off" => Setting::Off,
        "custom" => crate::vault::get(VAULT, data_dir).and_then(|b| String::from_utf8(b).ok()).map_or(Setting::System, Setting::Custom),
        _ => Setting::System,
    }
}

pub fn save(data_dir: &Path, s: &Setting) -> anyhow::Result<()> {
    let mode = match s {
        Setting::System => "system",
        Setting::Off => "off",
        Setting::Custom(u) => {
            parse(u)?;
            crate::vault::set(VAULT, data_dir, u.trim().as_bytes())?;
            "custom"
        }
    };
    if !matches!(s, Setting::Custom(_)) {
        crate::vault::delete(VAULT, data_dir);
    }
    std::fs::write(data_dir.join("proxy"), mode)?;
    Ok(())
}

/// Check a custom proxy URL: scheme, host and port.
pub fn parse(s: &str) -> anyhow::Result<url::Url> {
    let s = s.trim();
    // "host:port" alone means an HTTP proxy, as Windows writes it.
    let full = if s.contains("://") { s.to_owned() } else { format!("http://{s}") };
    let u = url::Url::parse(&full).map_err(|_| anyhow::anyhow!("that isn't a proxy address, like http://127.0.0.1:8080"))?;
    anyhow::ensure!(matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h"), "use an http:// or socks5:// address");
    anyhow::ensure!(u.host_str().is_some_and(|h| !h.is_empty()), "the address needs a host");
    anyhow::ensure!(u.port_or_known_default().is_some(), "the address needs a port, like :8080");
    Ok(u)
}

/// The URL with any password hidden, for logs and the settings page.
pub fn redact(u: &url::Url) -> String {
    let host = u.host_str().unwrap_or_default();
    let port = u.port_or_known_default().map(|p| format!(":{p}")).unwrap_or_default();
    let user = match (u.username(), u.password()) {
        ("", _) => String::new(),
        (name, Some(_)) => format!("{}:•••@", percent(name)),
        (name, None) => format!("{}@", percent(name)),
    };
    format!("{}://{user}{host}{port}", u.scheme())
}

/// Route a client through the active setting.
pub fn apply(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    apply_with(b, active())
}

/// Tor gives each SOCKS username a circuit of its own, so a new name is a new exit.
/// 0 = no name (the proxy's own choice).
static CIRCUIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static HTTP: std::sync::RwLock<Option<reqwest::Client>> = std::sync::RwLock::new(None);

/// The client for YouTube Music and its streams. It's rebuilt when `new_circuit`
/// picks another exit: pooled connections would otherwise stay on the old one.
pub fn http() -> reqwest::Client {
    if let Some(c) = HTTP.read().unwrap().as_ref() {
        return c.clone();
    }
    HTTP.write().unwrap().get_or_insert_with(|| build_for(CIRCUIT.load(std::sync::atomic::Ordering::Relaxed))).clone()
}

fn build_for(n: u32) -> reqwest::Client {
    let b = match (n, active().resolve()) {
        (1.., Some(mut p)) => {
            let _ = p.set_username(&format!("ytm-rs-{n}"));
            let _ = p.set_password(Some("x"));
            reqwest::Proxy::all(p.as_str()).map_or_else(|_| apply(reqwest::Client::builder()), |p| reqwest::Client::builder().proxy(p))
        }
        _ => apply(reqwest::Client::builder()),
    };
    b.connect_timeout(Duration::from_secs(if active().resolve().is_some() { 15 } else { 5 }))
        .pool_idle_timeout(Duration::from_secs(30))
        // A server that stops sending mid-response; streams have their own, tighter check.
        .read_timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36")
        .build()
        .unwrap_or_default()
}

/// Whether the proxy can be asked for another exit: a SOCKS proxy without a sign-in
/// of its own, i.e. Tor (which isolates circuits by username).
pub fn can_switch_exit() -> bool {
    active().resolve().is_some_and(|p| p.scheme().starts_with("socks") && p.username().is_empty())
}

/// Move to an exit where YouTube Music is offered (it isn't where the current one
/// is). Asks for a few circuits at once and keeps the first that works; false when
/// the proxy can't switch or none of them did.
pub async fn new_circuit(available: impl AsyncFn(&reqwest::Client) -> bool) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    const AT_ONCE: u32 = 4;
    const ROUNDS: u32 = 3;
    if !can_switch_exit() {
        return false;
    }
    let base = CIRCUIT.load(Relaxed);
    for round in 0..ROUNDS {
        let tries = (1..=AT_ONCE).map(|i| base + round * AT_ONCE + i).map(|n| {
            let available = &available;
            async move {
                let c = build_for(n);
                anyhow::ensure!(available(&c).await, "no");
                anyhow::Ok((n, c))
            }
        });
        if let Ok(((n, c), _)) = futures_util::future::select_ok(tries.map(Box::pin)).await {
            log::info!("proxy: switched to another exit (circuit {n})");
            CIRCUIT.store(n, Relaxed);
            *HTTP.write().unwrap() = Some(c);
            return true;
        }
    }
    false
}

fn apply_with(b: reqwest::ClientBuilder, s: &Setting) -> reqwest::ClientBuilder {
    match (s, s.resolve()) {
        // Outside Windows that's the environment, which reqwest reads by itself (NO_PROXY too).
        (Setting::System, _) if cfg!(not(windows)) => b,
        (_, Some(p)) => match reqwest::Proxy::all(p.as_str()) {
            Ok(proxy) => b.proxy(proxy),
            Err(e) => {
                log::warn!("proxy {}: {e}", redact(&p));
                b
            }
        },
        (Setting::Off, None) => b.no_proxy(),
        // System with nothing set in Windows: reqwest still honours HTTPS_PROXY & co.
        _ => b,
    }
}

/// Hand the active setting to a child process (the sign-in windows). A proxy the
/// webview can't use itself goes through the local relay instead.
pub fn pass_to(cmd: &mut std::process::Command) {
    let v = match (active(), RELAY.get()) {
        (Setting::Off, _) => "off".to_owned(),
        (_, Some(port)) if needs_relay() => format!("http://127.0.0.1:{port}"),
        (Setting::Custom(u), _) => u.clone(),
        (Setting::System, _) => "system".to_owned(),
    };
    cmd.env(CHILD_ENV, v);
}

static RELAY: OnceLock<u16> = OnceLock::new();

/// Whether the sign-in windows need the relay: Chromium can't sign in to a SOCKS
/// proxy at all, and for an HTTP one it would ask in a box of its own; WebKitGTK is
/// only told a host and port. On Windows a system proxy is WebView2's own business.
pub fn needs_relay() -> bool {
    let p = match active() {
        Setting::Custom(_) => active().resolve(),
        _ if cfg!(target_os = "linux") => webkit_proxy(),
        _ => None,
    };
    p.is_some_and(|p| !p.username().is_empty() || p.scheme().starts_with("socks") || p.scheme() == "https")
}

/// The proxy a WebKitGTK window is told. A system one that's `https://`, or that
/// `NO_PROXY` skips for Google, is left to WebKit, which reads those variables itself.
fn webkit_proxy() -> Option<url::Url> {
    match active() {
        Setting::System => system().filter(|p| p.scheme() != "https" && !no_proxy("accounts.google.com")),
        s => s.resolve(),
    }
}

/// Whether `NO_PROXY` sends `host` direct.
fn no_proxy(host: &str) -> bool {
    let list = ["NO_PROXY", "no_proxy"].iter().find_map(|k| std::env::var(k).ok()).unwrap_or_default();
    list.trim() == "*" || list.split(',').map(|e| e.trim().trim_start_matches('*').trim_start_matches('.')).any(|e| !e.is_empty() && (host == e || host.ends_with(&format!(".{e}"))))
}

/// A local, password-free HTTP proxy (CONNECT only) on a random loopback port that
/// forwards through the custom proxy with its credentials. Runs on the caller's
/// tokio runtime for the life of the app.
pub async fn start_relay() -> anyhow::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else { continue };
            tokio::spawn(async move {
                if let Err(e) = relay_one(&mut client).await {
                    log::debug!("proxy relay: {e:#}");
                }
            });
        }
    });
    let _ = RELAY.set(port);
    log::info!("proxy: sign-in windows go through a local relay on port {port}");
    Ok(port)
}

async fn relay_one(client: &mut TcpStream) -> anyhow::Result<()> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < 8192, "oversized request");
        head.push(client.read_u8().await?);
    }
    let text = String::from_utf8_lossy(&head);
    let mut first = text.lines().next().unwrap_or_default().split_whitespace();
    let (method, target) = (first.next().unwrap_or_default(), first.next().unwrap_or_default());
    if method != "CONNECT" {
        client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n").await?;
        anyhow::bail!("{method} isn't CONNECT");
    }
    let (host, port) = target.rsplit_once(':').ok_or_else(|| anyhow::anyhow!("bad target {target}"))?;
    let mut upstream = match connect(host, port.parse()?).await {
        Ok(s) => s,
        Err(e) => {
            client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n").await?;
            return Err(e);
        }
    };
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    tokio::io::copy_bidirectional(client, &mut upstream).await?;
    Ok(())
}

/// A sign-in window's webview. WebView2 follows Windows' proxy by itself, so only a
/// custom proxy, or none at all, needs saying. (Chromium takes no password here: an
/// authenticated proxy asks for it in the window.) WebKitGTK goes by the desktop's
/// proxy settings, which outside GNOME and KDE are usually none: it's told whichever
/// proxy is in effect. `unbound`: no device-bound sessions (see `login::run`).
pub fn webview(b: wry::WebViewBuilder<'_>, unbound: bool) -> wry::WebViewBuilder<'_> {
    #[cfg(windows)]
    {
        use wry::WebViewBuilderExtWindows;
        // wry's own defaults, which these arguments replace.
        const DEFAULT: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";
        let features = if unbound { format!("{DEFAULT},DeviceBoundSessions,EnableBoundSessionCredentials") } else { DEFAULT.to_owned() };
        match (active(), active().resolve()) {
            (Setting::Off, _) => return b.with_additional_browser_args(format!("{features} --no-proxy-server")),
            (Setting::Custom(_), Some(p)) => {
                let scheme = if p.scheme().starts_with("socks") { "socks5" } else { p.scheme() };
                let (host, port) = (p.host_str().unwrap_or_default(), p.port_or_known_default().unwrap_or(8080));
                return b.with_additional_browser_args(format!("{features} --proxy-server={scheme}://{host}:{port}"));
            }
            _ if unbound => return b.with_additional_browser_args(features),
            _ => {}
        }
    }
    #[cfg(not(windows))]
    let _ = unbound;
    #[cfg(target_os = "linux")]
    if let Some(p) = webkit_proxy() {
        let endpoint = wry::ProxyEndpoint { host: p.host_str().unwrap_or_default().to_owned(), port: p.port_or_known_default().unwrap_or(8080).to_string() };
        return b.with_proxy_config(if p.scheme().starts_with("socks") { wry::ProxyConfig::Socks5(endpoint) } else { wry::ProxyConfig::Http(endpoint) });
    }
    b
}

/// For yt-dlp: `--proxy <url>`, or `--proxy ""` (direct) when the proxy is off. One with
/// a password goes by `ytdlp_env` instead: other programs can read a command line.
pub fn ytdlp_args() -> Vec<String> {
    match (active(), active().resolve()) {
        (_, Some(p)) if !has_login(&p) => vec!["--proxy".into(), p.to_string()],
        (Setting::Off, None) => vec!["--proxy".into(), String::new()],
        _ => vec![],
    }
}

/// A proxy with a login, in the environment yt-dlp reads when it isn't given `--proxy`.
pub fn ytdlp_env(cmd: &mut std::process::Command) {
    let Some(p) = active().resolve().filter(has_login) else { return };
    let url = p.to_string();
    // Lowercase wins on Unix; on Windows these are the same variables.
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        cmd.env(name, &url);
    }
    // Like `--proxy`, for everything.
    cmd.env_remove("NO_PROXY").env_remove("no_proxy");
}

fn has_login(p: &url::Url) -> bool {
    !p.username().is_empty() || p.password().is_some()
}

/// A TCP connection to `host:port`, through the active proxy when there is one.
/// Used for Listen together's WebSocket, which reqwest doesn't carry.
pub async fn connect(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let Some(p) = active().resolve().or_else(env_proxy) else {
        return Ok(TcpStream::connect((host, port)).await?);
    };
    let (ph, pp) = (p.host_str().unwrap_or_default().to_owned(), p.port_or_known_default().unwrap_or(8080));
    let mut s = TcpStream::connect((ph.as_str(), pp)).await?;
    match p.scheme() {
        "socks5" | "socks5h" => socks5(&mut s, &p, host, port).await?,
        "http" => http_connect(&mut s, &p, host, port).await?,
        other => anyhow::bail!("can't tunnel through a {other}:// proxy"),
    }
    Ok(s)
}

async fn http_connect(s: &mut TcpStream, p: &url::Url, host: &str, port: u16) -> anyhow::Result<()> {
    let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if !p.username().is_empty() {
        use base64::Engine;
        let user = percent(p.username());
        let pass = percent(p.password().unwrap_or_default());
        req += &format!("Proxy-Authorization: Basic {}\r\n", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}")));
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).await?;
    // Read the response head byte by byte, so nothing past it is consumed.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < 8192, "proxy sent an oversized response");
        head.push(s.read_u8().await?);
    }
    let status = String::from_utf8_lossy(&head);
    let line = status.lines().next().unwrap_or_default();
    anyhow::ensure!(line.split_whitespace().nth(1) == Some("200"), "proxy refused the connection: {line}");
    Ok(())
}

async fn socks5(s: &mut TcpStream, p: &url::Url, host: &str, port: u16) -> anyhow::Result<()> {
    let auth = !p.username().is_empty();
    s.write_all(if auth { &[5, 2, 0, 2] } else { &[5, 1, 0] }).await?;
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).await?;
    match reply {
        [5, 0] => {}
        [5, 2] => {
            let (user, pass) = (percent(p.username()), percent(p.password().unwrap_or_default()));
            let mut msg = vec![1, user.len() as u8];
            msg.extend(user.as_bytes());
            msg.push(pass.len() as u8);
            msg.extend(pass.as_bytes());
            s.write_all(&msg).await?;
            s.read_exact(&mut reply).await?;
            anyhow::ensure!(reply[1] == 0, "the SOCKS proxy rejected the username or password");
        }
        _ => anyhow::bail!("the SOCKS proxy wants a sign-in ytm-rs doesn't support"),
    }
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend(host.as_bytes());
    req.extend(port.to_be_bytes());
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    anyhow::ensure!(head[1] == 0, "the SOCKS proxy couldn't connect (code {})", head[1]);
    // Skip the bound address it reports.
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => s.read_u8().await? as usize,
        _ => anyhow::bail!("unexpected SOCKS reply"),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await?;
    Ok(())
}

fn percent(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default()
}

/// `HTTPS_PROXY`/`HTTP_PROXY`, which reqwest honours by itself; Listen together reads them here.
fn env_proxy() -> Option<url::Url> {
    if matches!(active(), Setting::Off) {
        return None;
    }
    env_var(&["HTTPS_PROXY", "https_proxy"]).or_else(|| env_var(&["HTTP_PROXY", "http_proxy"])).or_else(|| env_var(&["ALL_PROXY", "all_proxy"]))
}

/// The first of `names` that's set, even to nothing, as reqwest reads them.
fn env_var(names: &[&str]) -> Option<url::Url> {
    names.iter().find_map(|k| std::env::var(k).ok()).and_then(|v| parse(&v).ok())
}

/// The proxy Windows is set to use (Settings › Network › Proxy › Manual), if on.
/// Setup scripts (PAC) aren't read. Elsewhere, the one the environment sets for
/// https, as reqwest picks it.
pub fn system() -> Option<url::Url> {
    #[cfg(windows)]
    {
        let (enabled, server) = windows_proxy()?;
        if !enabled || server.trim().is_empty() {
            return None;
        }
        // "host:port", or per protocol: "http=h:p;https=h:p;socks=h:p".
        let pick = |key: &str| server.split(';').find_map(|part| part.trim().strip_prefix(key).map(str::to_owned));
        let addr = if server.contains('=') {
            pick("https=").or_else(|| pick("http=")).map(|a| format!("http://{a}")).or_else(|| pick("socks=").map(|a| format!("socks5://{a}")))?
        } else {
            format!("http://{}", server.trim())
        };
        parse(&addr).ok()
    }
    #[cfg(not(windows))]
    env_var(&["HTTPS_PROXY", "https_proxy"]).or_else(|| env_var(&["ALL_PROXY", "all_proxy"]))
}

#[cfg(windows)]
fn windows_proxy() -> Option<(bool, String)> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    // SAFETY: out-buffers sized to what we pass; the strings are static.
    unsafe {
        let mut enabled = 0u32;
        let mut size = 4u32;
        let ok = RegGetValueW(HKEY_CURRENT_USER, key, w!("ProxyEnable"), RRF_RT_REG_DWORD, None, Some(&mut enabled as *mut u32 as *mut _), Some(&mut size));
        if ok.is_err() {
            return Some((false, String::new()));
        }
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let ok = RegGetValueW(HKEY_CURRENT_USER, key, w!("ProxyServer"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size));
        let server = if ok.is_ok() { String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]) } else { String::new() };
        Some((enabled != 0, server))
    }
}

/// Try a setting before saving it: one request through it to YouTube Music.
pub async fn test(s: &Setting) -> anyhow::Result<Duration> {
    if let Setting::Custom(u) = s {
        parse(u)?;
    }
    let http = apply_with(reqwest::Client::builder().connect_timeout(Duration::from_secs(8)).timeout(Duration::from_secs(12)), s).build()?;
    let t0 = Instant::now();
    if let Err(e) = http.get("https://music.youtube.com/generate_204").send().await.and_then(|r| r.error_for_status()) {
        anyhow::bail!(plain(e.without_url(), s.resolve().as_ref()));
    }
    let took = t0.elapsed();
    // Reaching YouTube isn't enough: where the proxy comes out, YouTube Music must be offered.
    if !crate::ytmapi::offered_through(&http).await {
        let tor = s.resolve().is_some_and(|p| p.scheme().starts_with("socks") && p.username().is_empty());
        anyhow::ensure!(tor, "it connects, but YouTube Music isn't available where this proxy comes out. Try a server in another country.");
        log::info!("proxy test: this exit isn't offered YouTube Music; the app switches exits by itself");
    }
    Ok(took)
}

/// The usual ways a proxy fails, said plainly.
fn plain(e: reqwest::Error, proxy: Option<&url::Url>) -> String {
    let mut chain = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        chain += &format!(": {s}");
        src = s.source();
    }
    let chain = chain.to_lowercase();
    let at = proxy.map(|p| format!("{}:{}", p.host_str().unwrap_or_default(), p.port_or_known_default().unwrap_or(0))).unwrap_or_default();
    if chain.contains("10061") || chain.contains("refused") {
        format!("nothing is answering at {at}. Is the proxy running?")
    } else if e.is_timeout() || chain.contains("timed out") {
        "the proxy didn't answer in time.".to_owned()
    } else if chain.contains("407") || chain.contains("authentication") || chain.contains("auth") {
        "the proxy didn't accept the username or password.".to_owned()
    } else if chain.contains("dns") || chain.contains("resolve") || chain.contains("11001") {
        "couldn't find that proxy's address.".to_owned()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proxy_addresses() {
        assert_eq!(parse("127.0.0.1:8080").unwrap().as_str(), "http://127.0.0.1:8080/");
        assert_eq!(parse("socks5://u:p@10.0.0.2:1080").unwrap().username(), "u");
        assert!(parse("ftp://x:1").is_err());
        assert!(parse("http://").is_err());
        assert!(parse("not a url at all").is_err());
        let u = parse("http://me:secret@host:3128").unwrap();
        assert_eq!(redact(&u), "http://me:•••@host:3128");
    }
}
