<div align="center">
  <img src="assets/icon/icon.png" alt="ytm-rs app icon" width="88">
  <h1>ytm-rs</h1>
  <p><strong>YouTube Music in a fast, native desktop app.</strong></p>
  <p>Built with Rust, egui, and rodio. No Electron or browser-based player.</p>
  <p>
    <a href="https://github.com/grapedjuice/ytm-rs/releases/latest">Download for Windows</a>
    · <a href="#features">Features</a>
    · <a href="#build-from-source">Build from source</a>
  </p>
</div>

## Features

| Feature                 | What you can do                                                                                                                                         |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Your music feed**     | Browse your personalized YouTube Music home feed, including Listen again, Quick picks, mixes, and mood filters. Keep scrolling for more.                |
| **Explore and search**  | Find new releases from US charting artists and artists in your music feed, browse charts, and search for songs, albums, and playlists.              |
| **Your library**        | Access liked songs, saved albums and playlists, and listening history after signing in.                                                                 |
| **Playback controls**   | Manage the queue, start radio from a song, and use shuffle, repeat, and autoplay. The next track is prefetched for quicker skips.                       |
| **Lyrics and visuals**  | Follow synced lyrics when available, click a line to seek, switch to full-screen lyrics, and turn on the animated, music-reactive album-art background. |
| **Desktop integration** | Use system media controls, Discord Rich Presence, and screen-reader-accessible controls.                                                                |

## Get started

### Install on Windows

Download the latest `ytm-rs-setup-<version>.exe` from [GitHub Releases](https://github.com/grapedjuice/ytm-rs/releases/latest) and run it. The installer works for the current user without administrator access and adds a Start menu shortcut and an entry in Windows Settings for uninstalling.

### Sign in

Open **Settings → Sign in with Google** in the app. Signing in unlocks your music feed, library, likes, and history. You can also browse and search without signing in.

## Build from source

On Windows, install the [Rust toolchain](https://rustup.rs/) and the MSVC C++ build tools, then build the app itself:

```powershell
git clone https://github.com/grapedjuice/ytm-rs.git
cd ytm-rs
cargo build --release
```

The built application is at `target\release\ytm-rs.exe`.

## Notes

- The app interface and audio playback are native. Google sign-in and lyrics verification use temporary system webview windows; on Windows, these use WebView2.
- Synced lyrics come from the Better Lyrics API. If they are unavailable, the app can fall back to YouTube Music's plain lyrics. Lyrics availability depends on the track and the providers.
- Age-restricted tracks use an automatic `yt-dlp` fallback and require Node.js. Regular playback does not.
- ytm-rs is an unofficial client. Changes to YouTube Music or third-party services can affect playback, sign-in, or lyrics.

## License

[GPL-3.0](LICENSE). ytm-rs uses rustypipe, which is GPL-3.0.

## Built with

[egui](https://github.com/emilk/egui) for the interface, [rodio](https://github.com/RustAudio/rodio) and Symphonia for audio, and [rustypipe](https://crates.io/crates/rustypipe) for YouTube Music browsing. The animated background is based on Kawarp from Better Lyrics. The app also uses Inter and Lucide icons.

Found a bug or have an idea? [Open an issue](https://github.com/grapedjuice/ytm-rs/issues).
