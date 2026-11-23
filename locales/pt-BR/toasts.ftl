### The short notices at the bottom of the window.

likes-removed = { $count ->
    [one] Removida { $count } curtida
   *[other] Removidas { $count } curtidas
}

## Recognize song

recognize-overline = Reconhecer música · Experimental
recognize-song = Reconhecer música
recognize-experimental = Experimental
recognize-waiting = Esperando som…
recognize-waiting-line = Toque a música em outro app
recognize-listening = Ouvindo…
recognize-listening-line = Dá para ouvir, deixe tocando
recognize-identifying = Identificando…
recognize-identifying-line = Perguntando ao Shazam
recognize-no-match = Nenhum resultado
recognize-no-match-line = O Shazam não conhece esta música
recognize-failed = Não foi possível reconhecer
recognize-unsupported = Este computador não consegue gravar o que toca
recognize-no-device = Não há saída de som para ouvir
recognize-nothing-playing = Nada está tocando
recognize-device-changed = A saída de som mudou. Tente de novo.
recognize-offline = Não foi possível acessar o Shazam. Verifique sua conexão.
recognize-no-answer = O Shazam não respondeu. Tente de novo daqui a pouco.
recognize-rate-limited = { $secs ->
    [one] O Shazam precisa de uma pausa. Tente de novo em { $secs } segundo.
   *[other] O Shazam precisa de uma pausa. Tente de novo em { $secs } segundos.
}
recognize-playing = Tocando agora
recognize-play = Tocar
recognize-playlist = Adicionar à playlist
recognize-cancel = Cancelar
recognize-dismiss = Fechar
