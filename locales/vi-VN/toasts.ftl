### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = Đã bỏ thích { $count } mục

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Không phát được trên { $name }: { $reason }
airplay-unreachable = không liên lạc được
airplay-busy = đang bận phát từ thiết bị khác
airplay-needs-pairing = nó muốn mật khẩu hoặc ghép nối, chưa được hỗ trợ
airplay-pairing-refused = nó không chịu ghép nối (trên Apple TV, Cài đặt › AirPlay và HomeKit › Cho phép truy cập phải là bất kỳ ai trên cùng mạng)
airplay-no-clock = nó không gửi đồng hồ (tường lửa có thể đang chặn cổng UDP 319 và 320)
airplay-clock-ports = đồng hồ của nó đi qua cổng UDP 319 và 320, đang bị ứng dụng khác dùng (ứng dụng AirPlay khác, hoặc dịch vụ PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = đồng hồ của nó đi qua cổng UDP 319 và 320, Linux dành cho root (cho phép bằng "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", hoặc cấp cho ứng dụng CAP_NET_BIND_SERVICE), hoặc ứng dụng khác đang dùng
airplay-failed = có gì đó không ổn
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Mất loa AirPlay

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Nhận diện bài hát · Thử nghiệm
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Nhận diện bài hát
recognize-experimental = Thử nghiệm
recognize-waiting = Đang chờ tiếng…
recognize-waiting-line = Phát bài trong ứng dụng khác
recognize-listening = Đang nghe…
recognize-listening-line = Nghe được rồi, cứ để phát
recognize-identifying = Đang nhận diện…
recognize-identifying-line = Đang hỏi Shazam
recognize-no-match = Không khớp
recognize-no-match-line = Shazam không biết bài này
recognize-failed = Không nhận diện được
recognize-unsupported = Máy tính này không ghi được những gì nó phát
recognize-headset-output = Không nghe được tai nghe. Chuyển đầu ra âm thanh sang loa rồi thử lại.
recognize-no-device = Không có đầu ra âm thanh để nghe
recognize-nothing-playing = Không có gì đang phát
recognize-device-changed = Đầu ra âm thanh đã đổi. Thử lại.
recognize-offline = Không liên lạc được với Shazam. Kiểm tra kết nối.
recognize-no-answer = Shazam không trả lời. Thử lại sau giây lát.
recognize-rate-limited = Shazam cần nghỉ. Thử lại sau { $secs } giây.
# Buttons on the card.
recognize-playing = Đang phát
recognize-play = Phát
recognize-playlist = Thêm vào danh sách phát
recognize-cancel = Hủy
recognize-dismiss = Đóng
