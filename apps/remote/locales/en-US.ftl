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
remote-add-connection = Added { $name }
remote-delete-connection = Deleted { $item }
remote-mark-used = Connected to { $item }

## What this app does beside editing saved connections, told to whoever invokes it
remote-describe-put-file = Send one local file to this SSH machine: name the local path and where to write it remotely.
remote-describe-fetch-file = Fetch one file from this SSH machine: name the remote path and where to write it here.
remote-describe-forget-host-key = Remove this device's stale SSH host-key pin; reconnect to inspect a replacement key.

## Window and accessibility copy
remote-a11y-window = Remote
