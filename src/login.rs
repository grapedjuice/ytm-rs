//! "Sign in with Google" window.
//!
//! Opens Google's normal sign-in page in the system webview (WebView2 on Windows, which
//! ships with the OS), waits until the flow lands on music.youtube.com, then hands the
//! session cookies (HttpOnly ones included) to the app.
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

const START: &str = "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";

pub enum Outcome {
    /// `Cookie` header value for youtube.com.
    Cookie(String),
    Cancelled,
    Failed(String),
}

/// Command-line flag that makes the binary run only the sign-in window.
pub const FLAG: &str = "--login";

/// Show the sign-in window in a child process; `done` is called exactly once.
pub fn spawn(profile_dir: PathBuf, done: impl FnOnce(Outcome) + Send + 'static) {
    std::thread::Builder::new()
        .name("login".into())
        .spawn(move || {
            let res = std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .arg(FLAG)
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
pub fn child_main(profile_dir: PathBuf) {
    match run(profile_dir) {
        Ok(Outcome::Cookie(c)) => println!("cookie {c}"),
        Ok(Outcome::Cancelled) => println!("cancelled"),
        Ok(Outcome::Failed(e)) => println!("failed {e}"),
        Err(e) => println!("failed {e:#}"),
    }
}

/// Delete the webview profile so the next sign-in starts fresh (used on sign-out).
pub fn forget(profile_dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(profile_dir);
}

fn run(profile_dir: PathBuf) -> anyhow::Result<Outcome> {
    let mut event_loop = EventLoopBuilder::<()>::with_user_event().build();

    let window = WindowBuilder::new()
        .with_title("Sign in to YouTube Music")
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 720.0))
        .build(&event_loop)?;

    let proxy = event_loop.create_proxy();
    let mut context = WebContext::new(Some(profile_dir));
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(START)
        .with_on_page_load_handler(move |ev, url| {
            if matches!(ev, PageLoadEvent::Finished) && url.starts_with("https://music.youtube.com") {
                let _ = proxy.send_event(());
            }
        })
        .build(&window)?;

    let mut outcome = Outcome::Cancelled;
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(()) => match webview.cookies_for_url("https://music.youtube.com/") {
                Ok(cookies) => {
                    // Landing on music.youtube.com while signed out also fires this; only a
                    // session with SAPISID can authenticate API requests.
                    if cookies.iter().any(|c| c.name() == "SAPISID") {
                        let header = cookies
                            .iter()
                            .map(|c| format!("{}={}", c.name(), c.value()))
                            .collect::<Vec<_>>()
                            .join("; ");
                        outcome = Outcome::Cookie(header);
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
