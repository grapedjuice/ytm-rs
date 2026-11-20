### Settings: the page's categories, sections, rows and their descriptions.
###
### Each line is a message: its id, then `=`, then the text. Translate the text only.
### Keep { $name }-style variables as they are; they can go anywhere in the sentence.
### The descriptions under a setting say what it does in a sentence or two; buttons and
### row titles have less room, so keep those short.

## The page and its categories

settings-playback = Playback
settings-lyrics = Lyrics
settings-appearance = Appearance
settings-connections = Connections
settings-account = Account
settings-general = General
settings-keyboard-shortcuts = Keyboard shortcuts
settings-about = About
settings-no-match = No settings match "{ $query }"
settings-title = Settings
settings-search-box = Search settings
settings-clear = Clear

## Account

settings-signed-in-desc = Signed in with Google. Your home feed, library and likes come from this account. Saved encrypted in Windows Credential Manager.
settings-sign-out = Sign out
settings-channel = Channel
settings-channel-desc = Your Google account and each YouTube channel it manages have their own library, likes, history and recommendations.
settings-not-signed-in = Not signed in
settings-not-signed-in-desc = Sign in to get your home feed, library and likes.
settings-sign-in-with-google = Sign in with Google
settings-sign-in-finish = Finish signing in in the window that opened.
settings-no-channel = { $name } (No channel)
settings-another-channel = Another channel
settings-channel-not-during-transfer = Not while a transfer runs
settings-switching = Switching…

## Playback

settings-autoplay = Autoplay
settings-autoplay-desc = Keep playing similar songs when the queue ends.
settings-crossfade-songs = Crossfade songs
settings-crossfade-songs-desc = Blend the end of each song into the start of the next.
settings-crossfade-length = Crossfade length
settings-crossfade-when-skipping = Crossfade when skipping
settings-crossfade-when-skipping-desc = Also fade into the next song when you skip ahead. Going back or picking a song still switches right away.
settings-hide-explicit-songs = Hide explicit songs
settings-hide-explicit-songs-desc = Leave songs marked E out of lists, search, the queue and autoplay. Ones already playing finish.
settings-show-podcasts = Show podcasts
settings-show-podcasts-desc = Podcast shows and episodes: a Podcasts filter on Home, in search results, and the ones you've saved in your library.
settings-show-comments = Show comments
settings-show-comments-desc = A Comments tab in now playing, beside Up next and Lyrics: what people say about the song, Top or Newest, with replies. Signed in, you can comment, reply and like too.
settings-use-youtubes-restricted-mode = Use YouTube's Restricted Mode
settings-use-youtubes-restricted-mode-desc = Stricter: YouTube also leaves out whatever else it finds mature, not just explicit songs. Applies to pages loaded from now on.

## Song ratings

settings-song-ratings = Song ratings
settings-ratings-player-button = Dislike button in the player
settings-ratings-player-button-desc = Show the thumbs-down button beside the heart.
settings-ratings-list-icons = Rating icons in song lists
settings-ratings-list-icons-desc = Show ratings beside the duration. Hover over an unrated song to reveal its Like button.
settings-ratings-dim-disliked = Dim disliked songs
settings-ratings-dim-disliked-desc = Show disliked songs in muted colors in song lists.
settings-ratings-skip-disliked = Dislike skips to the next track
settings-ratings-skip-disliked-desc = Advance after YouTube Music confirms the dislike. Guests in Listen together follow the host’s playback.
settings-ratings-dislike-notices = Dislike notifications
settings-ratings-dislike-notices-desc = Show a message when a dislike is saved or removed.
settings-ratings-unlike-notices = Like removal notifications
settings-ratings-unlike-notices-desc = Show an Undo button before sending the removal to YouTube Music.
settings-ratings-safe-likes = Safe liked songs
settings-ratings-safe-likes-desc = Hold the like button to remove a like.
settings-ratings-hold-to-remove = Hold to remove like

## Sound

