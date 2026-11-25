### App interface.

status-upload-complete = Upload complete ({ $count }). YouTube Music may take a minute to process the tracks.
status-upload-failed = Upload failed: { $details }
status-upload-partial = Uploaded: { $count }. Failed: { $details }
status-waiting-transfer = Waiting for the transfer to finish adding to Liked Music…
status-liking-progress = Adding likes: { $done } of { $total }…
status-unliking-progress = Removing likes: { $done } of { $total }…
status-playlist-created = Playlist created
status-new-playlist-tracks = Tracks added to new playlist: { $count }
status-playlist-added = Added to playlist
status-playlist-updated = Playlist updated
status-playlist-sorted = Playlist sorted
status-playlist-sort-failed = Couldn't sort the playlist
status-playlist-move-failed = YouTube Music didn't take that move
status-library-saved = Saved to library
status-library-removed = Removed from library
status-unsubscribed = Unsubscribed
status-subscribe-failed = Couldn't subscribe
status-unsubscribe-failed = Couldn't unsubscribe
status-like-success = Likes added: { $count }
status-like-refused = YouTube Music didn't accept the change. Try again in a while.
status-refused-count = YouTube Music refused: { $count }
status-removed-liked = Removed from liked songs
status-download-batch = Downloading tracks: { $count }
status-download-collection = Downloading from “{ $title }”: { $count }
status-download-stopped = Stopped downloading{ $name }: { $songs } saved
status-download-already = Downloaded already
status-download-name-already = “{ $title }” is downloaded already
status-download-done = Downloaded{ $name }: { $songs }
status-download-already-count = { $count } already there
status-download-fail-count = { $count } couldn't be downloaded ({ $details })
status-play-premium = “{ $title }” needs YouTube Music Premium
status-play-sign-in = Sign in to play “{ $title }”
status-age-restricted = it's age-restricted
status-songs = { $count ->
    [one] { $count } song
   *[other] { $count } songs
}
status-episodes = { $count ->
    [one] { $count } episode
   *[other] { $count } episodes
}
status-plays = { $count } plays
status-subscribers = { $count } subscribers
status-download-quoted-title = “{ $title }”
discord-view-artist = View artist
discord-view-album = View album
status-kept-downloaded = Kept downloaded: songs added to this playlist are downloaded too
status-not-kept-downloaded = No longer kept downloaded. Songs downloaded already stay
status-resumed-at = Picking up where you left off, at { $time }
