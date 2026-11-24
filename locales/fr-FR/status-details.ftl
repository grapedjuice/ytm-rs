### App interface.

status-upload-complete = Importation terminée ({ $count }). YouTube Music peut prendre une minute pour traiter les titres.
status-upload-failed = Échec de l’importation : { $details }
status-upload-partial = Importés : { $count }. Échecs : { $details }
status-waiting-transfer = En attente de la fin du transfert des titres aimés…
status-liking-progress = Ajout de « J’aime » : { $done } sur { $total }…
status-unliking-progress = Retrait des « J’aime » : { $done } sur { $total }…
status-playlist-created = Playlist créée
status-new-playlist-tracks = Titres ajoutés à la nouvelle playlist : { $count }
status-playlist-added = Ajouté à la playlist
status-playlist-updated = Playlist mise à jour
status-playlist-sorted = Playlist triée
status-playlist-sort-failed = Impossible de trier la playlist
status-playlist-move-failed = YouTube Music n’a pas enregistré le déplacement
status-library-saved = Enregistré dans la bibliothèque
status-library-removed = Retiré de la bibliothèque
status-unsubscribed = Abonnement annulé
status-subscribe-failed = Impossible de s’abonner
status-unsubscribe-failed = Impossible d’annuler l’abonnement
status-like-success = « J’aime » ajoutés : { $count }
status-like-refused = YouTube Music n’a pas accepté la modification. Réessayez dans un instant.
status-refused-count = Refusés par YouTube Music : { $count }
status-removed-liked = Retiré des titres aimés
status-download-batch = Téléchargement de titres : { $count }
status-download-collection = Téléchargements depuis « { $title } » : { $count }
status-download-stopped = Téléchargement{ $name } arrêté : { $songs } enregistrés
status-download-already = Déjà téléchargé
status-download-name-already = « { $title } » est déjà téléchargé
status-download-done = Téléchargé{ $name } : { $songs }
status-download-already-count = { $count } déjà présents
status-download-fail-count = { $count } n’ont pas pu être téléchargés ({ $details })
status-play-premium = « { $title } » nécessite YouTube Music Premium
status-play-sign-in = Connectez-vous pour lire « { $title } »
status-age-restricted = soumis à une limite d’âge
status-songs = { $count ->
    [one] { $count } titre
   *[other] { $count } titres
}
status-episodes = { $count ->
    [one] { $count } épisode
   *[other] { $count } épisodes
}
status-download-quoted-title = « { $title } »
discord-view-artist = Aller à l’artiste
discord-view-album = Aller à l’album