settings-sound = Sound
settings-volume-normalization = Volume normalization
settings-logarithmic-volume = Logarithmic volume
settings-logarithmic-volume-desc = Space the volume slider out the way ears hear loudness, so quiet levels aren't squeezed into its first few pixels.
settings-fade-when-pausing = Fade when pausing
settings-fade-when-pausing-desc = Turn the music down when you pause, and back up when you play again, over a fifth of a second. Off, it stops and starts right away.
settings-high-quality-audio = High quality audio
settings-high-quality-audio-desc = 256 kbps instead of 128 kbps, for accounts with YouTube Premium. Applies from the next song.
settings-output-device = Output device
settings-output-device-desc = Where the music plays. A picked device that isn't connected leaves it to Windows' default until it's back.
settings-windows-default = Windows default
settings-not-connected = { $name } (not connected)
settings-normalization-off-desc = Songs play as loud as they were made. Normal brings them all to the same level, and Quiet to the same, lower one.
settings-normalization-normal-desc = Songs play at the same level: loud ones turned down, as on YouTube Music, and quiet ones up by as much as 6 dB.
settings-normalization-quiet-desc = Songs play at the same, lower level: loud ones turned down further, so quiet ones rarely need turning up.
settings-headphone-crossfeed = Headphone crossfeed
settings-headphone-crossfeed-desc = Let each ear hear a little of the other side's bass, as from speakers in front of you, so songs panned hard to one side are easier on the ears over headphones.
settings-mono-audio = Mono audio
settings-mono-audio-desc = Play both channels in each ear: for one earbud, or hearing in one ear.
settings-balance = Balance
settings-balance-desc = Move the sound towards the left or right ear. It stays as loud overall: the side it moves towards gets louder as the other gets quieter.
settings-centre = Centre

## Equalizer

settings-equalizer = Equalizer
settings-equalizer-desc = Shape the sound from deep bass to treble. Only what you hear changes: not downloads, or what others in a jam hear.
settings-preset = Preset
settings-custom = Custom
settings-equalizer-preset = Equalizer preset
# Not shown: words a search in Settings finds the equalizer by.
settings-equalizer-search-words = Equalizer bands bass mids treble frequency
settings-prevent-clipping = Prevent clipping
settings-prevent-clipping-desc = Turn everything down by as much as the bands boost, so loud songs are less likely to crackle.
settings-0-db = 0 dB

## Playback speed

settings-playback-speed = Playback speed
settings-playback-speed-desc = Play songs faster or slower, also from a button in the player bar. Lyrics stay in time. Back to 1× each time ytm-rs starts.
settings-speed = Speed
settings-keep-pitch = Keep the pitch
settings-keep-pitch-desc = Faster or slower without sounding higher or lower. Off, the pitch changes with the speed, like a record.
settings-playback-speed-jam-note = Back to normal while you're in a jam, so everyone hears the same song.

## Music videos

settings-music-videos = Music videos
settings-songs-not-videos = Play songs, not music videos
settings-songs-not-videos-desc = An official music video plays as its song, the album recording, when YouTube Music has one: no intros, skits or video-only edits. Lists still show what they hold.
settings-show-music-videos = Show music videos
settings-show-music-videos-desc = Music videos play as moving video, with their own sound and no lyrics. Turned off while songs play in their place, songs have no Song and Video switch in Now playing; music videos keep theirs.
settings-video-quality = Video quality
settings-video-quality-desc = Music videos play this sharp, or as close below it as the video comes. 1080p takes about three times the work of 720p, which a slower computer may not keep up with.
settings-ambient-light = Ambient light
settings-ambient-light-desc = Now playing glows with the video's colours around it.
settings-animated-covers = Animated covers
settings-animated-covers-desc = Some albums' covers move in Now playing, where Apple Music has a clip for them. Finding them sends each song's title and artist to Apple Music.
settings-hide-videos-in-search = Hide videos in search
settings-hide-videos-in-search-desc = Only songs, albums, artists and playlists in search results.
settings-skip-non-music-parts = Skip non-music parts
settings-skip-non-music-parts-desc = Jump over talking, skits and sponsor breaks in music videos, as marked by the SponsorBlock community. Album songs don't have any.

## Lyrics

settings-romanization = Romanization
settings-romanization-desc = Japanese, Korean, Chinese, Cyrillic and other scripts in Latin letters. Timed with the lyrics when the source has it.
settings-translation = Translation
settings-translation-desc = Translated by Unison, or Google when it can't. Lyrics that ship their own translation use it.
settings-translate-into = Translate into
settings-text-size = Text size
settings-text-size-desc = Make the lyrics larger or smaller. Works on top of the theme's own size.
settings-larger = Larger
settings-smaller = Smaller
settings-quick-line-changes = Quick line changes
settings-quick-line-changes-desc = The next line snaps into place quickly, the Better Lyrics way. Off, each theme moves its lines at its own pace.
settings-lyrics-full-frame-rate = Lyrics at the screen's refresh rate
settings-lyrics-full-frame-rate-desc = Synced lyrics animate as fast as your screen refreshes, such as 144 fps on a 144 Hz monitor, instead of 60. Smoother on fast screens, for more CPU and graphics work.

