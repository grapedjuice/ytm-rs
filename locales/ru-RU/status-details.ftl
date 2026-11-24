### App interface.

status-upload-complete = Загружено: { $count }. Обработка в YouTube Music может занять минуту.
status-upload-failed = Не удалось загрузить: { $details }
status-upload-partial = Загружено: { $count }. Ошибки: { $details }
status-waiting-transfer = Ожидание, пока перенос закончит добавление понравившихся…
status-liking-progress = Добавление лайков: { $done } из { $total }…
status-unliking-progress = Снятие лайков: { $done } из { $total }…
status-playlist-created = Плейлист создан
status-new-playlist-tracks = Добавлено в новый плейлист: { $count }
status-playlist-added = Добавлено в плейлист
status-playlist-updated = Плейлист обновлён
status-playlist-sorted = Плейлист отсортирован
status-playlist-sort-failed = Не удалось отсортировать плейлист
status-playlist-move-failed = YouTube Music не принял перемещение
status-library-saved = Сохранено в медиатеке
status-library-removed = Удалено из медиатеки
status-unsubscribed = Подписка отменена
status-subscribe-failed = Не удалось подписаться
status-unsubscribe-failed = Не удалось отписаться
status-like-success = Поставлено лайков: { $count }
status-like-refused = YouTube Music не принял изменение. Повторите попытку позже.
status-refused-count = Отклонено YouTube Music: { $count }
status-removed-liked = Убрано из понравившихся
status-download-batch = Скачивание треков: { $count }
status-download-collection = Скачивание из «{ $title }»: { $count }
status-download-stopped = Скачивание{ $name } остановлено. Сохранено: { $songs }
status-download-already = Уже скачано
status-download-name-already = «{ $title }» уже скачано
status-download-done = Скачано{ $name }: { $songs }
status-download-already-count = Уже было: { $count }
status-download-fail-count = Не удалось скачать: { $count } ({ $details })
status-play-premium = Для «{ $title }» нужен YouTube Music Premium
status-play-sign-in = Войдите, чтобы воспроизвести «{ $title }»
status-age-restricted = действует ограничение по возрасту
status-songs = { $count ->
    [one] { $count } трек
    [few] { $count } трека
    [many] { $count } треков
   *[other] { $count } трека
}
status-episodes = { $count ->
    [one] { $count } выпуск
    [few] { $count } выпуска
    [many] { $count } выпусков
   *[other] { $count } выпуска
}
status-download-quoted-title = «{ $title }»
discord-view-artist = К исполнителю
discord-view-album = К альбому
