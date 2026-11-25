### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Se quitó { $count } me gusta
   *[other] Se quitaron { $count } me gusta
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = No se pudo reproducir en { $name }: { $reason }
airplay-unreachable = no se pudo alcanzar
airplay-busy = está ocupado reproduciendo desde otro dispositivo
airplay-needs-pairing = pide una contraseña o un emparejamiento, que aún no están admitidos
airplay-pairing-refused = no quiso emparejarse (en un Apple TV, Ajustes › AirPlay y HomeKit › Permitir acceso debe estar en cualquiera de la misma red)
airplay-no-clock = no envió su reloj (un cortafuegos puede estar bloqueando los puertos UDP 319 y 320)
airplay-clock-ports = su reloj usa los puertos UDP 319 y 320, que está usando otra app (otra app de AirPlay o un servicio PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = su reloj usa los puertos UDP 319 y 320, que Linux reserva para root (permítelos con "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", o dale a la app CAP_NET_BIND_SERVICE), u otra app los está usando
airplay-failed = algo salió mal
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Se perdió el altavoz AirPlay

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Reconocer canción · Experimental
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
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
recognize-headset-output = No puede escuchar unos auriculares. Cambia la salida de sonido a los altavoces e inténtalo de nuevo.
recognize-no-device = No hay ninguna salida de sonido que escuchar
recognize-nothing-playing = No se está reproduciendo nada
recognize-device-changed = La salida de sonido ha cambiado. Inténtalo de nuevo.
recognize-offline = No se pudo conectar con Shazam. Comprueba tu conexión.
recognize-no-answer = Shazam no ha respondido. Inténtalo de nuevo en un momento.
recognize-rate-limited = { $secs ->
    [one] Shazam necesita un descanso. Inténtalo de nuevo en { $secs } segundo.
   *[other] Shazam necesita un descanso. Inténtalo de nuevo en { $secs } segundos.
}
# Buttons on the card.
recognize-playing = Reproduciendo
recognize-play = Reproducir
recognize-playlist = Añadir a playlist
recognize-cancel = Cancelar
recognize-dismiss = Cerrar
