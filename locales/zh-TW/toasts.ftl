### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = 已取消 { $count } 個讚

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = 無法在 { $name } 上播放：{ $reason }
airplay-unreachable = 無法連線
airplay-busy = 正忙著播放另一台裝置的內容
airplay-needs-pairing = 它要求密碼或配對，目前還不支援
airplay-pairing-refused = 它拒絕配對（Apple TV 上需將 設定 › AirPlay 與 HomeKit › 允許取用 設為同一網路上的任何人）
airplay-no-clock = 它沒有送出時鐘（防火牆可能擋住了 UDP 連接埠 319 和 320）
airplay-clock-ports = 它的時鐘走 UDP 連接埠 319 和 320，而另一個應用程式正在占用（另一個 AirPlay 應用程式，或 PTP 服務）
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = 它的時鐘走 UDP 連接埠 319 和 320，而 Linux 把它們留給 root（用 "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" 放開，或給應用程式 CAP_NET_BIND_SERVICE），也可能是另一個應用程式在占用
airplay-failed = 出了點問題
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay 喇叭中斷了

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = 辨識歌曲 · 實驗性
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = 辨識歌曲
recognize-experimental = 實驗性
recognize-waiting = 正在等待聲音…
recognize-waiting-line = 在另一個應用程式裡播放這首歌
recognize-listening = 正在聽…
recognize-listening-line = 聽到了，讓它繼續放
recognize-identifying = 正在辨識…
recognize-identifying-line = 正在詢問 Shazam
recognize-no-match = 沒有相符
recognize-no-match-line = Shazam 不認識這首
recognize-failed = 無法辨識
recognize-unsupported = 這台電腦錄不了自己正在播放的聲音
recognize-headset-output = 聽不了耳機。把聲音輸出改到喇叭後再試一次。
recognize-no-device = 沒有可聽的聲音輸出
recognize-nothing-playing = 沒有在播放
recognize-device-changed = 聲音輸出變了。再試一次。
recognize-offline = 連不上 Shazam。請檢查網路。
recognize-no-answer = Shazam 沒有回應。過一會兒再試。
recognize-rate-limited = Shazam 需要歇一會兒。請在 { $secs } 秒後再試。
# Buttons on the card.
recognize-playing = 正在播放
recognize-play = 播放
recognize-playlist = 新增至播放列表
recognize-cancel = 取消
recognize-dismiss = 關閉
