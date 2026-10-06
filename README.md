# ytm-rs

A native YouTube Music desktop client written in Rust. No Electron, no webview, no
bundled browser engine. It's inspired by [spotifast](https://github.com/crmne/spotifast)
(egui Spotify client) and [Pear Desktop](https://github.com/pear-devs/pear-desktop).

## Features

- Search (songs, albums, artists, playlists), plus home with new releases and charts
- Album, artist and playlist pages, and your library (liked songs, playlists, albums) after sign-in
- Queue with play next, add to queue, shuffle and repeat (off / all / one)
- Radio from any track, with autoplay that keeps the queue going when it runs out
- Lyrics panel
- OS media controls: Windows SMTC, MPRIS on Linux, Now Playing on macOS (via souvlaki)
- Keyboard: `Space` play/pause, `Ctrl+→/←` next/previous, `Ctrl+F` search, `Alt+←` or mouse back to go back
- Every control has an accessible name, so the app works with screen readers through AccessKit

## Build

```sh
cargo run --release
```

On Windows this needs the MSVC build tools. Settings live in the eframe storage dir
(`%APPDATA%\ytm-rs`), and rustypipe's cache sits next to them.

To sign in, open Settings and paste the `cookie` request header from a signed-in
music.youtube.com tab (DevTools → Network).

## How it stays light

| | |
|---|---|
| UI | egui on glow (OpenGL). It repaints only on input, on network events, or twice a second while playing |
| Audio | rodio + symphonia, pure-Rust AAC decoding (itag 140) |
| API | [rustypipe](https://crates.io/crates/rustypipe) for search, browse, library, radio and lyrics |
| Threads | UI, a 2-worker tokio runtime, the audio thread and the OS audio callback |

**Stream resolution.** As of October 2026, rustypipe's player clients either fail
signature deobfuscation or get cut off after 1 MiB without a PO token. Instead,
`src/innertube.rs` calls InnerTube `/player` as the visionOS client, the same client
yt-dlp uses. It returns plain URLs that need no JS player and no PO token. rustypipe
is kept as a fallback.

**Progressive, parallel download.** `src/stream.rs` splits the file into 256 KiB
blocks and fetches them with 4 concurrent range requests. Workers start from the
block the decoder is waiting on. Playback opens a *non-seekable* decoder, which
starts at the first MP4 fragment; symphonia's seekable mode would index every
fragment first, which means waiting for the whole file. A seek builds a seekable
decoder over the same buffer and swaps it in. If googlevideo starts answering 403
partway through, the URL is re-resolved and the download resumes.

**Prefetch.** Once a track starts, the next one in the queue is resolved and
downloaded, so Next and auto-advance open a decoder in under a millisecond.

### Measured (Windows 11, Ryzen + RX 6750 XT, release build)

- Binary: 14 MB
- CPU: 0% idle, about 0.3% of one core while playing
- Memory: about 126 MB private at idle. Most of that is AMD's OpenGL driver
  (`atio6axx.dll` alone maps 62 MB). The wgpu/DX12 renderer measured 407 MB on the
  same machine, so glow stays.
- Click to audio: about 150 ms to resolve, then the first fragment arrives in
  0.1–1.1 s depending on googlevideo throttling. Skipping to a prefetched track
  takes under 1 ms.

## Development

- `cargo run --release --example probe -- "query"` runs the headless playback path:
  search → resolve → download → decode → seek, with timings
- `cargo run --example apis` reports which rustypipe endpoints currently work
- `YTM_SMOKE="query" cargo run` searches and plays the first song on launch
