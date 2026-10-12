## History of the saved connections

remote-connection-name = Renamed { $item }
remote-connection-host = Pointed { $item } at { $new }
remote-connection-port = Set the port of { $item } to { $new }
remote-connection-user = Set the user of { $item } to { $new }
remote-connection-kind = Set { $item } to connect over { $new ->
    [ssh] SSH
    [vnc] VNC
    [rdp] RDP
   *[tilo] the Tilo device network
}
remote-connection-auth = Set { $item } to sign in { $new ->
    [password] with a password
    [public_key] with a public key
   *[device_identity] with this device's identity
}
remote-connection-group = Filed { $item } under { $new }
remote-connection-favorite = { $new ->
    [true] Pinned { $item }
   *[false] Unpinned { $item }
}
remote-remote-connections = { $op ->
    [insert] Saved a connection
    [remove] Deleted a connection
   *[other] Changed the saved connections
}
remote-add-connection = Saved { $name }
remote-delete-connection = Deleted { $item }
remote-mark-used = Connected to { $item }
