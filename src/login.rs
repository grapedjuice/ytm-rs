//! "Sign in with Google" and "Sign in to Spotify" windows.
//!
//! Opens the service's normal sign-in page in the system webview (WebView2 on Windows,
//! WKWebView on macOS, WebKitGTK on Linux), waits until the flow lands on the app's
//! site, then hands the session cookies (HttpOnly ones included) to the app.
//!
//! The window runs in a child process (`ytm-rs --login <dir>`): tao and winit crash
//! when both pump windows in one process (STATUS_FATAL_USER_CALLBACK_EXCEPTION), and a
//! separate process also returns all webview memory to the OS once sign-in ends.

use std::path::PathBuf;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebContext, WebViewBuilder};

/// Which sign-in.
#[derive(Clone, Copy)]
pub enum Site {
    /// youtube.com, for YouTube Music.
    Google,
    /// open.spotify.com, for the Spotify transfer.
    Spotify,
}

impl Site {
    fn flag(self) -> &'static str {
        match self {
            Site::Google => FLAG,
            Site::Spotify => SPOTIFY_FLAG,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Site::Google => "Sign in to YouTube Music",
            Site::Spotify => "Sign in to Spotify",
        }
    }

    fn start(self) -> &'static str {
        match self {
            Site::Google => "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F",
            Site::Spotify => "https://accounts.spotify.com/en/login?continue=https%3A%2F%2Fopen.spotify.com%2F",
        }
    }

    /// Where the flow lands once signed in.
    fn home(self) -> &'static str {
        match self {
            Site::Google => "https://music.youtube.com/",
            Site::Spotify => "https://open.spotify.com/",
        }
    }

    /// The cookie that only a signed-in session has.
    fn session_cookie(self) -> &'static str {
        match self {
            Site::Google => "SAPISID",
            Site::Spotify => "sp_dc",
        }
    }
}

pub enum Outcome {
    /// Google: the `Cookie` header value for youtube.com. Spotify: the `sp_dc` cookie's value.
    Cookie(String),
    Cancelled,
    Failed(String),
}

/// Safari's user agent (macOS always reports 10_15_7 here, whatever the version).
#[cfg(target_os = "macos")]
const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

/// Command-line flags that make the binary run only a sign-in window.
pub const FLAG: &str = "--login";
pub const SPOTIFY_FLAG: &str = "--login-spotify";

/// Show the sign-in window in a child process; `done` is called exactly once.
pub fn spawn(site: Site, profile_dir: PathBuf, done: impl FnOnce(Outcome) + Send + 'static) {
    std::thread::Builder::new()
        .name("login".into())
        .spawn(move || {
            let res = std::env::current_exe().and_then(|exe| {
                let mut cmd = std::process::Command::new(exe);
                crate::proxy::pass_to(&mut cmd);
                cmd.arg(site.flag())
                    .arg(&profile_dir)
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
            });
            done(match res {
                Err(e) => Outcome::Failed(format!("could not start sign-in window: {e}")),
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let line = text.lines().last().unwrap_or_default();
                    if let Some(c) = line.strip_prefix("cookie ") {
                        Outcome::Cookie(c.to_owned())
                    } else if let Some(e) = line.strip_prefix("failed ") {
                        Outcome::Failed(e.to_owned())
                    } else if line == "cancelled" {
                        Outcome::Cancelled
                    } else {
                        Outcome::Failed(format!("sign-in window exited unexpectedly ({})", out.status))
                    }
                }
            })
        })
        .expect("spawn login thread");
}

/// Entry point of the child process: run the window and report on stdout.
pub fn child_main(site: Site, profile_dir: PathBuf) {
    match run(site, profile_dir) {
        Ok(Outcome::Cookie(c)) => println!("cookie {c}"),
        Ok(Outcome::Cancelled) => println!("cancelled"),
        Ok(Outcome::Failed(e)) => println!("failed {e}"),
        Err(e) => println!("failed {e:#}"),
    }
}

