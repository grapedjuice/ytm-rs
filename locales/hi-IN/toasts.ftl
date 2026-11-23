### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count } लाइक हटाए गए

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = { $name } पर नहीं चल सका: { $reason }
airplay-unreachable = उस तक पहुँचा नहीं जा सका
airplay-busy = वह किसी दूसरे डिवाइस से चला रहा है
airplay-needs-pairing = वह पासवर्ड या पेयरिंग माँगता है, जो अभी समर्थित नहीं
airplay-pairing-refused = वह पेयर नहीं होना चाहता (Apple TV पर सेटिंग्स › AirPlay और HomeKit › ऐक्सेस की अनुमति एक ही नेटवर्क पर किसी को भी होनी चाहिए)
airplay-no-clock = उसने अपनी घड़ी नहीं भेजी (फ़ायरवॉल UDP पोर्ट 319 और 320 रोक रहा हो सकता है)
airplay-clock-ports = उसकी घड़ी UDP पोर्ट 319 और 320 पर आती है, जिन्हें कोई और ऐप इस्तेमाल कर रहा है (कोई और AirPlay ऐप, या PTP सेवा)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = उसकी घड़ी UDP पोर्ट 319 और 320 पर आती है, जिन्हें Linux रूट के लिए रखता है ("sudo sysctl net.ipv4.ip_unprivileged_port_start=319" से अनुमति दें, या ऐप को CAP_NET_BIND_SERVICE दें), या कोई और ऐप उन्हें इस्तेमाल कर रहा है
airplay-failed = कुछ गड़बड़ हो गई
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = AirPlay स्पीकर छूट गया

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = गाना पहचानें · प्रयोगात्मक
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = गाना पहचानें
recognize-experimental = प्रयोगात्मक
recognize-waiting = आवाज़ का इंतज़ार…
recognize-waiting-line = गाना किसी और ऐप में चलाएँ
recognize-listening = सुन रहे हैं…
recognize-listening-line = सुनाई दे रहा है, चलने दें
recognize-identifying = पहचान रहे हैं…
recognize-identifying-line = Shazam से पूछ रहे हैं
recognize-no-match = कोई मेल नहीं
recognize-no-match-line = Shazam इसे नहीं जानता
recognize-failed = पहचान नहीं हो सकी
recognize-unsupported = यह कंप्यूटर जो चला रहा है उसे रिकॉर्ड नहीं कर सकता
recognize-headset-output = हेडसेट नहीं सुना जा सकता। ध्वनि आउटपुट स्पीकर पर बदलकर फिर कोशिश करें।
recognize-no-device = सुनने के लिए कोई ध्वनि आउटपुट नहीं है
recognize-nothing-playing = कुछ नहीं चल रहा
recognize-device-changed = ध्वनि आउटपुट बदल गया। फिर कोशिश करें।
recognize-offline = Shazam तक नहीं पहुँच सके। कनेक्शन जाँचें।
recognize-no-answer = Shazam ने जवाब नहीं दिया। थोड़ी देर में फिर कोशिश करें।
recognize-rate-limited = Shazam को विश्राम चाहिए। { $secs } सेकंड बाद फिर कोशिश करें।
# Buttons on the card.
recognize-playing = अभी चल रहा है
recognize-play = चलाएँ
recognize-playlist = प्लेलिस्ट में जोड़ें
recognize-cancel = रद्द करें
recognize-dismiss = ख़ारिज करें
