### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Снят { $count } лайк
    [few] Снято { $count } лайка
    [many] Снято { $count } лайков
   *[other] Снято { $count } лайка
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Не удалось воспроизвести на { $name }: { $reason }
airplay-unreachable = до него не удалось достучаться
airplay-busy = он занят воспроизведением с другого устройства
airplay-needs-pairing = он просит пароль или сопряжение, а это пока не поддерживается
airplay-pairing-refused = он отказался сопрягаться (на Apple TV в Настройки › AirPlay и HomeKit › Разрешить доступ должно быть «Все в одной сети»)
airplay-no-clock = он не прислал свои часы (брандмауэр может блокировать UDP-порты 319 и 320)
airplay-clock-ports = его часы идут по UDP-портам 319 и 320, которые заняты другим приложением (другим приложением AirPlay или службой PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = его часы идут по UDP-портам 319 и 320, которые Linux оставляет root (разрешите их командой "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" или дайте приложению CAP_NET_BIND_SERVICE), либо их занимает другое приложение
airplay-failed = что-то пошло не так
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Колонка AirPlay пропала

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Распознать трек · Экспериментально
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Распознать трек
recognize-experimental = Экспериментально
recognize-waiting = Жду звук…
recognize-waiting-line = Включите трек в другом приложении
recognize-listening = Слушаю…
recognize-listening-line = Слышно, пусть играет
recognize-identifying = Определяю…
recognize-identifying-line = Спрашиваю Shazam
recognize-no-match = Нет совпадения
recognize-no-match-line = Shazam не знает этот трек
recognize-failed = Не удалось распознать
recognize-unsupported = Этот компьютер не может записать то, что играет
recognize-headset-output = Гарнитуру прослушать нельзя. Переключите вывод звука на колонки и попробуйте снова.
recognize-no-device = Нет вывода звука, который можно прослушать
recognize-nothing-playing = Ничего не играет
recognize-device-changed = Вывод звука изменился. Попробуйте ещё раз.
recognize-offline = Не удалось связаться с Shazam. Проверьте соединение.
recognize-no-answer = Shazam не ответил. Попробуйте через минуту.
recognize-rate-limited = { $secs ->
    [one] Shazam нужна пауза. Попробуйте через { $secs } секунду.
    [few] Shazam нужна пауза. Попробуйте через { $secs } секунды.
    [many] Shazam нужна пауза. Попробуйте через { $secs } секунд.
   *[other] Shazam нужна пауза. Попробуйте через { $secs } секунды.
}
# Buttons on the card.
recognize-playing = Сейчас играет
recognize-play = Воспроизвести
recognize-playlist = Добавить в плейлист
recognize-cancel = Отмена
recognize-dismiss = Закрыть
