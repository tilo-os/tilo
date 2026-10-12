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

- The OS image enables magic-packet Wake-on-LAN on Ethernet. Lenovo 83ED
  hardware images default the WCN7850 Wi-Fi driver to `wake-on-wlan=0`:
  observed deep-sleep WoW resume failed in MHI and left the interface unusable
  until reboot. This is a targeted NetworkManager default, not a kernel fix or
  proof that non-WoW deep resume succeeds. Explicit saved profile wake flags
  override it; see `os/hardware/README.md` for diagnosis. Other hardware's
  Wi-Fi wake policy is unchanged. The wake address is the interface's MAC as
  the network sees it.
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

  The route is served by `tilo-device-admin.service` on dev images, a system
  service rather than the session's endpoint, so a device at the greeter, or
  one nobody has logged in to since boot, stays reachable. It shares the
  device key: its endpoint id is derived from it, as tilo-relay's is, since
  one endpoint id published by two processes would take each other's relay
  slot, and the device key proves who answers inside the handshake. Both
  sides present their root-signed certificate and sign a challenge bound to
  the QUIC connection's two endpoint ids. The device admits only a device of
  its own identity whose certificate grants `can_administer_devices` and that
  is not revoked, bridges the session to a fresh `tilo-vmd rpc`, and logs each
  session and request to its journal. A developer's machine is certified for
  this with `tilo-enroll certify --capability admin` on the device that holds
  the identity, which also lists it among the person's devices, where Settings
  can remove it (`tilo-vm vm enrol-admin`).

## Updating what it runs

An update is a set of layers, never edits to `/usr`. Every change can be
rolled back, and the next boot either keeps or drops it on purpose.

- **Apps** update as signed tapps through the app registry, system apps
  included. The OS image is never switched just to update an app.
- **OS** updates are bootc image switches with rollback.
- **Session binaries on a dev device** are tilo-vm layers. Each one is a
  system-extension directory with recorded provenance (CI digest, or the
  commit built locally), applied to that device's session:
  - Stand-in session units bind a readonly combined `/usr` and `/opt` view.
    The daemon retains the immutable boot root in a private mount, then
    combines the global layers and that slot's layers once, avoiding another
    overlay over the host's system-extension view. Later system-extension
    refreshes cannot propagate into the retained boot root. An older shared
    retained mount is disconnected before its propagated children are removed,
    leaving the host's active mounts intact.
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

## Unlocking it after a reboot

An encrypted disk that waits for a passphrase at the machine's keyboard
undoes everything above: an update reboots, and nobody can reach the machine
until someone types it in. Unlock has three factors. Each one is optional, and
the recovery key always remains (`tilo-disk-encrypt`).

- **TPM2 with a signed PCR policy**, where the machine has a TPM Linux can
  drive. The policy is PCR 4 + PCR 7 with a signed prediction per generation,
  as `docs/security/boot-chain.md` describes, so an OS update does not fall
  back to the recovery key. The machine unlocks by itself, and only into a
  boot chain Tilo signed.
- **A sibling's consent over the network.** A LUKS slot opens with a key
  derived from two halves:
  - The device half lives on the machine, in a token in the volume's own
    LUKS2 header beside the slot it opens: sealed to the TPM where there is
    one, otherwise in the clear. Like the boot partition, the header is
    readable by anyone holding the disk, and early boot reads it without
    mounting anything.
  - The sibling half is sealed to the person's root-derived secret-sync key
    and held by their other devices.

  Early boot brings up the network and asks the person's devices for the
  sibling half over the peer network (relay fallback included). It
  authenticates with a boot-only key pair whose public half the person's
  devices certified when the slot was enrolled. A sibling releases its half
  only after the owner approves the typed `device.unlock` action, which works
  like any other action from their own identity: a prompt on their phone or
  laptop, or an admin invoking it, a developer's tilo-vm among them. Policy can
  auto-approve a request from a machine whose TPM attested the expected boot
  chain. A stolen machine can ask, but it cannot unlock without the owner. A
  copied boot partition alone gets nothing.
