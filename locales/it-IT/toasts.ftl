### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Rimosso { $count } «Mi piace»
   *[other] Rimossi { $count } «Mi piace»
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Impossibile riprodurre su { $name }: { $reason }
airplay-unreachable = non era raggiungibile
airplay-busy = è occupato a riprodurre da un altro dispositivo
airplay-needs-pairing = chiede una password o un abbinamento, non ancora supportati
airplay-pairing-refused = non ha voluto abbinarsi (su una Apple TV, Impostazioni › AirPlay e HomeKit › Consenti accesso deve essere chiunque sulla stessa rete)
airplay-no-clock = non ha inviato il suo orologio (un firewall potrebbe bloccare le porte UDP 319 e 320)
airplay-clock-ports = il suo orologio usa le porte UDP 319 e 320, già usate da un’altra app (un’altra app AirPlay o un servizio PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = il suo orologio usa le porte UDP 319 e 320, che Linux riserva a root (consentile con "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", oppure dai all’app CAP_NET_BIND_SERVICE), o le sta usando un’altra app
airplay-failed = qualcosa è andato storto
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Diffusore AirPlay perso

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Riconosci brano · Sperimentale
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Riconosci brano
recognize-experimental = Sperimentale
recognize-waiting = In attesa del suono…
recognize-waiting-line = Riproduci il brano in un’altra app
recognize-listening = In ascolto…
recognize-listening-line = Si sente, lascialo andare
recognize-identifying = Identificazione…
recognize-identifying-line = Chiedo a Shazam
recognize-no-match = Nessuna corrispondenza
recognize-no-match-line = Shazam non conosce questo
recognize-failed = Impossibile riconoscerlo
recognize-unsupported = Questo computer non può registrare ciò che riproduce
recognize-headset-output = Non può ascoltare una cuffia. Passa l’uscita audio agli altoparlanti e riprova.
recognize-no-device = Non c’è un’uscita audio da ascoltare
recognize-nothing-playing = Non sta suonando nulla
recognize-device-changed = L’uscita audio è cambiata. Riprova.
recognize-offline = Shazam non raggiungibile. Controlla la connessione.
recognize-no-answer = Shazam non ha risposto. Riprova tra un momento.
recognize-rate-limited = { $secs ->
    [one] Shazam ha bisogno di una pausa. Riprova tra { $secs } secondo.
   *[other] Shazam ha bisogno di una pausa. Riprova tra { $secs } secondi.
}
# Buttons on the card.
recognize-playing = In riproduzione
recognize-play = Riproduci
recognize-playlist = Aggiungi alla playlist
recognize-cancel = Annulla
recognize-dismiss = Chiudi
