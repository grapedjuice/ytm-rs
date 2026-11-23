### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = 已取消 { $count } 个赞

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = 无法在 { $name } 上播放：{ $reason }
airplay-unreachable = 无法连接
airplay-busy = 正忙着播放另一台设备的内容
airplay-needs-pairing = 它要求密码或配对，目前还不支持
airplay-pairing-refused = 它拒绝配对（Apple TV 上需将 设置 › AirPlay 与 HomeKit › 允许访问 设为同一网络中的任何人）
airplay-no-clock = 它没有发送时钟（防火墙可能拦住了 UDP 端口 319 和 320）
airplay-clock-ports = 它的时钟走 UDP 端口 319 和 320，而另一个应用正在占用（另一个 AirPlay 应用，或 PTP 服务）
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = 它的时钟走 UDP 端口 319 和 320，而 Linux 把它们留给 root（用 "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" 放开，或给应用 CAP_NET_BIND_SERVICE），也可能是另一个应用在占用
airplay-failed = 出了点问题
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay 音箱断开了

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = 识别歌曲 · 实验性
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = 识别歌曲
recognize-experimental = 实验性
recognize-waiting = 正在等待声音…
recognize-waiting-line = 在另一个应用里播放这首歌
recognize-listening = 正在听…
recognize-listening-line = 听到了，让它继续放
recognize-identifying = 正在识别…
recognize-identifying-line = 正在询问 Shazam
recognize-no-match = 没有匹配
recognize-no-match-line = Shazam 不认识这首
recognize-failed = 无法识别
recognize-unsupported = 这台电脑录不了自己正在播放的声音
recognize-headset-output = 听不了耳机。把声音输出改到扬声器后再试一次。
recognize-no-device = 没有可听的声音输出
recognize-nothing-playing = 没有在播放
recognize-device-changed = 声音输出变了。再试一次。
recognize-offline = 连不上 Shazam。请检查网络。
recognize-no-answer = Shazam 没有回应。过一会儿再试。
recognize-rate-limited = Shazam 需要歇一会儿。请在 { $secs } 秒后再试。
# Buttons on the card.
recognize-playing = 正在播放
recognize-play = 播放
recognize-playlist = 添加到播放列表
recognize-cancel = 取消
recognize-dismiss = 关闭
