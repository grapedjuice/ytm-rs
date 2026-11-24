### App interface.

status-upload-complete = Subida completada ({ $count }). YouTube Music puede tardar un minuto en procesarlas.
status-upload-failed = Error en la subida: { $details }
status-upload-partial = Subidas: { $count }. Errores: { $details }
status-waiting-transfer = Esperando a que la transferencia añada las canciones favoritas…
status-liking-progress = Añadiendo «Me gusta»: { $done } de { $total }…
status-unliking-progress = Quitando «Me gusta»: { $done } de { $total }…
status-playlist-created = Lista creada
status-new-playlist-tracks = Añadidas a la nueva lista: { $count }
status-playlist-added = Añadido a la lista
status-playlist-updated = Lista actualizada
status-playlist-sorted = Lista ordenada
status-playlist-sort-failed = No se pudo ordenar la lista
status-playlist-move-failed = YouTube Music no guardó el cambio de orden
status-library-saved = Guardado en la biblioteca
status-library-removed = Quitado de la biblioteca
status-unsubscribed = Suscripción cancelada
status-subscribe-failed = No se pudo suscribir
status-unsubscribe-failed = No se pudo cancelar la suscripción
status-like-success = «Me gusta» añadidos: { $count }
status-like-refused = YouTube Music no aceptó el cambio. Vuelve a intentarlo más tarde.
status-refused-count = Rechazadas por YouTube Music: { $count }
status-removed-liked = Quitado de las canciones favoritas
status-download-batch = Descargando canciones: { $count }
status-download-collection = Descargando de «{ $title }»: { $count }
status-download-stopped = Descarga detenida{ $name }. Guardadas: { $songs }
status-download-already = Ya está descargado
status-download-name-already = «{ $title }» ya está descargado
status-download-done = Descarga completa{ $name }: { $songs }
status-download-already-count = { $count } ya estaban
status-download-fail-count = No se pudieron descargar { $count } ({ $details })
status-play-premium = «{ $title }» requiere YouTube Music Premium
status-play-sign-in = Inicia sesión para reproducir «{ $title }»
status-age-restricted = tiene restricción de edad
status-songs = { $count ->
    [one] { $count } canción
   *[other] { $count } canciones
}
status-episodes = { $count ->
    [one] { $count } episodio
   *[other] { $count } episodios
}
status-download-quoted-title = «{ $title }»
discord-view-artist = Ir al artista
discord-view-album = Ir al álbum
