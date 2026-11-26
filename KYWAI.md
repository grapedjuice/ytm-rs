# ytm-rs for kywai

This branch is the copy of ytm-rs that [kywai](https://kywai.app) downloads and runs inside itself. It is
[grapedjuice/ytm-rs](https://github.com/grapedjuice/ytm-rs) (GPL-3.0-only) with these changes, kept small so they can
be offered upstream:

- **Remote control commands:** `play` and `queue` (a YouTube video ID), `seek` (seconds), `lyrics` (optionally with a
  language: the shown lyrics, translated, as their own `{"lyrics":…}` frame, now and on every change), `show` (bring the
  window up) and `quit`. Snapshots also carry `video_id` and `position_ms`, and `commands` lists the extras.
- **`YTM_EMBEDDED`:** run by another app. The window starts hidden, × hides it, the remote control is always on (on
  `YTM_REMOTE_PORT` if set) and the copy doesn't update itself: the app that runs it picks the version.

## Releases

Tags can't be pushed from where kywai's changes are made, so a release is made by hand: Actions › Release › Run
workflow on this branch, with `kywai_tag` set to `v<version>-kywai.<n>`. It builds the usual files for every platform
and publishes them as a prerelease with that tag (never the latest release). kywai pins that tag and the SHA-256 of
each portable download.
