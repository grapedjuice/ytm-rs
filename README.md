<div align="center">
  <img src="assets/icon/icon.png" alt="ytm-rs app icon" width="96">
  <h1>ytm-rs</h1>
  <p><strong>YouTube Music in a fast, native desktop app.</strong></p>
  <p>Built in Rust with egui and rodio. No Electron, no browser-based player, and around 130 MB of memory while playing.</p>

  <p>
    <a href="https://github.com/grapedjuice/ytm-rs/releases/latest"><img src="https://img.shields.io/github/v/release/grapedjuice/ytm-rs?style=flat-square&color=ff4e45" alt="Latest release"></a>
    <a href="https://github.com/grapedjuice/ytm-rs/releases"><img src="https://img.shields.io/github/downloads/grapedjuice/ytm-rs/total?style=flat-square&color=ff4e45" alt="Downloads"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-333?style=flat-square" alt="Platforms">
    <a href="LICENSE"><img src="https://img.shields.io/github/license/grapedjuice/ytm-rs?style=flat-square&color=333" alt="License: GPL-3.0"></a>
    <a href="https://ko-fi.com/grapedjuice"><img src="https://img.shields.io/badge/Ko--fi-support-ff4e45?style=flat-square&logo=kofi&logoColor=white" alt="Support on Ko-fi"></a>
  </p>

  <p>
    <a href="https://grapedjuice.github.io/ytm-rs/">Website</a> ·
    <a href="https://github.com/grapedjuice/ytm-rs/releases/latest">Download</a> ·
    <a href="#features">Features</a> ·
    <a href="#installation">Installation</a> ·
    <a href="#building-from-source">Build from source</a> ·
    <a href="https://github.com/grapedjuice/ytm-rs/discussions">Discussions</a>
  </p>
</div>

<p align="center">
  <img src="docs/readme/v2/hero.jpg" alt="ytm-rs playing Espresso by Sabrina Carpenter with synced lyrics in the Apple Music theme, in front of the Home page, with the mini player floating over both" width="100%">
</p>

## Why ytm-rs

Most YouTube Music desktop apps are the website running in a hidden browser. ytm-rs draws everything itself, so it starts quickly, stays light and leaves your memory for everything else.

| | ytm-rs | YouTube Music in Chrome | Pear Desktop |
| --- | --- | --- | --- |
| Memory with a song playing | **~130 MB** | 496 MB | 638 MB |
| Windows download | **11 MB** | – | 107 MB |
| Installed size | **36 MB** | – | 397 MB |
| Runs a web browser inside | No | Yes | Yes |

<sub>Memory with a song playing on the same Windows 11 PC: ytm-rs 0.3.0, Chrome 155 with one tab and Pear Desktop 3.12.0. Sizes: ytm-rs 0.3.0 and Pear Desktop 3.12.0.</sub>

## Table of contents

