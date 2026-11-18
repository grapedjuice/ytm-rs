### App interface.

status-upload-complete = Завантажено: { $count }. Обробка в YouTube Music може тривати хвилину.
status-upload-failed = Не вдалося завантажити: { $details }
status-upload-partial = Завантажено: { $count }. Помилки: { $details }
status-waiting-transfer = Очікування завершення перенесення вподобаних треків…
status-liking-progress = Додавання вподобайок: { $done } з { $total }…
status-unliking-progress = Зняття вподобайок: { $done } з { $total }…
status-playlist-created = Список створено
status-new-playlist-tracks = Додано до нового списку: { $count }
status-playlist-added = Додано до списку
status-playlist-updated = Список оновлено
status-playlist-sorted = Список відсортовано
status-playlist-sort-failed = Не вдалося відсортувати список
status-playlist-move-failed = YouTube Music не прийняв переміщення
status-library-saved = Збережено в бібліотеці
status-library-removed = Вилучено з бібліотеки
status-unsubscribed = Підписку скасовано
status-subscribe-failed = Не вдалося підписатися
status-unsubscribe-failed = Не вдалося скасувати підписку
status-like-success = Додано вподобайок: { $count }
status-like-refused = YouTube Music не прийняв зміну. Спробуйте пізніше.
status-refused-count = Відхилено YouTube Music: { $count }
status-removed-liked = Вилучено з уподобаних
status-download-batch = Завантаження треків: { $count }
status-download-collection = Завантаження з «{ $title }»: { $count }
status-download-stopped = Завантаження{ $name } зупинено. Збережено: { $songs }
status-download-already = Уже завантажено
status-download-name-already = «{ $title }» уже завантажено
status-download-done = Завантажено{ $name }: { $songs }
status-download-already-count = Уже було: { $count }
status-download-fail-count = Не вдалося завантажити: { $count } ({ $details })
status-play-premium = Для «{ $title }» потрібен YouTube Music Premium
status-play-sign-in = Увійдіть, щоб відтворити «{ $title }»
status-age-restricted = діє вікове обмеження
status-songs = { $count ->
    [one] { $count } трек
    [few] { $count } треки
    [many] { $count } треків
   *[other] { $count } треку
}
status-episodes = { $count ->
    [one] { $count } випуск
    [few] { $count } випуски
    [many] { $count } випусків
   *[other] { $count } випуску
}
status-download-quoted-title = «{ $title }»
discord-view-artist = До виконавця
discord-view-album = До альбому
