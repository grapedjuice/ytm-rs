### The short notices at the bottom of the window.

# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [one] Removed { $count } like
   *[other] Removed { $count } likes
}

# An AirPlay speaker picked as the output device wouldn't play; the music stays on this
# computer. { $reason } is one of the airplay- lines below, starting in lower case.
airplay-couldnt-play = Couldn't play on { $name }: { $reason }
airplay-unreachable = it couldn't be reached
airplay-busy = it's busy playing from another device
airplay-needs-pairing = it wants a password or a pairing, which aren't supported yet
airplay-pairing-refused = it wouldn't pair (an Apple TV needs Settings › AirPlay and HomeKit › Allow Access set to anyone on the same network)
airplay-no-clock = it didn't send its clock (a firewall may be blocking UDP ports 319 and 320)
airplay-clock-ports = its clock comes on UDP ports 319 and 320, which another app is using (another AirPlay app, or a PTP service)
# "sysctl …" and "CAP_NET_BIND_SERVICE" are a command and a Linux name: keep them as they are.
airplay-clock-ports-linux = its clock comes on UDP ports 319 and 320, which Linux keeps for root (allow them with "sudo sysctl net.ipv4.ip_unprivileged_port_start=319", or give the app CAP_NET_BIND_SERVICE), or another app is using them
airplay-failed = something went wrong
# The AirPlay speaker playing hung up (turned off, or another device took it over).
airplay-lost = Lost the AirPlay speaker
