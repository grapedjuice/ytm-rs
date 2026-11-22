### The short notices at the bottom of the window.

likes-removed = { $count ->
    [one] Se quitó { $count } me gusta
   *[other] Se quitaron { $count } me gusta
}

## Recognize song

recognize-overline = Reconocer canción · Experimental
recognize-song = Reconocer canción
recognize-experimental = Experimental
recognize-waiting = Esperando sonido…
recognize-waiting-line = Reproduce la canción en otra aplicación
recognize-listening = Escuchando…
recognize-listening-line = Se oye: déjala sonando
recognize-identifying = Identificando…
recognize-identifying-line = Preguntando a Shazam
recognize-no-match = Sin resultados
recognize-no-match-line = Shazam no conoce esta canción
recognize-failed = No se pudo reconocer
recognize-unsupported = Este ordenador no puede grabar lo que reproduce
recognize-no-device = No hay ninguna salida de sonido que escuchar
recognize-nothing-playing = No se está reproduciendo nada
recognize-device-changed = La salida de sonido ha cambiado. Inténtalo de nuevo.
recognize-offline = No se pudo conectar con Shazam. Comprueba tu conexión.
recognize-no-answer = Shazam no ha respondido. Inténtalo de nuevo en un momento.
recognize-rate-limited = { $secs ->
    [one] Shazam necesita un descanso. Inténtalo de nuevo en { $secs } segundo.
   *[other] Shazam necesita un descanso. Inténtalo de nuevo en { $secs } segundos.
}
recognize-playing = Reproduciendo
recognize-play = Reproducir
recognize-playlist = Añadir a playlist
recognize-cancel = Cancelar
recognize-dismiss = Cerrar