## Lyrics timing

settings-lyrics-timing = Lyrics timing
settings-timing-offset = Timing offset
settings-timing-offset-desc = Shift all lyrics later (+) or earlier (−).
settings-word-synced-trim = Word-synced trim
settings-word-synced-trim-desc = Extra shift for syllable and word-synced lyrics.
settings-line-synced-trim = Line-synced trim
settings-line-synced-trim-desc = Extra shift for line-synced lyrics.
settings-later = Later
settings-earlier = Earlier

## Lyrics sources

settings-lyrics-sources = Lyrics sources
settings-lyrics-sources-note = Tried in this order. Switch one off to skip it; the toolbar under the lyrics picks a different one for a single song.
settings-lower = Lower
settings-higher = Higher

## Appearance

settings-mode-desc-windows = Light or dark pages, in any theme. System follows Windows' app mode. Now playing stays dark, over the album art.
settings-mode-desc = Light or dark pages, in any theme. System follows your system's setting. Now playing stays dark, over the album art.
settings-mode = Mode
settings-accent-desc-file = Follows the colours in theme.conf: the accent, the text and, if you like, the background. Save the file and the app follows in a couple of seconds. wallust or pywal can write it too.
settings-accent-desc-windows = The theme's colour, your Windows accent colour, a colour file, or one you pick.
settings-accent-desc = The theme's colour, a colour file, or one you pick.
settings-accent-colour = Accent colour
settings-edit-file = Edit file
settings-custom-accent-colour = Custom accent colour
settings-accent-coloured-controls = Accent-coloured controls
settings-accent-coloured-controls-desc = The play button and volume slider in the accent colour, like the progress line.
settings-always-show-seek-knob = Always show the seek knob
settings-always-show-seek-knob-desc = The knob on the progress line stays up, not only when you point at it, in themes that have one.
settings-animated-background = Animated background
settings-animated-background-desc = The album art slowly drifts behind everything, in themes whose background moves.
settings-frosted-glass = Frosted glass
settings-frosted-glass-desc = Menus and floating bars blur what's behind them, in themes that use it. Off, they're a little more solid.
settings-pulse-with-the-music = Pulse with the music
settings-pulse-with-the-music-desc = The background swells with the beat, in themes whose background moves.
settings-dim-bright-covers = Dim bright covers
settings-dim-bright-covers-desc = Darkens the background when the album art is bright, so the lyrics and titles over it stay easy to read. Dark covers are left as they are.
settings-font = Font
settings-font-desc = For the whole app and the lyrics. Any font installed on this PC works, e.g. one made for your language.
settings-themes = Themes
settings-themes-note = Ported from the Better Lyrics community themes. Each one restyles the background, lyrics, cover, player bar and the rest of the app, and every setting above works in all of them.
settings-zoom = Zoom
settings-zoom-desc = Makes everything bigger or smaller. Ctrl + plus and Ctrl + minus do the same, and Ctrl + 0 goes back to 100%.
settings-window-material-desc = Windows 11's material behind the pages, in place of the theme's background.
settings-window-material-desc-needs-opengl = Windows 11's material behind the pages. Needs OpenGL, not Vulkan.
settings-window-material = Window material
settings-window-material-restart-desc = The window gets its new material from the next start.
settings-themes-font = Theme's font
settings-installed-on-this-pc = Installed on this PC
settings-looking-for-fonts = Looking for fonts…

## Mini player

settings-pill-mini-player-desc = Experimental. A small pill that shows its controls when you point at it. Double-click it for the full window.
settings-pill-mini-player-desc-needs-opengl = Experimental. A small pill that shows its controls when you point at it. Needs OpenGL, not Vulkan.
settings-mini-player = Mini player
settings-pill-mini-player = Pill mini player
settings-keep-on-top = Keep on top
settings-keep-on-top-desc = The mini player stays above other windows.
settings-pill-mini-player-restart-desc = The mini player changes style from the next start.

## App icon

settings-app-icon-note-linux = Changes the icon in the app menu, on the window and in the tray
settings-app-icon-note = Changes the icon on the window, taskbar, tray and Start menu
settings-app-icon = App icon
settings-preview = Preview
settings-preview-desc = How it looks on the taskbar and in the tray.
settings-app-icon-named = App icon: { $name }

