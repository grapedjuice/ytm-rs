### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = Clear cache?
dialog-cache-freed-size = This frees up { $size }.
dialog-cache-frees-space = This frees up space.
dialog-delete-playlist-title = Delete playlist?
dialog-delete-playlist = Delete playlist
dialog-delete-upload-title = Delete upload?
dialog-delete-upload = Delete upload
dialog-name = Name
dialog-my-playlist = My playlist
dialog-private-desc = Private · only you can see it
dialog-playlist-name = Playlist name
dialog-description = Description
dialog-optional-description = Add an optional description
dialog-visibility = Visibility
dialog-private = Private
dialog-unlisted = Unlisted
dialog-public = Public
dialog-visibility-prompt = Unchanged. Pick one to change who can see it.
dialog-turn-off = Turn off
dialog-stop-collaboration = Turn off collaboration?
dialog-stop-collaboration-desc = Collaborators will be removed from the playlist. Songs they added stay.
dialog-collaboration-allow = Collaborators can add songs.
dialog-collaboration-allow-private = Collaborators can add songs. Makes this private playlist unlisted.
dialog-new-collaborators = Allow new collaborators
dialog-new-collaborators-desc = Turn off to stop new people joining with the link.
dialog-copy-invite = Copy invite link
dialog-invite-copied = Invite link copied. Whoever opens it can add songs.
dialog-invite-instructions = Send the link to the people you want to add. They join from YouTube Music.
dialog-invites-off = New collaborators can't join. Turn on Allow new collaborators for an invite link.
dialog-collaborators = Collaborators
dialog-no-collaborators = No one has joined yet.
dialog-delete-playlist-body = “{ $title }” will be removed from your YouTube Music library. This can't be undone.
dialog-delete-upload-body = “{ $title }” is deleted from your YouTube Music uploads. This can't be undone.
dialog-download-count = Download ({ $count })
dialog-edit-playlist = Edit playlist
dialog-download-body = Tracks left to download: { $count }. Downloads run in the background and start with the current track.
dialog-download-collection-body = Tracks left to download from “{ $title }”: { $count }. Downloads run in the background and start with the current track.
dialog-add-duplicates-title = Already in the playlist
dialog-add-duplicates-some = Some of these songs are already in “{ $title }”. Add them anyway?
dialog-add-duplicates-body =
    { $count ->
        [one] “{ $songs }” is already in “{ $title }”. Add it anyway?
       *[other] { $more ->
            [0] These { $count } songs are already in “{ $title }”: { $songs }. Add them anyway?
           *[other] These { $count } songs are already in “{ $title }”: { $songs } and { $more } more. Add them anyway?
        }
    }
dialog-add-anyway = Add anyway
dialog-skip-duplicates = Skip duplicates