/// Delete the webview profile so the next sign-in starts fresh (used on sign-out).
/// Put the webview in its window. On Linux it goes in the GTK box tao made for it:
/// wry's plain `build` only works with an X11 window that already exists, and these
/// start hidden (and Wayland has no such window at all).
pub trait BuildIn<'a> {
    fn build_in(self, window: &'a tao::window::Window) -> wry::Result<wry::WebView>;
}

impl<'a> BuildIn<'a> for WebViewBuilder<'a> {
    fn build_in(self, window: &'a tao::window::Window) -> wry::Result<wry::WebView> {
        #[cfg(target_os = "linux")]
        {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            self.build_gtk(window.default_vbox().expect("tao makes a vbox by default"))
        }
        #[cfg(not(target_os = "linux"))]
        self.build(window)
    }
}

pub fn forget(profile_dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(profile_dir);
}

/// WKWebView has no profile folder (wry ignores the context's on macOS), so each
/// profile gets a data store of its own (macOS 14+; older ones share the default),
/// named by an id kept in the folder. `forget` deletes the folder, so the next sign-in
/// starts from a fresh store.
#[cfg(target_os = "macos")]
fn store_id(profile_dir: &std::path::Path) -> [u8; 16] {
    use sha1::Digest;
    let file = profile_dir.join("store-id");
    if let Ok(saved) = std::fs::read(&file)
        && let Ok(id) = <[u8; 16]>::try_from(saved.as_slice())
    {
        return id;
    }
    let seed = format!("{} {:?} {}", profile_dir.display(), std::time::SystemTime::now(), std::process::id());
    let mut id = [0u8; 16];
    id.copy_from_slice(&sha1::Sha1::digest(seed)[..16]);
    let _ = std::fs::create_dir_all(profile_dir);
    let _ = std::fs::write(&file, id);
    id
}

fn run(site: Site, profile_dir: PathBuf) -> anyhow::Result<Outcome> {
    let mut event_loop = EventLoopBuilder::<()>::with_user_event().build();

    let window = WindowBuilder::new()
        .with_title(site.title())
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 720.0))
        .build(&event_loop)?;

    let proxy = event_loop.create_proxy();
    #[cfg(target_os = "macos")]
    let store = store_id(&profile_dir);
    let mut context = WebContext::new(Some(profile_dir));
    let builder = WebViewBuilder::new_with_web_context(&mut context);
    #[cfg(target_os = "macos")]
    let builder = {
        use wry::WebViewBuilderExtDarwin;
        // WKWebView's own user agent has no "Version/… Safari/…", so YouTube Music takes it
        // for an unknown browser and shows "not optimized for your browser" instead of the
        // site. Say it's Safari, which is the engine it is.
        builder.with_data_store_identifier(store).with_user_agent(SAFARI_UA)
    };
    let webview = crate::proxy::webview(builder)
        .with_url(site.start())
        .with_on_page_load_handler(move |ev, url| {
            if matches!(ev, PageLoadEvent::Finished) && url.starts_with(site.home()) {
                let _ = proxy.send_event(());
            }
        })
        .build_in(&window)?;

    let mut outcome = Outcome::Cancelled;
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(()) => match webview.cookies_for_url(site.home()) {
                Ok(cookies) => {
                    // Landing there while signed out also fires this; only a signed-in
                    // session has the session cookie (SAPISID, sp_dc).
                    if let Some(c) = cookies.iter().find(|c| c.name() == site.session_cookie()) {
                        outcome = Outcome::Cookie(match site {
                            Site::Spotify => c.value().to_owned(),
                            Site::Google => cookies.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; "),
                        });
                        *flow = ControlFlow::Exit;
                    }
                }
                Err(e) => {
                    outcome = Outcome::Failed(format!("could not read cookies: {e}"));
                    *flow = ControlFlow::Exit;
                }
            },
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *flow = ControlFlow::Exit,
            _ => {}
        }
    });
    drop(webview);
    Ok(outcome)
}
