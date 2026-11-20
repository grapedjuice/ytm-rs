### App interface.

status-upload-complete = Envio concluído ({ $count }). O YouTube Music pode levar um minuto para processar as faixas.
status-upload-failed = Falha no envio: { $details }
status-upload-partial = Enviadas: { $count }. Falhas: { $details }
status-waiting-transfer = Aguardando a transferência concluir as curtidas…
status-liking-progress = Curtindo: { $done } de { $total }…
status-unliking-progress = Removendo curtidas: { $done } de { $total }…
status-playlist-created = Playlist criada
status-new-playlist-tracks = Faixas na nova playlist: { $count }
status-playlist-added = Adicionado à playlist
status-playlist-updated = Playlist atualizada
status-playlist-sorted = Playlist ordenada
status-playlist-sort-failed = Não foi possível ordenar a playlist
status-playlist-move-failed = O YouTube Music não salvou a mudança de ordem
status-library-saved = Salvo na biblioteca
status-library-removed = Removido da biblioteca
status-unsubscribed = Inscrição cancelada
status-subscribe-failed = Não foi possível se inscrever
status-unsubscribe-failed = Não foi possível cancelar a inscrição
status-like-success = Curtidas adicionadas: { $count }
status-like-refused = O YouTube Music não aceitou a alteração. Tente novamente mais tarde.
status-refused-count = Recusadas pelo YouTube Music: { $count }
status-removed-liked = Removido das faixas curtidas
status-download-batch = Baixando faixas: { $count }
status-download-collection = Baixando de “{ $title }”: { $count }
status-download-stopped = Download interrompido{ $name }. Salvas: { $songs }
status-download-already = Já foi baixado
status-download-name-already = “{ $title }” já foi baixado
status-download-done = Baixado{ $name }: { $songs }
status-download-already-count = { $count } já estavam lá
status-download-fail-count = Não foi possível baixar { $count } ({ $details })
status-play-premium = “{ $title }” requer YouTube Music Premium
status-play-sign-in = Entre para tocar “{ $title }”
status-age-restricted = tem restrição de idade
status-songs = { $count ->
    [one] { $count } faixa
   *[other] { $count } faixas
}
status-episodes = { $count ->
    [one] { $count } episódio
   *[other] { $count } episódios
}
status-download-quoted-title = “{ $title }”
discord-view-artist = Ir para o artista
discord-view-album = Ir para o álbum