- **The passphrase or recovery key** at the keyboard, as today.

Wi-Fi-only machines need the network in early boot. The initramfs runs the
image's NetworkManager, only for a root enrolled for sibling unlock: wired
links come up with DHCP, plus the networks the owner chose for unlocking
(`tilo-disk-encrypt enrol --sibling --network NAME`), and no others. Their
credentials are stored with the device half: sealed to the TPM where there is
one, otherwise in the clear, readable by anyone holding the disk. They unlock
nothing on their own.

Settings (Privacy & Security, "Disk unlock") shows each enrolled volume: whether
its secrets are sealed to the TPM or stored in the clear in the disk header,
saying plainly that anyone holding the disk can read the clear ones, which
devices are asked, and which Wi-Fi networks may unlock it, with a control to
use or stop using each saved Wi-Fi network. The description of an enrolment
(`/var/lib/tilo/sibling-unlock/<uuid>.json`) is root's alone, since it names
the networks the owner trusts; Settings reads it with `pkexec
tilo-sibling-unlock volumes`, which polkit grants an administrator at the
machine's seat without a prompt. Changing the networks runs the same enrolment
(`tilo-sibling-unlock enrol --replace-networks`, or `tilo-disk-encrypt enrol
--sibling --replace-networks`) after administrator authentication; it keeps
the slot, its certificate and the siblings, and seals the new networks the way
the old ones were.

The owner can enroll an already-open encrypted root over the existing
authenticated device-admin route, using its declared semantic resource:

```bash
tilo-vm vm actions --device yoga list "disk unlock"
tilo-vm vm actions --device yoga invoke device-unlock enroll@device-unlock --args-stdin
```

The adapter carries the same `actions_enumerate` and `actions_invoke` operations
as `tilo-control`. VMD publishes `ActionSpec`s and uses the shared console
catalogue and admission code; it has no additional control socket. The invoke
command reads a JSON object on private stdin, containing `passphrase`, optional
saved `networks`, and `confirm: true`. Obtain that input from a private producer;
never put the disk passphrase in command arguments or shell history. The
passphrase is a `Secret` parameter and is absent from the encoded operation,
resource catalogue, state and results.

This operation requires the device owner's current signed-in session and the
normal administrator certificate/transport admission. It derives the current
LUKS2 root and account from the host; callers cannot select a device path, key
file, account, slot key or boot identity. The fixed enrollment helper receives
the passphrase through a sealed anonymous descriptor. TPM sealing is required
unless `allow_unsealed: true` explicitly permits readable device-factor and
network material. The chosen sealing mode is pinned before a slot is added;
TPM failure does not fall back to cleartext. Failed registration still removes
the new slot through the normal helper rollback path.

The device must contain both the typed VMD action and the matching
`tilo-sibling-unlock --require-tpm` helper. Enrollment needs an existing disk
passphrase or recovery key; a login password does not acquire that authority.
This does not enroll a locked disk or replace the existing early-boot approval
and certificate checks.

The prompt for the passphrase is up while early boot asks; whichever answers
first opens the volume. The client answers systemd-cryptsetup's passphrase
request as a password agent, and only with a key it has already checked
against the slot, so a failed or refused attempt costs the person nothing and
leaves the prompt as it was.

The Lenovo Yoga Slim 7x has no TPM Linux can use yet. Its firmware TPM sits
behind Qualcomm's TEE, and the driver for it (`tpm_qcom` on QCOMTEE) is still
in review. So on that machine the sibling factor is what replaces typing the
passphrase.

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
- A VM with an encrypted disk reboots and is unlocked by its paired sibling
  after `device.unlock` is approved (`os/dev/test-nearby-pairing.sh
  --unlock`). A denied request, a replayed earlier exchange, a boot key that
  was never certified, and a device removed from the fleet each leave it at
  the passphrase prompt, with the attempt in the owner's history.