- [Why ytm-rs](#why-ytm-rs)
- [Features](#features)
- [Screenshots](#screenshots)
- [Installation](#installation)
- [Usage](#usage)
- [Building from source](#building-from-source)
- [Notes](#notes)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgements](#acknowledgements)
- [Star History](#star-history)

## Features

**Listening**
- **Your music feed:** Listen again, Quick picks, your mixes (My Supermix, Discover Mix and more) and mood filters, refreshed each time you open a page.
- **Explore and search:** new releases, charts, and search across songs, albums, artists and playlists.
- **Your library:** liked songs, saved albums and playlists, history and uploads. Upload your own songs from the Uploads tab or by dropping files on the window. Turn on podcasts to see shows and episodes too.
- **Song lists:** drag songs to reorder the queue and your own playlists, or drop them on a playlist in the sidebar to add them. A find box narrows any list to the songs that match.
- **Playback:** queue, song radio, shuffle, repeat, autoplay and crossfade, with the next track prefetched for instant skips. Volume normalization, a 10-band equalizer with presets, and a choice of output device.
- **Sound quality:** Opus at about 256 kbps with YouTube Premium, the best stream YouTube offers, and Opus at about 130 kbps without it.
- **Songs, not music videos:** an official music video plays as its album recording, without the intros and skits, or switch to the video itself in Now playing.
- **Skip non-music parts:** jumps over talking, skits and sponsor breaks in music videos, as marked by SponsorBlock.
- **Explicit songs:** marked with an E. Turn on Hide explicit songs to leave them out of lists, search, the queue and autoplay, and optionally use YouTube's Restricted Mode as well.
- **Downloads:** save any song as a tagged `.m4a` with cover art, at the best quality your account gets. Saved songs are listed in Library › Downloads and play offline.

**Lyrics and looks**
- **Karaoke lyrics:** word-by-word lyrics drawn the way the Better Lyrics extension draws them, from every Better Lyrics source, with automatic fallback, romanization, translations and per-song timing offsets.
- **Animated background:** the album art drifts and pulses with the music behind everything. Go full-screen, or hide the lyrics to see just the artwork.
- **30 themes:** 27 rebuilt from the [Better Lyrics community themes](https://github.com/better-lyrics/themes), such as Apple Music, Spicy, Spinning Record, reCover and Material 3, plus the app's own ytm-rs and Classic looks and Fluent, in the style of Windows 11. Each one restyles the whole app, in light or dark mode, with real frosted glass where the theme has it. Use any bundled or installed font.
- **Your colours:** the accent can follow the theme or your system, or be any colour you pick. A `theme.conf` file can set every colour in the app, for tools like wallust and pywal to write. Zoom makes everything bigger or smaller.

**Together and across services**
- **Listen together:** start a jam, share the code or link, and up to 16 people hear the same song at the same moment. The host decides whether guests can add songs or control playback.
- **Transfer from Spotify:** bring playlists and Liked Songs over to YouTube Music in the background. ytm-rs asks you when it isn't sure about a match and picks up where it left off if interrupted.
- **Last.fm, ListenBrainz and Discord:** scrobbling and Rich Presence.

**Desktop**
- System media controls with a seekable timeline, taskbar thumbnail buttons, close to tray (Wayland too), a choice of page to open on, a collapsible sidebar and screen-reader-accessible controls.
- The app draws its own title bar on Windows, so the window is one surface, and on Linux desktops that draw none. On Windows 11, Mica or Acrylic can show behind the window.
- **Mini player:** shrink the window to a small strip with the song and controls (Ctrl+M), or try the experimental pill that sits in a corner and opens on hover.
- **System-wide media shortcuts:** play/pause, next, previous and volume work while another app is in front or the window is in the tray. Change them in Settings › Keyboard shortcuts.
- Automatic updates in the background for both the installed and portable versions, with release notes when an update is ready. Pick the Nightly channel for builds of the newest changes.

## Screenshots

<p align="center">
  <img src="docs/readme/v2/themes-grid.jpg" alt="Six of the 30 themes in Now playing: ytm-rs, Apple Music, Material 3, Spicy, Spinning Record and reCover, each with synced lyrics" width="100%">
</p>

<p align="center">
  <img src="docs/readme/v2/features.jpg" alt="Light mode on the Home page, Listen together, the 10-band equalizer, the mini player, and the Last.fm and Discord connections in Settings" width="100%">
</p>

## Installation

Downloads for every platform are on the [latest release](https://github.com/grapedjuice/ytm-rs/releases/latest). Once installed, ytm-rs keeps itself up to date; you can turn this off, or switch to nightly builds, in **Settings › About › Updates**.

Free code signing for Windows provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org). See the [code signing policy](CODE_SIGNING.md).

<details open>
<summary><b>Windows</b></summary>

Download `ytm-rs-setup-<version>.exe` and run it. It installs for the current user without administrator access, and adds a Start menu shortcut and an uninstall entry in Windows Settings.

</details>

<details>
<summary><b>macOS</b></summary>

Download `ytm-rs-setup-<version>-macos-aarch64.dmg` (Apple silicon) or `-macos-x86_64.dmg` (Intel), open it, and drag **ytm-rs** to Applications. Requires macOS 11 or later.

The app isn't notarized, so the first time, right-click it and pick **Open**, or run:

```sh
xattr -dr com.apple.quarantine /Applications/ytm-rs.app
```

Sign-ins are kept in your login Keychain. Jam links don't open the app on macOS yet; type the code on the **Listen together** page instead.

</details>

<details>
<summary><b>Linux</b></summary>

```sh
curl -fsSL https://raw.githubusercontent.com/grapedjuice/ytm-rs/main/linux/install.sh | sh
```

This installs ytm-rs for your user (no root) in `~/.local/lib/ytm-rs`, with a `ytm-rs` command and an app menu entry. If WebKitGTK or other required libraries are missing, it installs them too (that step asks for your password). You can also download `ytm-rs-<version>-linux-x86_64.tar.gz`, extract it and run `./install.sh`.

On Debian, Ubuntu and their derivatives you can instead download `ytm-rs_<version>_amd64.deb` and open it, or run `sudo apt install ./ytm-rs_<version>_amd64.deb`. It doesn't update itself: when a new version is out, the app says so and links to the new `.deb`, which installs over the old one. To uninstall it, run `sudo apt remove ytm-rs`.

Sign-ins are kept in your keyring (GNOME Keyring or KWallet). To uninstall, run `~/.local/lib/ytm-rs/install.sh --uninstall`.

</details>

<details>
<summary><b>Portable</b></summary>

Every platform has a portable download that keeps its settings in a `data` folder beside the app and updates itself in place.

| Platform | File | How to run |
| --- | --- | --- |
| Windows | `ytm-rs-portable-<version>.zip` (`-arm64` for ARM) | Unzip anywhere and run `ytm-rs.exe`. It stays portable while `portable.txt` is beside it. |
| macOS | `ytm-rs-portable-<version>-macos-aarch64.zip` or `-macos-x86_64.zip` | Unzip, run `xattr -dr com.apple.quarantine` on the folder, then open `ytm-rs.app`. Without that step macOS runs it from a read-only copy and it uses the normal data location. |
| Linux | `ytm-rs-portable-<version>-linux-x86_64.tar.gz` | Extract anywhere and run `./ytm-rs`. |

Sign-ins are still kept by the system (Credential Manager, Keychain or your keyring), so on another computer you sign in again. Opening jam links needs the installed app; in the portable one, type the code on the **Listen together** page.

</details>

## Usage

| To… | Do this |
| --- | --- |
| **Sign in** | **Settings › Account › Sign in with Google.** Unlocks your feed, library, likes and history. Browsing and search work without it. If your account has more than one YouTube channel, pick one there too. |
| **Listen together** | Click **Listen together** in the sidebar, start a jam and share the code or link. Songs guests add play next, marked with who added them, up to 5 waiting per guest. |
| **Transfer from Spotify** | **Settings › General › Transfer from Spotify**, then sign in to Spotify to pick a playlist or Liked Songs, or paste a public playlist link without signing in. Matches ytm-rs isn't sure about come to you for review before it creates a private playlist. |
| **Upload music** | **Library › Uploads › Upload music**, or drop MP3, M4A, FLAC, OGG or WMA files on the window. Title, artist, album and cover come from the file's tags. |
| **Download songs** | Open a song's menu and click **Download**. Files go to `Music\ytm-rs`; change this in **Settings › General › Downloads**. |
| **Scrobble to Last.fm** | **Settings › Account › Connections**, click **Connect** next to Last.fm, then allow ytm-rs in your browser. Songs scrobble after half their length or four minutes. |
| **Send listens to ListenBrainz** | Copy your user token from [listenbrainz.org/settings](https://listenbrainz.org/settings/), then in **Settings › Account › Connections** paste it under ListenBrainz and click **Connect**. Songs count after half their length or four minutes. |
| **Change the theme** | **Settings › Appearance**, click a theme card. Mode switches between light and dark, and Accent colour sets the highlight colour. |
| **Shape the sound** | **Settings › Playback** has the equalizer, crossfade and volume normalization. |
| **Tune the lyrics** | **Settings › Lyrics** sets the source order, translation language and timing offsets. |

## Building from source

You need the [Rust toolchain](https://rustup.rs/).

```sh
git clone https://github.com/grapedjuice/ytm-rs.git
cd ytm-rs
```

<details open>
<summary><b>Windows</b></summary>

Install the MSVC C++ build tools, then:

```powershell
cargo build --release
```

The app is at `target\release\ytm-rs.exe`.

</details>

<details>
<summary><b>Linux</b> (Debian/Ubuntu package names)</summary>

```sh
sudo apt install build-essential pkg-config libssl-dev libasound2-dev libgtk-3-dev \
  libwebkit2gtk-4.1-dev libxdo-dev libayatana-appindicator3-dev libdbus-1-dev
cargo build --release
```

The app is at `target/release/ytm-rs`.

</details>

<details>
<summary><b>macOS</b></summary>

Install the Xcode command line tools (`xcode-select --install`), then build and wrap it in an app bundle:

```sh
cargo build --release
sh macos/bundle.sh target/release/ytm-rs "$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)" target/release
```

The app is at `target/release/ytm-rs.app`.

</details>

### Listen together relay

The relay in `server/` is a separate Cloudflare Worker, also in Rust ([workers-rs](https://github.com/cloudflare/workers-rs)). You don't need it to build or run the app, which connects to the hosted relay. To run it locally, start `npx wrangler dev --local` in `server/` and launch the app with `YTM_JAM_SERVER=ws://127.0.0.1:8787`.

## Notes

- The interface and audio playback are native. Google and Spotify sign-in and lyrics verification open temporary system webview windows (WebView2 on Windows).
- Lyrics come from the providers Better Lyrics uses (Better Lyrics, Unison, Musixmatch, LRCLib and others), so availability depends on the track. Translations come from the lyrics when included, otherwise from Unison or Google Translate; machine translations and romanization can be rough.
- The first song after launch can take about 5 seconds to start while the app prepares YouTube's player. After that, songs start instantly.
- Premium-only and age-restricted songs play through your signed-in account. With YouTube Premium they play natively; without it, age-restricted songs fall back to `yt-dlp`, which needs Node.js.
- No audio goes through the Listen together relay. It only passes along what's playing and where; every listener streams the song from YouTube Music themselves.
- Themes are native translations of CSS themes, so some details differ from the originals. Fonts that can't be redistributed (SF Pro, Spotify's) are replaced with the closest open fonts.
- ytm-rs is an unofficial client. Changes to YouTube Music or third-party services can affect playback, sign-in or lyrics.

## Contributing

Found a bug or have an idea? [Open an issue](https://github.com/grapedjuice/ytm-rs/issues). Questions, or a setup to show off? Head to [Discussions](https://github.com/grapedjuice/ytm-rs/discussions). Pull requests are welcome.

## License

ytm-rs is licensed under [GPL-3.0](LICENSE).

## Acknowledgements

- [egui](https://github.com/emilk/egui) for the interface, [rodio](https://github.com/RustAudio/rodio) and Symphonia for audio.
- Browsing, search and your library come straight from YouTube Music's own web API, the one music.youtube.com uses.
- Signed-in stream URLs are decoded with yt-dlp's [EJS](https://github.com/yt-dlp/ejs) solver running in QuickJS.
- Listen together runs on a small relay built with [workers-rs](https://github.com/cloudflare/workers-rs) on Cloudflare Durable Objects.
- The animated background is based on Kawarp from [Better Lyrics](https://github.com/better-lyrics), and the theme presets are adapted from the Better Lyrics theme store, with credit to each theme's creators.
- Fonts: Inter, DM Sans, Google Sans Flex, Figtree and Instrument Sans (SIL OFL 1.1). Icons: Lucide.

## Star History

<a href="https://www.star-history.com/?repos=grapedjuice%2Fytm-rs&type=date&legend=bottom-right">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=grapedjuice/ytm-rs&type=date&theme=dark&legend=bottom-right" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=grapedjuice/ytm-rs&type=date&legend=bottom-right" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=grapedjuice/ytm-rs&type=date&legend=bottom-right" />
 </picture>
</a>
