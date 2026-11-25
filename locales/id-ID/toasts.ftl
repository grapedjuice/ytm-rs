### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
   *[other] Suka dihapus dari { $count } lagu
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Tidak bisa diputar di { $name }: { $reason }
airplay-unreachable = tidak bisa dihubungi
airplay-busy = sedang sibuk memutar dari perangkat lain
airplay-needs-pairing = minta kata sandi atau pemasangan, dan itu belum didukung
airplay-pairing-refused = tidak mau dipasangkan (di Apple TV, Setelan › AirPlay dan HomeKit › Izinkan Akses harus siapa saja di jaringan yang sama)
airplay-no-clock = tidak mengirim jamnya (firewall mungkin memblokir port UDP 319 dan 320)
airplay-clock-ports = jamnya lewat port UDP 319 dan 320, yang sedang dipakai aplikasi lain (aplikasi AirPlay lain, atau layanan PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = jamnya lewat port UDP 319 dan 320, yang Linux simpan untuk root (izinkan dengan "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", atau beri aplikasi CAP_NET_BIND_SERVICE), atau aplikasi lain sedang memakainya
airplay-failed = ada yang salah
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Speaker AirPlay terputus

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Kenali lagu · Eksperimental
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Kenali lagu
recognize-experimental = Eksperimental
recognize-waiting = Menunggu suara…
recognize-waiting-line = Putar lagunya di aplikasi lain
recognize-listening = Mendengarkan…
recognize-listening-line = Kedengaran, biarkan terus main
recognize-identifying = Mengenali…
recognize-identifying-line = Bertanya ke Shazam
recognize-no-match = Tidak cocok
recognize-no-match-line = Shazam tidak kenal yang ini
recognize-failed = Tidak bisa dikenali
recognize-unsupported = Komputer ini tidak bisa merekam yang diputarnya
recognize-headset-output = Headset tidak bisa didengarkan. Pindahkan keluaran suara ke speaker lalu coba lagi.
recognize-no-device = Tidak ada keluaran suara untuk didengarkan
recognize-nothing-playing = Tidak ada yang diputar
recognize-device-changed = Keluaran suara berubah. Coba lagi.
recognize-offline = Tidak bisa menghubungi Shazam. Cek koneksi kamu.
recognize-no-answer = Shazam tidak menjawab. Coba lagi sebentar lagi.
recognize-rate-limited = Shazam perlu istirahat. Coba lagi dalam { $secs } detik.
# Buttons on the card.
recognize-playing = Sedang diputar
recognize-play = Putar
recognize-playlist = Tambah ke playlist
recognize-cancel = Batal
recognize-dismiss = Tutup
