### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] { $count } like verwijderd
   *[other] { $count } likes verwijderd
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Afspelen op { $name } lukt niet: { $reason }
airplay-unreachable = het was niet bereikbaar
airplay-busy = het is bezig met afspelen vanaf een ander apparaat
airplay-needs-pairing = het wil een wachtwoord of een koppeling, en dat wordt nog niet ondersteund
airplay-pairing-refused = het wilde niet koppelen (op een Apple TV moet Instellingen › AirPlay en HomeKit › Toegang toestaan op iedereen op hetzelfde netwerk staan)
airplay-no-clock = het stuurde zijn klok niet (een firewall blokkeert mogelijk UDP-poorten 319 en 320)
airplay-clock-ports = zijn klok komt binnen op UDP-poorten 319 en 320, die een andere app gebruikt (een andere AirPlay-app of een PTP-dienst)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = zijn klok komt binnen op UDP-poorten 319 en 320, die Linux voor root bewaart (sta ze toe met "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", of geef de app CAP_NET_BIND_SERVICE), of een andere app gebruikt ze
airplay-failed = er ging iets mis
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay-speaker kwijt

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Nummer herkennen · Experimenteel
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Nummer herkennen
recognize-experimental = Experimenteel
recognize-waiting = Wachten op geluid…
recognize-waiting-line = Speel het nummer af in een andere app
recognize-listening = Luisteren…
recognize-listening-line = Te horen, laat het doorspelen
recognize-identifying = Herkennen…
recognize-identifying-line = Shazam vragen
recognize-no-match = Geen treffer
recognize-no-match-line = Shazam kent deze niet
recognize-failed = Herkennen lukt niet
recognize-unsupported = Deze computer kan niet opnemen wat hij afspeelt
recognize-headset-output = Een headset kan niet worden afgeluisterd. Zet de geluidsuitvoer op je speakers en probeer het opnieuw.
recognize-no-device = Er is geen geluidsuitvoer om naar te luisteren
recognize-nothing-playing = Er speelt niets
recognize-device-changed = De geluidsuitvoer is veranderd. Probeer het opnieuw.
recognize-offline = Shazam niet bereikbaar. Controleer je verbinding.
recognize-no-answer = Shazam antwoordde niet. Probeer het zo weer.
recognize-rate-limited = { $secs ->
    [one] Shazam heeft een pauze nodig. Probeer het over { $secs } seconde opnieuw.
   *[other] Shazam heeft een pauze nodig. Probeer het over { $secs } seconden opnieuw.
}
# Buttons on the card.
recognize-playing = Speelt nu
recognize-play = Afspelen
recognize-playlist = Toevoegen aan afspeellijst
recognize-cancel = Annuleren
recognize-dismiss = Sluiten
