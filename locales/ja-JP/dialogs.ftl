### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = キャッシュを削除しますか？
dialog-cache-freed-size = { $size } を解放します。
dialog-cache-frees-space = 空き容量が増えます。
dialog-delete-playlist-title = プレイリストを削除しますか？
dialog-delete-playlist = プレイリストを削除
dialog-delete-upload-title = アップロードを削除しますか？
dialog-delete-upload = アップロードを削除
dialog-name = 名前
dialog-my-playlist = マイ プレイリスト
dialog-private-desc = 非公開 · 自分だけが表示できます
dialog-playlist-name = プレイリスト名
dialog-description = 説明
dialog-optional-description = 説明を追加（任意）
dialog-visibility = 公開設定
dialog-private = 非公開
dialog-unlisted = 限定公開
dialog-public = 公開
dialog-visibility-prompt = 変更なし。公開範囲を変えるには選択してください。
dialog-turn-off = オフにする
dialog-stop-collaboration = 共同編集をオフにしますか？
dialog-stop-collaboration-desc = 共同編集者はプレイリストから削除されます。追加した曲は残ります。
dialog-collaboration-allow = 共同編集者が曲を追加できます。
dialog-collaboration-allow-private = 共同編集者が曲を追加できます。非公開プレイリストは限定公開になります。
dialog-new-collaborators = 新しい共同編集者を許可
dialog-new-collaborators-desc = オフにするとリンクから新しく参加できなくなります。
dialog-copy-invite = 招待リンクをコピー
dialog-invite-copied = 招待リンクをコピーしました。リンクを開いた人は曲を追加できます。
dialog-invite-instructions = 招待したい人にリンクを送ってください。YouTube Music から参加できます。
dialog-invites-off = 新しい共同編集者は参加できません。招待リンクを使うには「新しい共同編集者を許可」をオンにしてください。
dialog-collaborators = 共同編集者
dialog-no-collaborators = まだ誰も参加していません。
dialog-delete-playlist-body = 「{ $title }」を YouTube Music のライブラリから削除します。この操作は元に戻せません。
dialog-delete-upload-body = 「{ $title }」を YouTube Music のアップロードから削除します。この操作は元に戻せません。
dialog-download-count = ダウンロード（{ $count } 件）
dialog-edit-playlist = プレイリストを編集
dialog-download-body = 残り { $count } 曲です。再生中の曲からバックグラウンドでダウンロードします。
dialog-download-collection-body = 「{ $title }」の残りは { $count } 曲です。再生中の曲からバックグラウンドでダウンロードします。
dialog-add-duplicates-title = すでにプレイリストに入っています
dialog-add-duplicates-some = これらの曲の一部はすでに 「{ $title }」 に入っています。それでも追加しますか？
dialog-add-duplicates-body =
    { $count ->
       *[other] { $more ->
            [0] この { $count } 曲はすでに 「{ $title }」 に入っています: { $songs }。それでも追加しますか？
           *[other] この { $count } 曲はすでに 「{ $title }」 に入っています: { $songs }、ほか { $more } 曲。それでも追加しますか？
        }
    }
dialog-add-anyway = それでも追加
dialog-skip-duplicates = 重複をスキップ
