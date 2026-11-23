### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Removido { $count } gosto
   *[other] Removidos { $count } gostos
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Não foi possível tocar em { $name }: { $reason }
airplay-unreachable = não deu para alcançar
airplay-busy = está ocupado a tocar de outro aparelho
airplay-needs-pairing = pede uma palavra-passe ou um pareamento, que ainda não são suportados
airplay-pairing-refused = não quis parear (num Apple TV, Ajustes › AirPlay e HomeKit › Permitir acesso precisa estar em qualquer pessoa na mesma rede)
airplay-no-clock = não enviou o relógio (um firewall pode estar a bloquear as portas UDP 319 e 320)
airplay-clock-ports = o relógio dele usa as portas UDP 319 e 320, que outro app está a usar (outro app de AirPlay ou um serviço PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = o relógio dele usa as portas UDP 319 e 320, que o Linux reserva para o root (libere com "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", ou dê à aplicação CAP_NET_BIND_SERVICE), ou outro app está a usar
airplay-failed = algo deu errado
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = O altifalante AirPlay caiu

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Reconhecer música · Experimental
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Reconhecer música
recognize-experimental = Experimental
recognize-waiting = A aguardar som…
recognize-waiting-line = Toque a música em outro app
recognize-listening = A ouvir…
recognize-listening-line = É possível ouvir, deixe a tocar
recognize-identifying = A identificar…
recognize-identifying-line = A perguntar ao Shazam
recognize-no-match = Nenhum resultado
recognize-no-match-line = O Shazam não conhece esta música
recognize-failed = Não foi possível reconhecer
recognize-unsupported = Este computador não consegue gravar o que toca
recognize-headset-output = Não é possível ouvir um headset. Mude a saída de som para os altifalantes e tente de novo.
recognize-no-device = Não há saída de som para ouvir
recognize-nothing-playing = Nada está a tocar
recognize-device-changed = A saída de som mudou. Tente de novo.
recognize-offline = Não foi possível aceder ao Shazam. Verifique a ligação.
recognize-no-answer = O Shazam não respondeu. Tente de novo daqui a pouco.
recognize-rate-limited = { $secs ->
    [one] O Shazam precisa de uma pausa. Tente de novo em { $secs } segundo.
   *[other] O Shazam precisa de uma pausa. Tente de novo em { $secs } segundos.
}
# Buttons on the card.
recognize-playing = A reproduzir
recognize-play = Tocar
recognize-playlist = Adicionar à lista de reprodução
recognize-cancel = Cancelar
recognize-dismiss = Fechar