## Connections

settings-stream-deck = Stream Deck
settings-stream-deck-desc = Let a Stream Deck plugin on this computer control playback.
settings-lastfm-connected = Scrobbling what you play to { $name }'s profile.
settings-lastfm-allow = Allow ytm-rs on the Last.fm page that opened in your browser.
settings-lastfm-desc = Scrobble what you play to your Last.fm profile.
settings-disconnect = Disconnect
settings-connect = Connect
settings-listenbrainz-connected = Sending what you play to { $name }'s profile.
settings-listenbrainz-desc = Send what you play to your ListenBrainz profile.
settings-user-token = User token
settings-listenbrainz-token-hint = Paste your ListenBrainz user token
settings-listenbrainz-token-link = Get your token on listenbrainz.org

## Language

# { $language } is the system's language, in its own name.
settings-language-system = System ({ $language })
settings-language = Language
settings-language-desc = The app's own words. Song titles, names and YouTube Music's shelves come in the Song info language below.
settings-restart-to-apply = Restart to apply
settings-language-restart-desc = The app changes language from the next start.
settings-restart-now = Restart now

## Song info: the language and country YouTube Music answers in. Separate from Language above.

settings-song-info = Song info
settings-song-info-language = Song info language
settings-song-info-language-desc = The language song titles, artist and album names and YouTube Music's shelves come in, where YouTube Music has them in it.
settings-song-info-country = Song info country
settings-song-info-country-desc = The country charts and recommendations are for.

## Window

settings-window = Window
settings-open-on = Open on
settings-open-on-desc = The page the app starts on. The Library's own tabs need you signed in, except Downloads; signed out, the app opens on Home.
settings-home = Home
settings-close-to-tray-desc-linux = Keeps an icon in the tray. Clicking × hides the window there and keeps the music playing. The icon's menu brings it back, hides it or quits. Clicking the song in the system's media controls brings it back too, and Ctrl+Q quits.
settings-close-to-tray-desc = Keeps an icon in the tray. Clicking × hides the window there and keeps the music playing. Click the icon to bring it back, or right-click it to hide, show or quit.
settings-close-to-tray = Close to tray
settings-song-notifications-desc = When a new song starts while the window is minimized, in the tray or behind other windows, show its title, artist and cover in a notification.
settings-song-notifications-unavailable = Needs the installed app: Windows only shows notifications from apps in the Start menu.
settings-song-notifications = Song notifications
settings-close-to-tray-restart-desc = On Wayland the window can only hide in the tray and come back after a restart. Until then × minimizes it.
settings-title-bar = Title bar
settings-title-bar-desc = Who draws the window's title bar and buttons. Automatic uses ytm-rs's where the desktop draws none (GNOME on Wayland).
settings-title-bar-restart-desc = The title bar changes from the next start.
settings-collapse-sidebar = Collapse sidebar
settings-collapse-sidebar-desc = Show only icons in the sidebar, for narrow windows. The menu button at its top does the same.
settings-draw-with = Draw with { $api }
settings-renderer-restart-desc = The app draws with the new renderer from the next start.

## Downloads

settings-downloads = Downloads
settings-downloads-note = Download songs from their menu, or whole albums and playlists from their page. They're saved at the best quality your account gets, tagged with their cover art, and play from the file, even offline.
settings-download-folder = Download folder
settings-change = Change
settings-open = Open
settings-folder-per-list = Put albums and playlists in their own folder
settings-folder-per-list-desc = Downloading an album or playlist from its page or menu saves its songs in a folder named after it. Songs downloaded already stay where they are.
settings-file-lyrics = Lyrics in the file
settings-file-lyrics-desc = Downloaded songs carry the lyrics you see for them, for other music players. Timed lyrics are saved as LRC.
settings-file-lyrics-synced = Timed
settings-file-lyrics-plain = Words only
settings-file-lyrics-off = None

## Network

