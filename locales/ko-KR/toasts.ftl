### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = 좋아요 { $count }개를 취소했습니다

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = { $name }에서 재생하지 못했습니다: { $reason }
airplay-unreachable = 연결할 수 없었습니다
airplay-busy = 다른 기기에서 재생 중이라 바쁩니다
airplay-needs-pairing = 암호나 페어링을 요구하는데, 아직 지원하지 않습니다
airplay-pairing-refused = 페어링을 거부했습니다(Apple TV에서는 설정 › AirPlay 및 HomeKit › 접근 허용을 같은 네트워크의 모든 사람으로 두세요)
airplay-no-clock = 시계를 보내지 않았습니다(방화벽이 UDP 포트 319와 320을 막고 있을 수 있습니다)
airplay-clock-ports = 시계가 UDP 포트 319와 320으로 오는데, 다른 앱이 쓰고 있습니다(다른 AirPlay 앱이거나 PTP 서비스)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = 시계가 UDP 포트 319와 320으로 오는데, Linux가 root용으로 남겨 둡니다("sudo sysctl net.ipv4.ip_unprivileged_port_start=319"로 허용하거나 앱에 CAP_NET_BIND_SERVICE를 주세요). 아니면 다른 앱이 쓰고 있습니다
airplay-failed = 문제가 생겼습니다
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay 스피커와 연결이 끊겼습니다

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = 곡 인식 · 실험 기능
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = 곡 인식
recognize-experimental = 실험 기능
recognize-waiting = 소리를 기다리는 중…
recognize-waiting-line = 다른 앱에서 곡을 재생하세요
recognize-listening = 듣는 중…
recognize-listening-line = 들립니다. 계속 재생해 두세요
recognize-identifying = 확인하는 중…
recognize-identifying-line = Shazam에 묻는 중
recognize-no-match = 일치 없음
recognize-no-match-line = Shazam이 이 곡을 모릅니다
recognize-failed = 인식하지 못했습니다
recognize-unsupported = 이 컴퓨터는 재생 중인 소리를 녹음할 수 없습니다
recognize-headset-output = 헤드셋은 들을 수 없습니다. 소리 출력을 스피커로 바꾼 뒤 다시 시도하세요.
recognize-no-device = 들을 소리 출력이 없습니다
recognize-nothing-playing = 재생 중인 것이 없습니다
recognize-device-changed = 소리 출력이 바뀌었습니다. 다시 시도하세요.
recognize-offline = Shazam에 연결하지 못했습니다. 연결을 확인하세요.
recognize-no-answer = Shazam이 응답하지 않았습니다. 잠시 후 다시 시도하세요.
recognize-rate-limited = Shazam이 쉬어야 합니다. { $secs }초 후에 다시 시도하세요.
# Buttons on the card.
recognize-playing = 지금 재생 중
recognize-play = 재생
recognize-playlist = 재생목록에 추가
recognize-cancel = 취소
recognize-dismiss = 닫기
