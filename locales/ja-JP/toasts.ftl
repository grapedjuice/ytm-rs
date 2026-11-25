### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = 高評価を { $count } 件取り消しました

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = { $name } では再生できませんでした: { $reason }
airplay-unreachable = 届きませんでした
airplay-busy = 別の機器からの再生で使用中です
airplay-needs-pairing = パスワードかペアリングを求めていますが、まだ対応していません
airplay-pairing-refused = ペアリングを拒否しました（Apple TV では 設定 › AirPlayとHomeKit › アクセスを許可 を同じネットワーク上のすべての人にしてください）
airplay-no-clock = クロックを送りませんでした（ファイアウォールが UDP ポート 319 と 320 を塞いでいることがあります）
airplay-clock-ports = クロックは UDP ポート 319 と 320 に来ますが、別のアプリが使っています（別の AirPlay アプリ、または PTP サービス）
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = クロックは UDP ポート 319 と 320 に来ますが、Linux が root 用に取っています（"sudo sysctl net.ipv4.ip_unprivileged_port_start=319" で許可するか、アプリに CAP_NET_BIND_SERVICE を与えてください）。または別のアプリが使っています
airplay-failed = 何かがうまくいきませんでした
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay スピーカーとの接続が切れました

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = 曲を認識 · 試験機能
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = 曲を認識
recognize-experimental = 試験機能
recognize-waiting = 音を待っています…
recognize-waiting-line = 別のアプリで曲を再生してください
recognize-listening = 聞いています…
recognize-listening-line = 聞こえています。再生したままにしてください
recognize-identifying = 特定しています…
recognize-identifying-line = Shazam に問い合わせています
recognize-no-match = 該当なし
recognize-no-match-line = Shazam はこの曲を知りません
recognize-failed = 認識できませんでした
recognize-unsupported = このコンピューターは再生中の音を録音できません
recognize-headset-output = ヘッドセットは聞けません。音声出力をスピーカーに切り替えて、もう一度試してください。
recognize-no-device = 聞く音声出力がありません
recognize-nothing-playing = 何も再生していません
recognize-device-changed = 音声出力が変わりました。もう一度試してください。
recognize-offline = Shazam に届きませんでした。接続を確認してください。
recognize-no-answer = Shazam が応答しませんでした。しばらくしてからもう一度試してください。
recognize-rate-limited = Shazam は休憩が必要です。{ $secs } 秒後にもう一度試してください。
# Buttons on the card.
recognize-playing = 再生中
recognize-play = 再生
recognize-playlist = プレイリストに追加
recognize-cancel = キャンセル
recognize-dismiss = 閉じる
