### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = Wyczyścić pamięć podręczną?
dialog-cache-freed-size = Zwolni to { $size }.
dialog-cache-frees-space = Zwolni to miejsce.
dialog-delete-playlist-title = Usunąć playlistę?
dialog-delete-playlist = Usuń playlistę
dialog-delete-upload-title = Usunąć przesłany utwór?
dialog-delete-upload = Usuń przesłany utwór
dialog-name = Nazwa
dialog-my-playlist = Moja playlista
dialog-private-desc = Prywatna · widoczna tylko dla Ciebie
dialog-playlist-name = Nazwa playlisty
dialog-description = Opis
dialog-optional-description = Dodaj opcjonalny opis
dialog-visibility = Widoczność
dialog-private = Prywatna
dialog-unlisted = Niepubliczna
dialog-public = Publiczna
dialog-visibility-prompt = Bez zmian. Wybierz, kto może ją widzieć.
dialog-turn-off = Wyłącz
dialog-stop-collaboration = Wyłączyć współpracę?
dialog-stop-collaboration-desc = Współpracownicy stracą dostęp do playlisty. Dodane przez nich utwory pozostaną.
dialog-collaboration-allow = Współpracownicy mogą dodawać utwory.
dialog-collaboration-allow-private = Współpracownicy mogą dodawać utwory. Prywatna playlista stanie się niepubliczna.
dialog-new-collaborators = Zezwalaj na nowych współpracowników
dialog-new-collaborators-desc = Wyłącz, aby nikt nowy nie mógł dołączyć przez link.
dialog-copy-invite = Kopiuj link z zaproszeniem
dialog-invite-copied = Skopiowano link. Każdy, kto go otworzy, może dodawać utwory.
dialog-invite-instructions = Wyślij link osobom, które chcesz zaprosić. Dołączą przez YouTube Music.
dialog-invites-off = Nowe osoby nie mogą dołączyć. Zezwól na nowych współpracowników, by uzyskać link.
dialog-collaborators = Współpracownicy
dialog-no-collaborators = Nikt jeszcze nie dołączył.
dialog-delete-playlist-body = Playlista „{ $title }” zostanie usunięta z biblioteki YouTube Music. Tej operacji nie można cofnąć.
dialog-delete-upload-body = Utwór „{ $title }” zostanie usunięty z przesłanych w YouTube Music. Tej operacji nie można cofnąć.
dialog-download-count = Pobierz ({ $count })
dialog-edit-playlist = Edytuj playlistę
dialog-download-body = Utwory do pobrania: { $count }. Pobieranie działa w tle, od bieżącego utworu.
dialog-download-collection-body = Utwory do pobrania z „{ $title }”: { $count }. Pobieranie działa w tle, od bieżącego utworu.
dialog-add-duplicates-title = Już na playliście
dialog-add-duplicates-some = Część tych utworów jest już w „{ $title }”. Dodać mimo to?
dialog-add-duplicates-body =
    { $count ->
        [one] „{ $songs }” jest już w „{ $title }”. Dodać mimo to?
        [few] { $more ->
            [0] Te { $count } utwory są już w „{ $title }”: { $songs }. Dodać mimo to?
           *[other] Te { $count } utwory są już w „{ $title }”: { $songs } i jeszcze { $more }. Dodać mimo to?
        }
        [many] { $more ->
            [0] Tych { $count } utworów jest już w „{ $title }”: { $songs }. Dodać mimo to?
           *[other] Tych { $count } utworów jest już w „{ $title }”: { $songs } i jeszcze { $more }. Dodać mimo to?
        }
       *[other] { $more ->
            [0] Tego { $count } utworu jest już w „{ $title }”: { $songs }. Dodać mimo to?
           *[other] Tego { $count } utworu jest już w „{ $title }”: { $songs } i jeszcze { $more }. Dodać mimo to?
        }
    }
dialog-add-anyway = Dodaj mimo to
dialog-skip-duplicates = Pomiń duplikaty
