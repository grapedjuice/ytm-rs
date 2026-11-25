### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = Limpar cache?
dialog-cache-freed-size = Isso libera { $size }.
dialog-cache-frees-space = Isso libera espaço.
dialog-delete-playlist-title = Excluir playlist?
dialog-delete-playlist = Excluir playlist
dialog-delete-upload-title = Excluir upload?
dialog-delete-upload = Excluir upload
dialog-name = Nome
dialog-my-playlist = Minha playlist
dialog-private-desc = Privada · só você pode ver
dialog-playlist-name = Nome da playlist
dialog-description = Descrição
dialog-optional-description = Adicione uma descrição opcional
dialog-visibility = Visibilidade
dialog-private = Privada
dialog-unlisted = Não listada
dialog-public = Pública
dialog-visibility-prompt = Sem alterações. Escolha quem pode ver a playlist.
dialog-turn-off = Desativar
dialog-stop-collaboration = Desativar colaboração?
dialog-stop-collaboration-desc = Os colaboradores serão removidos da playlist. As faixas que adicionaram ficam.
dialog-collaboration-allow = Colaboradores podem adicionar faixas.
dialog-collaboration-allow-private = Colaboradores podem adicionar faixas. A playlist privada se torna não listada.
dialog-new-collaborators = Permitir novos colaboradores
dialog-new-collaborators-desc = Desative para impedir novas entradas pelo link.
dialog-copy-invite = Copiar link de convite
dialog-invite-copied = Link copiado. Quem abrir pode adicionar faixas.
dialog-invite-instructions = Envie o link a quem quer convidar. A pessoa entra pelo YouTube Music.
dialog-invites-off = Novos colaboradores não podem entrar. Permita novos colaboradores para obter um link.
dialog-collaborators = Colaboradores
dialog-no-collaborators = Ninguém entrou ainda.
dialog-delete-playlist-body = “{ $title }” será removida da sua biblioteca do YouTube Music. Não é possível desfazer esta ação.
dialog-delete-upload-body = “{ $title }” será excluída dos seus uploads do YouTube Music. Não é possível desfazer esta ação.
dialog-download-count = Baixar ({ $count })
dialog-edit-playlist = Editar playlist
dialog-download-body = Faixas que faltam baixar: { $count }. O download roda em segundo plano e começa pela faixa atual.
dialog-download-collection-body = Faixas de “{ $title }” que faltam baixar: { $count }. O download roda em segundo plano e começa pela faixa atual.
dialog-add-duplicates-title = Já está na playlist
dialog-add-duplicates-some = Algumas destas músicas já estão em “{ $title }”. Adicionar mesmo assim?
dialog-add-duplicates-body =
    { $count ->
        [one] “{ $songs }” já está em “{ $title }”. Adicionar mesmo assim?
       *[other] { $more ->
            [0] Estas { $count } músicas já estão em “{ $title }”: { $songs }. Adicionar mesmo assim?
           *[other] Estas { $count } músicas já estão em “{ $title }”: { $songs } e mais { $more }. Adicionar mesmo assim?
        }
    }
dialog-add-anyway = Adicionar mesmo assim
dialog-skip-duplicates = Pular repetidas
