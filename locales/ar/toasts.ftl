### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [zero] لم تتم إزالة أي إعجاب
    [one] تمت إزالة إعجاب واحد
    [two] تمت إزالة إعجابين
    [few] تمت إزالة { $count } إعجابات
    [many] تمت إزالة { $count } إعجابًا
   *[other] تمت إزالة { $count } إعجاب
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = تعذّر التشغيل على { $name }: { $reason }
airplay-unreachable = تعذّر الوصول إليه
airplay-busy = مشغول بالتشغيل من جهاز آخر
airplay-needs-pairing = يريد كلمة مرور أو اقترانًا، وهذا غير مدعوم بعد
airplay-pairing-refused = رفض الاقتران (على Apple TV يجب أن يكون الإعدادات › AirPlay وHomeKit › السماح بالوصول على أي شخص في الشبكة نفسها)
airplay-no-clock = لم يُرسل ساعته (قد يحجب جدار حماية منفذي UDP 319 و320)
airplay-clock-ports = تأتي ساعته عبر منفذي UDP 319 و320، ويستعملهما تطبيق آخر (تطبيق AirPlay آخر، أو خدمة PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = تأتي ساعته عبر منفذي UDP 319 و320، ويحجزهما Linux للجذر (اسمح بهما عبر "sudo sysctl net.ipv4.ip_unprivileged_port_start=319"، أو امنح التطبيق CAP_NET_BIND_SERVICE)، أو يستعملهما تطبيق آخر
airplay-failed = حدث خطأ ما
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = فُقد مكبّر AirPlay

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = التعرّف على الأغنية · تجريبي
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = التعرّف على الأغنية
recognize-experimental = تجريبي
recognize-waiting = بانتظار الصوت…
recognize-waiting-line = شغّل الأغنية في تطبيق آخر
recognize-listening = يستمع…
recognize-listening-line = يُسمع، اتركه يعمل
recognize-identifying = جارٍ التعرّف…
recognize-identifying-line = يسأل Shazam
recognize-no-match = لا يوجد تطابق
recognize-no-match-line = لا يعرف Shazam هذه الأغنية
recognize-failed = تعذّر التعرّف عليها
recognize-unsupported = لا يستطيع هذا الحاسوب تسجيل ما يشغّله
recognize-headset-output = لا يمكن الاستماع إلى سمّاعة رأس. بدّل مخرج الصوت إلى المكبّرات وحاول مرة أخرى.
recognize-no-device = لا يوجد مخرج صوت للاستماع إليه
recognize-nothing-playing = لا شيء قيد التشغيل
recognize-device-changed = تغيّر مخرج الصوت. حاول مرة أخرى.
recognize-offline = تعذّر الوصول إلى Shazam. تحقق من اتصالك.
recognize-no-answer = لم يُجب Shazam. حاول مرة أخرى بعد لحظات.
recognize-rate-limited = { $secs ->
    [zero] يحتاج Shazam إلى استراحة. أعد المحاولة بعد { $secs } ثانية.
    [one] يحتاج Shazam إلى استراحة. أعد المحاولة بعد ثانية واحدة.
    [two] يحتاج Shazam إلى استراحة. أعد المحاولة بعد ثانيتين.
    [few] يحتاج Shazam إلى استراحة. أعد المحاولة بعد { $secs } ثوانٍ.
    [many] يحتاج Shazam إلى استراحة. أعد المحاولة بعد { $secs } ثانية.
   *[other] يحتاج Shazam إلى استراحة. أعد المحاولة بعد { $secs } ثانية.
}
# Buttons on the card.
recognize-playing = قيد التشغيل الآن
recognize-play = تشغيل
recognize-playlist = إضافة إلى قائمة التشغيل
recognize-cancel = إلغاء
recognize-dismiss = تجاهل
