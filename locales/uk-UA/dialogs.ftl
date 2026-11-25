### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = Очистити кеш?
dialog-cache-freed-size = Буде звільнено { $size }.
dialog-cache-frees-space = Це звільнить місце.
dialog-delete-playlist-title = Видалити список відтворення?
dialog-delete-playlist = Видалити список відтворення
dialog-delete-upload-title = Видалити завантажений трек?
dialog-delete-upload = Видалити завантажений трек
dialog-name = Назва
dialog-my-playlist = Мій список відтворення
dialog-private-desc = Приватний · видимий лише вам
dialog-playlist-name = Назва списку відтворення
dialog-description = Опис
dialog-optional-description = Додайте опис (необов’язково)
dialog-visibility = Видимість
dialog-private = Приватний
dialog-unlisted = Доступ за посиланням
dialog-public = Публічний
dialog-visibility-prompt = Без змін. Виберіть, хто може бачити список.
dialog-turn-off = Вимкнути
dialog-stop-collaboration = Вимкнути спільний доступ?
dialog-stop-collaboration-desc = Учасники втратять доступ до списку. Додані ними треки збережуться.
dialog-collaboration-allow = Учасники можуть додавати треки.
dialog-collaboration-allow-private = Учасники можуть додавати треки. Приватний список стане доступним за посиланням.
dialog-new-collaborators = Дозволити нових учасників
dialog-new-collaborators-desc = Вимкніть, щоб нові учасники не могли приєднатися за посиланням.
dialog-copy-invite = Скопіювати посилання-запрошення
dialog-invite-copied = Посилання скопійовано. Кожен, хто його відкриє, зможе додавати треки.
dialog-invite-instructions = Надішліть посилання тим, кого хочете запросити. Вони приєднаються через YouTube Music.
dialog-invites-off = Нові учасники не можуть приєднатися. Щоб отримати посилання, дозвольте нових учасників.
dialog-collaborators = Учасники
dialog-no-collaborators = Поки ніхто не приєднався.
dialog-delete-playlist-body = Список «{ $title }» буде видалено з вашої бібліотеки YouTube Music. Цю дію неможливо скасувати.
dialog-delete-upload-body = Трек «{ $title }» буде видалено з ваших завантажень на YouTube Music. Цю дію неможливо скасувати.
dialog-download-count = Завантажити ({ $count })
dialog-edit-playlist = Змінити список відтворення
dialog-download-body = Лишилося завантажити: { $count }. Завантаження відбувається у фоновому режимі, починаючи з поточного треку.
dialog-download-collection-body = Лишилося завантажити з «{ $title }»: { $count }. Завантаження відбувається у фоновому режимі, починаючи з поточного треку.
dialog-add-duplicates-title = Уже в списку відтворення
dialog-add-duplicates-some = Частина цих треків уже є в «{ $title }». Додати все одно?
dialog-add-duplicates-body =
    { $count ->
        [one] «{ $songs }» уже є в «{ $title }». Додати все одно?
        [few] { $more ->
            [0] Ці { $count } треки вже є в «{ $title }»: { $songs }. Додати все одно?
           *[other] Ці { $count } треки вже є в «{ $title }»: { $songs } і ще { $more }. Додати все одно?
        }
        [many] { $more ->
            [0] Цих { $count } треків вже є в «{ $title }»: { $songs }. Додати все одно?
           *[other] Цих { $count } треків вже є в «{ $title }»: { $songs } і ще { $more }. Додати все одно?
        }
       *[other] { $more ->
            [0] Цього { $count } треку вже є в «{ $title }»: { $songs }. Додати все одно?
           *[other] Цього { $count } треку вже є в «{ $title }»: { $songs } і ще { $more }. Додати все одно?
        }
    }
dialog-add-anyway = Додати все одно
dialog-skip-duplicates = Пропустити дублікати
