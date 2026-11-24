### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Знято { $count } вподобайку
    [few] Знято { $count } вподобайки
    [many] Знято { $count } вподобайок
   *[other] Знято { $count } вподобайки
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Не вдалося відтворити на { $name }: { $reason }
airplay-unreachable = до нього не вдалося достукатися
airplay-busy = він зайнятий відтворенням з іншого пристрою
airplay-needs-pairing = він просить пароль або з’єднання, а це ще не підтримується
airplay-pairing-refused = він відмовився з’єднуватися (на Apple TV у Параметри › AirPlay і HomeKit › Дозволити доступ має бути «Будь-хто в тій самій мережі»)
airplay-no-clock = він не надіслав свій годинник (брандмауер може блокувати UDP-порти 319 і 320)
airplay-clock-ports = його годинник іде через UDP-порти 319 і 320, які зайняті іншим застосунком (іншим застосунком AirPlay або службою PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = його годинник іде через UDP-порти 319 і 320, які Linux лишає root (дозвольте їх командою "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" або дайте застосунку CAP_NET_BIND_SERVICE), або їх займає інший застосунок
airplay-failed = щось пішло не так
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Колонку AirPlay втрачено

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Розпізнати трек · Експериментально
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Розпізнати трек
recognize-experimental = Експериментально
recognize-waiting = Чекаю на звук…
recognize-waiting-line = Увімкніть трек в іншому застосунку
recognize-listening = Слухаю…
recognize-listening-line = Чути, хай грає далі
recognize-identifying = Визначаю…
recognize-identifying-line = Питаю Shazam
recognize-no-match = Немає збігу
recognize-no-match-line = Shazam не знає цей трек
recognize-failed = Не вдалося розпізнати
recognize-unsupported = Цей комп’ютер не може записати те, що відтворює
recognize-headset-output = Гарнітуру прослухати не можна. Перемкніть вивід звуку на колонки й спробуйте ще раз.
recognize-no-device = Немає виводу звуку, який можна прослухати
recognize-nothing-playing = Нічого не грає
recognize-device-changed = Вивід звуку змінився. Спробуйте ще раз.
recognize-offline = Не вдалося зв’язатися з Shazam. Перевірте з’єднання.
recognize-no-answer = Shazam не відповів. Спробуйте за мить.
recognize-rate-limited = { $secs ->
    [one] Shazam потрібна перерва. Спробуйте за { $secs } секунду.
    [few] Shazam потрібна перерва. Спробуйте за { $secs } секунди.
    [many] Shazam потрібна перерва. Спробуйте за { $secs } секунд.
   *[other] Shazam потрібна перерва. Спробуйте за { $secs } секунди.
}
# Buttons on the card.
recognize-playing = Зараз грає
recognize-play = Відтворити
recognize-playlist = Додати до списку відтворення
recognize-cancel = Скасувати
recognize-dismiss = Закрити