settings-network = Network
settings-backup-stream = Backup stream when songs stop
settings-backup-stream-desc = Signed in, when YouTube stops a song with a bot check (this can happen on a VPN), play it from a stream that comes with the video. Uses more data.
settings-backup-stream-prompt-title = Playback stopped
settings-backup-stream-prompt-body = YouTube stopped this song with a bot check, which can happen on a VPN. A backup stream can play it instead. It comes with the video, so it uses more data.
settings-backup-stream-prompt-enable = Turn on and retry
settings-backup-stream-prompt-settings = Settings
settings-backup-stream-prompt-dismiss = Not now
settings-proxy = Proxy
settings-proxy-desc = Send everything ytm-rs fetches through a proxy: songs, lyrics, sign-in and Listen together.
settings-proxy-windows-uses = Windows is set to use { $proxy }.
settings-proxy-windows-none = Windows isn't set to use a proxy, so ytm-rs connects directly.
settings-proxy-environment-sets = The environment sets { $proxy }.
settings-proxy-environment-none = No HTTPS_PROXY or ALL_PROXY is set, so ytm-rs connects directly.
settings-windows-proxy = Windows proxy
settings-system-proxy = System proxy
settings-proxy-address = Proxy address
settings-address = Address
settings-saved = Saved
settings-save = Save
settings-proxy-password-note = A password in the address is kept in Windows Credential Manager.
settings-checking = Checking…
settings-proxy-test-desc = Checks that YouTube Music answers through this setting.
settings-test-connection = Test connection
settings-test = Test
settings-test-working = Working: YouTube Music answered in { $ms } ms.
settings-test-failed = Didn't connect: { $error }
settings-proxy-restart-desc = The new proxy setting is used from the next start.

## Storage

settings-cache-cleared = Cache cleared: { $size } freed
settings-storage = Storage
settings-clearing = Clearing…
settings-measuring = Measuring…
settings-cache = Cache
settings-clear-cache = Clear cache

## Keyboard shortcuts

settings-play-pause = Play / pause
settings-space = Space
settings-next-previous-song = Next / previous song
settings-ctrl = Ctrl
settings-volume-up-down = Volume up / down
settings-shortcut-command-palette = Command palette: pages, settings, playlists and actions
settings-search = Search
settings-shortcut-find-in-list = Find in a song list
settings-shift = Shift
settings-lyrics-and-now-playing = Lyrics and now playing
settings-shortcut-full-screen = Full screen in now playing: the lyrics, or a video
settings-shortcut-video-fill = A full screen video: fill the screen, or fit it
settings-mini-player-on-off = Mini player on / off
settings-back-forward = Back / forward
settings-alt = Alt
settings-shortcut-back-forward-mouse = Back / forward with the mouse
settings-side-buttons = Side buttons
settings-refresh-the-page = Refresh the page
settings-f5-ctrl-r = F5 / Ctrl + R
settings-zoom-in-out = Zoom in / out
settings-shortcut-zoom-reset = Zoom back to 100%
settings-shortcut-leave = Leave full screen, now playing or selecting
settings-esc = Esc

## Updates

settings-updates = Updates
settings-updates-dev-build = Updates only work in the installed or portable app (this is a development build).
settings-checking-for-updates = Checking for updates…
settings-up-to-date = You're on the latest version ({ $version }).
settings-update-downloading = Downloading { $version }… { $percent }%
settings-update-ready = Version { $version } is ready. It installs when you close ytm-rs.
settings-update-available = Version { $version } is out. { $step }
settings-update-failed = Couldn't check for updates: { $error }
settings-version = Version { $version }.
settings-automatic-updates-desc-packaged = Check for new versions and tell you when one is out.
settings-automatic-updates-desc = Download new versions in the background and install them when you close the app.
settings-automatic-updates = Automatic updates
settings-update-channel = Update channel
settings-nightly = Nightly
settings-stable = Stable
settings-status = Status
settings-restart-to-update = Restart to update
settings-download = Download
settings-check-now = Check now
settings-another-copy = Another copy
settings-another-copy-desc = ytm-rs is also installed from { $kind }. To remove that copy, run { $cmd }
settings-whats-new = What's new
settings-whats-new-in = What's new in { $version }

## About

settings-about-tagline = A fast, native YouTube Music client.
settings-about-lyrics-sources = Better Lyrics API: Musixmatch, LRCLIB, QQ, KuGou and Better Lyrics TTML
settings-about-themes-credit = Adapted from the Better Lyrics theme store; each card credits its creators
settings-background = Background
settings-about-background-credit = A port of Kawarp, MIT © Better Lyrics
settings-fonts-and-icons = Fonts and icons
settings-about-fonts-credit = Inter, DM Sans, Google Sans Flex, Figtree, Instrument Sans (SIL OFL 1.1) • Lucide (ISC)
# { $shown } is { $count } written with its thousands separators.
settings-downloads-and-counting = { $count ->
    [one] { $shown } download and counting
   *[other] { $shown } downloads and counting
}
settings-counting-downloads = Counting downloads…
