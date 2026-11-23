### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Cofnięto { $count } polubienie
    [few] Cofnięto { $count } polubienia
    [many] Cofnięto { $count } polubień
   *[other] Cofnięto { $count } polubienia
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Nie można odtworzyć na { $name }: { $reason }
airplay-unreachable = nie można było go osiągnąć
airplay-busy = jest zajęty odtwarzaniem z innego urządzenia
airplay-needs-pairing = chce hasła albo parowania, a tego jeszcze nie obsługujemy
airplay-pairing-refused = nie chciał się sparować (w Apple TV ustawienia › AirPlay i HomeKit › Pozwól na dostęp muszą być ustawione na każdego w tej samej sieci)
airplay-no-clock = nie wysłał swojego zegara (zapora może blokować porty UDP 319 i 320)
airplay-clock-ports = jego zegar idzie portami UDP 319 i 320, których używa inna aplikacja (inna aplikacja AirPlay albo usługa PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = jego zegar idzie portami UDP 319 i 320, które Linux zostawia rootowi (zezwól przez "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" albo nadaj aplikacji CAP_NET_BIND_SERVICE), albo używa ich inna aplikacja
airplay-failed = coś poszło nie tak
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Utracono głośnik AirPlay

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Rozpoznaj utwór · Eksperymentalne
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Rozpoznaj utwór
recognize-experimental = Eksperymentalne
recognize-waiting = Czekam na dźwięk…
recognize-waiting-line = Odtwórz utwór w innej aplikacji
recognize-listening = Słucham…
recognize-listening-line = Słychać, niech gra dalej
recognize-identifying = Rozpoznawanie…
recognize-identifying-line = Pytam Shazam
recognize-no-match = Brak trafienia
recognize-no-match-line = Shazam nie zna tego utworu
recognize-failed = Nie udało się rozpoznać
recognize-unsupported = Ten komputer nie może nagrać tego, co odtwarza
recognize-headset-output = Nie da się podsłuchać słuchawek. Przełącz wyjście dźwięku na głośniki i spróbuj ponownie.
recognize-no-device = Nie ma wyjścia dźwięku do podsłuchania
recognize-nothing-playing = Nic nie gra
recognize-device-changed = Wyjście dźwięku się zmieniło. Spróbuj ponownie.
recognize-offline = Nie można połączyć się z Shazam. Sprawdź połączenie.
recognize-no-answer = Shazam nie odpowiedział. Spróbuj za chwilę.
recognize-rate-limited = { $secs ->
    [one] Shazam potrzebuje przerwy. Spróbuj ponownie za { $secs } sekundę.
    [few] Shazam potrzebuje przerwy. Spróbuj ponownie za { $secs } sekundy.
    [many] Shazam potrzebuje przerwy. Spróbuj ponownie za { $secs } sekund.
   *[other] Shazam potrzebuje przerwy. Spróbuj ponownie za { $secs } sekundy.
}
# Buttons on the card.
recognize-playing = Teraz odtwarzane
recognize-play = Odtwórz
recognize-playlist = Dodaj do playlisty
recognize-cancel = Anuluj
recognize-dismiss = Zamknij
