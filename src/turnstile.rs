//! Cloudflare Turnstile for the Better Lyrics API, completed the same way the browser
//! extension does: the API's own challenge page runs in a real browser engine
//! (WebView2) and its widget reports the token. The widget uses
//! `appearance: interaction-only`, so it normally passes invisibly; if Cloudflare wants
//! an interaction, the window is shown so the user can complete it. Nothing here
//! solves or forges the challenge.
//!
//! Like the sign-in window, this runs in a child process (`ytm-rs --turnstile <dir>`)
//! because tao and winit can't share a process.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebContext, WebViewBuilder};

use crate::login::BuildIn;

pub const FLAG: &str = "--turnstile";
const CHALLENGE: &str = "https://lyrics.api.dacubeking.com/challenge";
/// The widget says when it needs an interaction; this is only a fallback in case that
/// never arrives. Counted from the page having loaded, since a cold WebView2 start can
/// take longer than an invisible pass does.
const SHOW_AFTER: Duration = Duration::from_secs(15);
/// Give up if nothing happens (e.g. the user ignores a shown challenge).
const GIVE_UP_AFTER: Duration = Duration::from_secs(120);

/// The challenge page posts to `window.parent`; at top level that is the page itself,
/// so forward those messages to the native side. The page doesn't say when the widget
/// wants an interaction, so give the widget a before-interactive callback too; it reads
/// it when it renders on DOMContentLoaded, and this listener runs first.
const BRIDGE: &str = r#"
window.addEventListener('message', function (e) {
  if (e.data && typeof e.data.type === 'string' && e.data.type.indexOf('turnstile-') === 0) {
    window.ipc.postMessage(JSON.stringify(e.data));
  }
});
window.ytmTurnstileInteractive = function () {
  window.ipc.postMessage(JSON.stringify({ type: 'turnstile-interactive' }));
};
document.addEventListener('DOMContentLoaded', function () {
  document.querySelectorAll('.cf-turnstile').forEach(function (el) {
    if (!el.getAttribute('data-before-interactive-callback')) {
      el.setAttribute('data-before-interactive-callback', 'ytmTurnstileInteractive');
    }
  });
});
"#;

/// Run the challenge in a child process; returns the Turnstile token.
pub fn obtain(profile_dir: PathBuf) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    crate::proxy::pass_to(&mut cmd);
    crate::login::webkit_env(&mut cmd);
    let out = cmd
        .arg(FLAG)
        .arg(&profile_dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().last().unwrap_or_default();
    if let Some(t) = line.strip_prefix("token ") {
        Ok(t.to_owned())
    } else if let Some(e) = line.strip_prefix("failed ") {
        anyhow::bail!("lyrics verification failed: {e}")
    } else {
        anyhow::bail!("lyrics verification window exited ({})", out.status)
    }
}

/// Entry point of the child process.
pub fn child_main(profile_dir: PathBuf) {
    match run(profile_dir) {
        Ok(token) => println!("token {token}"),
        Err(e) => println!("failed {e:#}"),
    }
}

fn run(profile_dir: PathBuf) -> anyhow::Result<String> {
    let mut event_loop = EventLoopBuilder::<String>::with_user_event().build();
    // macOS: no Dock icon for a window that's usually never shown.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let window = WindowBuilder::new()
        .with_title("Verifying lyrics access")
        .with_inner_size(tao::dpi::LogicalSize::new(360.0, 120.0))
        .with_visible(false)
        .build(&event_loop)?;

    let proxy = event_loop.create_proxy();
    let loaded_proxy = proxy.clone();
    let mut context = WebContext::new(Some(profile_dir));
    let webview = crate::proxy::webview(WebViewBuilder::new_with_web_context(&mut context), false)
        .with_initialization_script(BRIDGE)
        .with_ipc_handler(move |req| {
            let _ = proxy.send_event(req.body().clone());
        })
        .with_on_page_load_handler(move |ev, _| {
            if matches!(ev, PageLoadEvent::Finished) {
                let _ = loaded_proxy.send_event(r#"{"type":"loaded"}"#.to_owned());
            }
        })
        .with_url(CHALLENGE)
        .build_in(&window)?;

    let started = Instant::now();
    let mut loaded: Option<Instant> = None;
    let mut result: anyhow::Result<String> = Err(anyhow::anyhow!("closed before verification finished"));
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
        match event {
            Event::UserEvent(msg) => {
                let v: serde_json::Value = serde_json::from_str(&msg).unwrap_or_default();
                match v["type"].as_str() {
                    Some("turnstile-token") => {
                        result = v["token"]
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow::anyhow!("empty token"));
                        *flow = ControlFlow::Exit;
                    }
                    Some("turnstile-error") => {
                        result = Err(anyhow::anyhow!("challenge error {}", v["error"]));
                        *flow = ControlFlow::Exit;
                    }
                    Some("turnstile-timeout") => {
                        result = Err(anyhow::anyhow!("challenge timed out"));
                        *flow = ControlFlow::Exit;
                    }
                    Some("turnstile-interactive") => show(&window),
                    Some("loaded") => {
                        loaded.get_or_insert_with(Instant::now);
                    }
                    // Expired before use: ask the widget for a fresh one.
                    Some("turnstile-expired") => {
                        let _ = webview.evaluate_script("turnstile.reset()");
                    }
                    _ => {}
                }
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                let waited = started.elapsed();
                if waited > GIVE_UP_AFTER {
                    result = Err(anyhow::anyhow!("timed out waiting for verification"));
                    *flow = ControlFlow::Exit;
                } else if loaded.is_some_and(|t| t.elapsed() > SHOW_AFTER) {
                    show(&window);
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *flow = ControlFlow::Exit,
            _ => {}
        }
    });
    drop(webview);
    result
}

/// Cloudflare wants an interaction; let the user see the widget.
fn show(window: &tao::window::Window) {
    if !window.is_visible() {
        window.set_inner_size(tao::dpi::LogicalSize::new(360.0, 160.0));
        window.set_visible(true);
        window.set_focus();
    }
}
