# Reaching, waking and updating your devices

A person's devices are certified peers of one identity (`docs/device-mesh.md`).
An admin, whether the person on another of their devices or a developer
driving a board or laptop, needs three things from such a device: reach it by
name wherever it is, wake it when it sleeps, and update what it runs without
losing the session. Each one builds on a concept Tilo already has. There is no
second channel beside them.

## Staying awake while someone works on it

A machine that suspends in the middle of a deploy looks like a network fault.
The idle-suspend timer counts only local input, so an admin working remotely
must hold the machine awake explicitly, and that hold must go away by itself.

- **Inhibitors are the only switch.** The compositor's idle-suspend waits for
  every hold in the session's `InhibitRegistry`. The power service also mirrors
  other programs' logind `block` locks into it: `sleep` holds off suspend only,
  so the screen still blanks and locks, while `idle` keeps the session awake.
  Closing the lid and critical battery still suspend.
- **A remote admin holds a lease, not a connection.** A connection lost to a
  Wi-Fi roam must not end the hold, and a crashed client must not keep a
  battery machine awake forever. The admin path takes a logind `sleep` lock
  that expires at a deadline and renews it on every request. tilo-vm's daemon
  holds `tilo-vm-awake` for 30 minutes after the last request, and
  `tilo-vm vm awake NAME --for 2h` holds it longer.
- **Remote actions count as use.** An action that arrives from one of the
  person's own devices over the peer network (`actions invoke --device`)
  renews the same kind of suspend-only lease in the target session.

## Waking it

What can wake a device depends on its hardware, so the device advertises this
itself instead of the admin guessing.

- The OS image enables magic-packet wake on every interface that supports it:
  Ethernet Wake-on-LAN, and WoWLAN on Wi-Fi chips that keep their association
  in suspend. The wake address is the interface's MAC as the network sees it,
  which is the per-network stable MAC for Wi-Fi.
- A device publishes its wake routes in its certified presence: MAC, interface
  kind and the subnet it was last seen on. Only same-identity devices receive
  them.
- **Any awake device on the same LAN can wake a sibling.** The typed
  `device.wake` action goes to a device that is awake on the target's last
  subnet. That device sends the magic packet and waits for the target to
  rejoin the mesh. An always-on home peer is just such a sibling. Without one,
  the request fails and names the reason (`no awake device on 10.0.0.0/24`)
  instead of hanging.
- tilo-vm on the dev machine is one more waker: `tilo-vm vm wake NAME` sends
  the magic packet from the LAN, and a libvirt VM is woken through its
  hypervisor (`dompmwakeup`).

## Reaching it by name

An IP address is only a hint.

- **On the LAN** every Tilo image announces `<hostname>.local` over mDNS.
  tilo-vm records a device's hostname and MAC when it is added. When the
  recorded address stops answering, it re-resolves the name and keeps the
  new address. If the name does not resolve either, it reports
  that the device is asleep or gone and offers `vm wake`. Each request
  retries over a short loss of connectivity, a Wi-Fi roam say, instead of
  failing on the first `No route to host`.
- **Beyond the LAN** the peer network is the route: hole-punched QUIC with
  relay fallback and automatic reconnection, between devices that prove the
  same identity. Typed actions already travel over it to an enrolled device
  (`PeerActionHost`). Admin traffic uses that same route as its own versioned
  ALPN (`tilo-device-admin/1`), which carries the admin daemon's existing
  request protocol, never a raw shell. It is accepted only from a
  same-identity device whose certificate grants administration. A developer's
  machine reaches devices this way by enrolling as one of the person's
  devices, like any other.

## Updating what it runs

An update is a set of layers, never edits to `/usr`. Every change can be
rolled back, and the next boot either keeps or drops it on purpose.

- **Apps** update as signed tapps through the app registry, system apps
  included. The OS image is never switched just to update an app.
- **OS** updates are bootc image switches with rollback.
- **Session binaries on a dev device** are tilo-vm layers. Each one is a
  system-extension directory with recorded provenance (CI digest, or the
  commit built locally), applied to that device's session:
  - On a board, the `device` slot's session units take the layers as
    `ExtensionDirectories=`.
  - On a laptop whose owner logs in through greetd, the `device` slot is the
    owner's session on seat0. Its layers merge system-wide with
    `systemd-sysext`, because greetd starts that session in the host's mount
    namespace.
- **Restarting the desktop does not log the person out.** `tilo-session` keeps
  the PAM session, the user manager and `tilo-graphical-session.target`, and
  starts the compositor again when asked to restart. Apps reopen through
  continuity, as after any compositor restart. A swap and its rollback both
  use this. Only a change to the greeter itself restarts greetd.
- **Builds run on the builder of the device's architecture**, never on the dev
  machine, and a laptop-sized device does not build either. CI layers come
  from the registry by digest. A local build streams its layer from the
  builder to the device, so the dev machine's disk never holds the binaries.

## Verification

- Unit tests cover how logind locks map onto the registry, lease expiry and
  renewal, magic-packet encoding, and wake-route selection.
- A two-VM end-to-end test on one Ethernet segment uses the paired devices of
  the nearby-pairing test:
  1. A remote admin's lease keeps device B awake past its idle timeout.
  2. Once the lease expires, B suspends.
  3. Device A invokes `device.wake` by B's name. The harness checks that the
     magic packet for B's MAC was sent and resumes B, standing in for the NIC.
  4. A's action reaches B again without being reconfigured.
  5. B's link is cut and restored, and the action route reconnects.
- On hardware, a board on Ethernet and a laptop on Wi-Fi each run a swap and a
  rollback through the `device` slot, then sleep and are woken by name.
