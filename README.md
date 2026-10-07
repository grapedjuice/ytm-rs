# ytm-rs

A native YouTube Music desktop client written in Rust. No Electron, no webview, no
bundled browser engine. It's inspired by [spotifast](https://github.com/crmne/spotifast)
(egui Spotify client) and [Pear Desktop](https://github.com/pear-devs/pear-desktop).

## Features

- Your real YouTube Music home feed when signed in: Listen again, Quick picks, mixes and mood chips, with infinite scroll
- Explore (new releases, charts), search with live suggestions, album/playlist/artist pages, and a library with liked songs, playlists, albums and history
- Time-synced lyrics from the Better Lyrics API (Musixmatch, LRCLIB, QQ, KuGou and Better Lyrics' own syllable-synced TTML), shown better-lyrics style: word-by-word fill, background vocals, duet alignment, instrumental-break dots, click a line to seek
- An animated album-art background: a port of Kawarp, the effect behind better-lyrics-shaders, which pulses with the music
- A now-playing view with full-screen lyrics (`F`), a queue drawer, radio and autoplay, shuffle and repeat, and liking songs
- OS media controls: Windows SMTC, MPRIS on Linux, Now Playing on macOS (via souvlaki)
- Every control has an accessible name, so the app works with screen readers through AccessKit

Lyrics access uses the same Cloudflare Turnstile check as the browser extension. The API's challenge page runs in a hidden WebView2 window, where it normally passes invisibly; if Cloudflare asks for a click, the window is shown. The resulting token lasts 24 h.

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

**Age-restricted songs.** YouTube serves these only to signed-in web players, and their stream URLs are signature-ciphered, so YouTube's JS player has to decode them. For just those tracks, the app falls back to [yt-dlp](https://github.com/yt-dlp/yt-dlp), which needs Node. yt-dlp is downloaded into the data folder on first use and refreshed weekly, and the session goes in as a cookies file that exists only for that call. Everything else stays pure Rust.

**Prefetch.** Once a track starts, the next one in the queue is resolved and
downloaded, so Next and auto-advance open a decoder in under a millisecond.

### Measured (Windows 11, Ryzen + RX 6750 XT, release build)

- Binary: about 15 MB (Inter and the Lucide icons are subset to roughly 300 KB)
- CPU: 0% idle; while playing, about 8% of one core on content pages (background at 15 fps) and about 6% in the now-playing view (30 fps). With the animated background off, the UI only ticks at 4 fps
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
- Dev hooks (environment variables): `YTM_SMOKE="query"` plays the first search hit, `YTM_SEARCH`, `YTM_PAGE=home|explore|library|settings|album:<id>|artist:<id>|playlist:<id>`, `YTM_NOWPLAYING=1`, `YTM_SEEK=<secs>`, `YTM_MUTE=1` (silent, volume isn't saved), `YTM_SCREENSHOT=out.png` (the app saves its own framebuffer), `YTM_NO_SHADER=1`
- `cargo run --example lyrics_check -- body.txt` summarises a saved Better Lyrics response

## Credits

Kawarp background (MIT © Better Lyrics) • Better Lyrics API • Inter (SIL OFL 1.1) • Lucide icons (ISC) • rustypipe • egui
