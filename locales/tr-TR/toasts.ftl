### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count } beğeni kaldırıldı

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = { $name } üzerinde çalınamadı: { $reason }
airplay-unreachable = ona ulaşılamadı
airplay-busy = başka bir cihazdan çalmakla meşgul
airplay-needs-pairing = bir parola ya da eşleştirme istiyor; bunlar henüz desteklenmiyor
airplay-pairing-refused = eşleşmek istemedi (bir Apple TV’de Ayarlar › AirPlay ve HomeKit › Erişime İzin Ver, aynı ağdaki herkes olmalıdır)
airplay-no-clock = saatini göndermedi (bir güvenlik duvarı UDP 319 ve 320 bağlantı noktalarını engelliyor olabilir)
airplay-clock-ports = saati UDP 319 ve 320 bağlantı noktalarından gelir; bunları başka bir uygulama kullanıyor (başka bir AirPlay uygulaması veya bir PTP hizmeti)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = saati UDP 319 ve 320 bağlantı noktalarından gelir; Linux bunları root için ayırır ("sudo sysctl net.ipv4.ip_unprivileged_port_start=319" ile açın veya uygulamaya CAP_NET_BIND_SERVICE verin) ya da başka bir uygulama kullanıyordur
airplay-failed = bir şeyler ters gitti
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay hoparlörü kayboldu

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Şarkıyı tanı · Deneysel
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Şarkıyı tanı
recognize-experimental = Deneysel
recognize-waiting = Ses bekleniyor…
recognize-waiting-line = Şarkıyı başka bir uygulamada çal
recognize-listening = Dinleniyor…
recognize-listening-line = Duyuluyor, çalmaya devam etsin
recognize-identifying = Tanınıyor…
recognize-identifying-line = Shazam’a soruluyor
recognize-no-match = Eşleşme yok
recognize-no-match-line = Shazam bunu bilmiyor
recognize-failed = Tanınamadı
recognize-unsupported = Bu bilgisayar çaldığını kaydedemiyor
recognize-headset-output = Bir kulaklık dinlenemez. Ses çıkışını hoparlörlere alıp yeniden deneyin.
recognize-no-device = Dinlenecek bir ses çıkışı yok
recognize-nothing-playing = Hiçbir şey çalmıyor
recognize-device-changed = Ses çıkışı değişti. Yeniden deneyin.
recognize-offline = Shazam’a ulaşılamadı. Bağlantınızı kontrol edin.
recognize-no-answer = Shazam yanıt vermedi. Birazdan yeniden deneyin.
recognize-rate-limited = Shazam’ın molaya ihtiyacı var. { $secs } saniye sonra yeniden deneyin.
# Buttons on the card.
recognize-playing = Şimdi oynatılıyor
recognize-play = Oynat
recognize-playlist = Çalma listesine ekle
recognize-cancel = İptal
recognize-dismiss = Kapat
