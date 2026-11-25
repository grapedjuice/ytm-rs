### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] { $count } mention « J’aime » retirée
   *[other] { $count } mentions « J’aime » retirées
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Lecture impossible sur { $name } : { $reason }
airplay-unreachable = il était injoignable
airplay-busy = il est occupé à lire depuis un autre appareil
airplay-needs-pairing = il demande un mot de passe ou un appairage, qui ne sont pas encore pris en charge
airplay-pairing-refused = il a refusé l’appairage (sur une Apple TV, Réglages › AirPlay et HomeKit › Autoriser l’accès doit être sur tout le monde sur le même réseau)
airplay-no-clock = il n’a pas envoyé son horloge (un pare-feu bloque peut-être les ports UDP 319 et 320)
airplay-clock-ports = son horloge passe par les ports UDP 319 et 320, qu’une autre app utilise (une autre app AirPlay, ou un service PTP)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = son horloge passe par les ports UDP 319 et 320, que Linux réserve à root (autorisez-les avec "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", ou donnez à l’app CAP_NET_BIND_SERVICE), ou une autre app les utilise
airplay-failed = quelque chose s’est mal passé
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Enceinte AirPlay perdue

## Recognize song: the card by the player bar while it listens to what the computer
## plays, and with what Shazam heard.

# The card's window title. Experimental: it uses an unofficial Shazam service.
recognize-overline = Reconnaître le titre · Expérimental
# The command: in the tray menu, the shortcuts, the palette, and on the search box.
recognize-song = Reconnaître le titre
recognize-experimental = Expérimental
recognize-waiting = En attente de son…
recognize-waiting-line = Lancez le titre dans une autre app
recognize-listening = Écoute…
recognize-listening-line = On l’entend, laissez-le jouer
recognize-identifying = Identification…
recognize-identifying-line = Demande à Shazam
recognize-no-match = Aucune correspondance
recognize-no-match-line = Shazam ne connaît pas celui-ci
recognize-failed = Reconnaissance impossible
recognize-unsupported = Cet ordinateur ne peut pas enregistrer ce qu’il lit
recognize-headset-output = Impossible d’écouter un casque. Passez la sortie son sur les haut-parleurs et réessayez.
recognize-no-device = Aucune sortie son à écouter
recognize-nothing-playing = Rien ne joue
recognize-device-changed = La sortie son a changé. Réessayez.
recognize-offline = Shazam est injoignable. Vérifiez votre connexion.
recognize-no-answer = Shazam n’a pas répondu. Réessayez dans un instant.
recognize-rate-limited = { $secs ->
    [one] Shazam a besoin d’une pause. Réessayez dans { $secs } seconde.
   *[other] Shazam a besoin d’une pause. Réessayez dans { $secs } secondes.
}
# Buttons on the card.
recognize-playing = Lecture en cours
recognize-play = Lire
recognize-playlist = Ajouter à une playlist
recognize-cancel = Annuler
recognize-dismiss = Fermer
