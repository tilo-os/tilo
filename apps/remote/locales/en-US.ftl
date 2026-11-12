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
remote-waiting-first-frame = Waiting for the first screen update

## Facts, property labels and section titles
remote-action-trust-host-key = Explicitly trust { $algorithm } { $fingerprint } for this device only after out-of-band verification.
remote-action-open-session = Open a live session to this saved connection. { $detail }
remote-nearby-devices = Nearby devices
remote-other-connections = Other connections
remote-pinned = Pinned
remote-a11y-terminal = Remote terminal
remote-a11y-desktop = Remote desktop

## What the window shows
remote-placeholder-name = Home server
remote-placeholder-endpoint = Device endpoint id
remote-placeholder-host = Host name or IP address
remote-placeholder-port = Port
remote-placeholder-optional = Optional
remote-placeholder-user = User name
remote-key-passphrase = Key passphrase
remote-password = Password
remote-delete-connection-button = Delete connection
remote-disconnect-title = Close the session to { $connection }?
remote-disconnect-detail = The live session ends and its scrollback goes with it. The saved connection stays.
remote-disconnect-confirm = Close session
remote-delete-title = Delete { $connection }?
remote-delete-detail = The saved connection is removed for everyone you share it with. The machine itself is not changed.
remote-delete-confirm = Delete connection
remote-connect-title = Connect to { $connection }?
remote-connect-detail = This opens a live session. Remote checks the saved address and access again before anything is sent.
remote-connect-confirm = Connect
remote-trust-key-title = Trust the SSH key of { $connection }?
remote-trust-key-detail = Check this key with the machine's owner before you trust it. Only this device remembers it.
remote-trust-key-confirm = Trust key
remote-forget-key-title = Forget the SSH key of { $connection }?
remote-forget-key-detail = Only this device's saved key is removed. Reconnect to check and trust the new key.
remote-forget-key-confirm = Forget key

## Why a connection cannot open
remote-state-ready = Remote can connect to this machine.
remote-state-no-ssh = Remote can't open SSH sessions on this device.
remote-state-no-vnc = Remote can't open VNC sessions on this device.
remote-state-no-session = Remote can't open this kind of session on this device.
remote-state-no-rdp = Remote opens SSH, VNC and Tilo devices, but not RDP.
remote-state-invalid-profile = The saved address or sign-in method doesn't fit this protocol.
remote-connection-deleted-message = Someone you share your connections with removed it.
remote-empty-pinned-hint = Pin the machines you use most to keep them here.
remote-empty-saved-hint = Save a machine to reach it over SSH, VNC or as a Tilo device.
remote-rdp-unsupported-message = Choose SSH or VNC to keep using this machine.

## The window bar's slots
remote-pinned-connections = Pinned connections
remote-connections = Connections
remote-new-connection = New connection
remote-edit-connection = Edit connection
remote-save = Save
remote-actual-size = Actual size
remote-fit-to-window = Fit to window
remote-full-screen = Full screen
remote-disconnect = Disconnect

## Actions

remote-effect-show-connections = Show connections
remote-effect-edit-connection = Connection settings
remote-effect-send-text = Send text
remote-effect-put-file = Send file
remote-effect-fetch-file = Fetch file
remote-effect-trust-host-key = Trust host key
remote-effect-forget-host-key = Forget host key
remote-connection-list = Remote connections
remote-connection-list-add-connection = Add connection
remote-connection-list-mark-used = Record use

## What a connection is called
remote-kind-ssh = SSH
remote-kind-vnc = VNC
remote-kind-rdp = RDP
remote-kind-tilo = Tilo device
remote-auth-password = Password
remote-auth-public-key = Public key
remote-auth-device-identity = Device identity
remote-readiness-ready = Ready
remote-readiness-elsewhere = Not available here
remote-readiness-no-rdp = RDP isn't supported
remote-readiness-editing = Needs editing
remote-issue-ssh-auth = SSH profiles support password or public-key authentication.
remote-issue-vnc-auth = VNC profiles support password authentication only.
remote-issue-rdp-auth = RDP profiles support password authentication only.
remote-issue-tilo-auth = Tilo device profiles must use Tilo device identity.
remote-issue-tilo-endpoint = Tilo device node must be the device's exact endpoint id.
remote-rdp-unsupported-title = Remote can't open RDP connections
