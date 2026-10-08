# Code signing policy

Windows releases are built from this repository by [GitHub Actions](.github/workflows/release.yml); nothing is built or signed on a personal machine.

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

## Team

- Committers and reviewers: [grapedjuice](https://github.com/grapedjuice)
- Approvers: [grapedjuice](https://github.com/grapedjuice)

## Privacy

ytm-rs only talks to the services it needs to play your music:

- **YouTube Music and Google** (`music.youtube.com`, `youtube.com`, `googlevideo.com`, `accounts.google.com`) for browsing, playback, likes, playlists and sign-in. Your sign-in cookie stays on your PC.
- **Better Lyrics** (`lyrics.api.dacubeking.com`) receives the song's YouTube ID, title, artist, album and length to find synced lyrics.
- **GitHub** (`github.com`) to download `yt-dlp` when an age-restricted song is played, and to refresh it weekly after that.
- **Discord**, through the Discord app on your PC, when the "Show what I'm playing on Discord" setting is on.

It sends no analytics or telemetry, and nothing goes to the developer.
