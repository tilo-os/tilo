## History

vpn-profile-name = Renamed the VPN profile { $item }
vpn-profile-tor = { $new ->
    [true] Routed { $item } over Tor
   *[false] Stopped routing { $item } over Tor
}
vpn-vpn-connections = { $op ->
    [insert] Added a VPN profile
    [remove] Removed the VPN profile { $item }
   *[other] Changed the VPN profiles
}
