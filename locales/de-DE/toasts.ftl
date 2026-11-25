### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] { $count } Like entfernt
   *[other] { $count } Likes entfernt
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Wiedergabe auf { $name } nicht möglich: { $reason }
airplay-unreachable = es war nicht erreichbar
airplay-busy = es spielt gerade von einem anderen Gerät
airplay-needs-pairing = es verlangt ein Passwort oder eine Kopplung, was noch nicht unterstützt wird
airplay-pairing-refused = es wollte sich nicht koppeln (bei einem Apple TV muss unter Einstellungen › AirPlay und HomeKit › Zugriff erlauben „Jeder im selben Netzwerk“ stehen)
airplay-no-clock = es hat seine Uhr nicht gesendet (eine Firewall blockiert möglicherweise die UDP-Ports 319 und 320)
airplay-clock-ports = seine Uhr läuft über die UDP-Ports 319 und 320, die eine andere App benutzt (eine andere AirPlay-App oder ein PTP-Dienst)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = seine Uhr läuft über die UDP-Ports 319 und 320, die Linux root vorbehält (erlauben mit "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" oder der App CAP_NET_BIND_SERVICE geben), oder eine andere App benutzt sie
airplay-failed = etwas ist schiefgelaufen
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Verbindung zum AirPlay-Lautsprecher verloren

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Titel erkennen · Experimentell
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Titel erkennen
recognize-experimental = Experimentell
recognize-waiting = Warte auf Ton…
recognize-waiting-line = Spiel den Titel in einer anderen App ab
recognize-listening = Höre zu…
recognize-listening-line = Zu hören, lass es weiterlaufen
recognize-identifying = Erkennen…
recognize-identifying-line = Frage Shazam
recognize-no-match = Kein Treffer
recognize-no-match-line = Shazam kennt den nicht
recognize-failed = Erkennen fehlgeschlagen
recognize-unsupported = Dieser Computer kann nicht aufnehmen, was er abspielt
recognize-headset-output = Ein Headset kann nicht abgehört werden. Stelle die Tonausgabe auf die Lautsprecher und versuche es erneut.
recognize-no-device = Es gibt keine Tonausgabe zum Abhören
recognize-nothing-playing = Es spielt nichts
recognize-device-changed = Die Tonausgabe hat sich geändert. Versuch es noch einmal.
recognize-offline = Shazam nicht erreichbar. Prüfe deine Verbindung.
recognize-no-answer = Shazam hat nicht geantwortet. Versuch es gleich noch einmal.
recognize-rate-limited = { $secs ->
    [one] Shazam braucht eine Pause. Versuch es in { $secs } Sekunde noch einmal.
   *[other] Shazam braucht eine Pause. Versuch es in { $secs } Sekunden noch einmal.
}
# Buttons on the card.
recognize-playing = Aktuelle Wiedergabe
recognize-play = Abspielen
recognize-playlist = Zu Playlist hinzufügen
recognize-cancel = Abbrechen
recognize-dismiss = Ausblenden
