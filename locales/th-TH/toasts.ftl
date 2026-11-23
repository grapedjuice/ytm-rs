### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = นำการกดถูกใจออก { $count } รายการ

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = เล่นบน { $name } ไม่ได้: { $reason }
airplay-unreachable = ติดต่อไม่ได้
airplay-busy = กำลังเล่นจากอุปกรณ์อื่นอยู่
airplay-needs-pairing = ต้องการรหัสผ่านหรือการจับคู่ ซึ่งยังไม่รองรับ
airplay-pairing-refused = ไม่ยอมจับคู่ (บน Apple TV ต้องตั้ง การตั้งค่า › AirPlay และ HomeKit › อนุญาตการเข้าถึง เป็นทุกคนในเครือข่ายเดียวกัน)
airplay-no-clock = ไม่ได้ส่งนาฬิกา (ไฟร์วอลล์อาจบล็อกพอร์ต UDP 319 และ 320)
airplay-clock-ports = นาฬิกามาทางพอร์ต UDP 319 และ 320 ซึ่งแอปอื่นใช้อยู่ (แอป AirPlay อื่น หรือบริการ PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = นาฬิกามาทางพอร์ต UDP 319 และ 320 ซึ่ง Linux สงวนไว้ให้ root (อนุญาตด้วย "sudo sysctl net.ipv4.ip_unprivileged_port_start=319" หรือให้แอปมี CAP_NET_BIND_SERVICE) หรือมีแอปอื่นใช้อยู่
airplay-failed = มีบางอย่างผิดพลาด
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = ลำโพง AirPlay หลุด

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = จำเพลง · ทดลอง
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = จำเพลง
recognize-experimental = ทดลอง
recognize-waiting = กำลังรอเสียง…
recognize-waiting-line = เล่นเพลงในแอปอื่น
recognize-listening = กำลังฟัง…
recognize-listening-line = ได้ยินแล้ว ปล่อยให้เล่นต่อ
recognize-identifying = กำลังระบุ…
recognize-identifying-line = ถาม Shazam
recognize-no-match = ไม่พบเพลง
recognize-no-match-line = Shazam ไม่รู้จักเพลงนี้
recognize-failed = จำไม่ได้
recognize-unsupported = คอมพิวเตอร์เครื่องนี้บันทึกสิ่งที่เล่นไม่ได้
recognize-headset-output = ฟังจากหูฟังไม่ได้ เปลี่ยนเอาต์พุตเสียงไปที่ลำโพงแล้วลองอีกครั้ง
recognize-no-device = ไม่มีเอาต์พุตเสียงให้ฟัง
recognize-nothing-playing = ไม่มีอะไรเล่นอยู่
recognize-device-changed = เอาต์พุตเสียงเปลี่ยน ลองอีกครั้ง
recognize-offline = ติดต่อ Shazam ไม่ได้ ตรวจการเชื่อมต่อ
recognize-no-answer = Shazam ไม่ตอบ ลองอีกครั้งอีกสักครู่
recognize-rate-limited = Shazam ต้องพัก ลองอีกครั้งใน { $secs } วินาที
# Buttons on the card.
recognize-playing = กำลังเล่นอยู่
recognize-play = เล่น
recognize-playlist = เพิ่มไปยังเพลย์ลิสต์
recognize-cancel = ยกเลิก
recognize-dismiss = ปิด
