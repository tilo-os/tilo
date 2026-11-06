# Device certificate renewal

*2026-11-05*

A device certificate may carry an expiry. A device that keeps presenting one
that has run out is refused by every host it talks to ("device cert has
expired"), so something has to reissue it before that happens, and again as
each renewal runs out. The rule is the one in
[architecture.md](architecture.md) §3.2: the only thing that signs a device
certificate is the person's account key. Renewal therefore follows the same
split as certification — whoever holds the key signs — and nothing on the
device renews itself.

## What a renewal is

A renewal is an ordinary root-signed [`DeviceCert`] for the same device key
under the same person's root, issued later, granting no more than the
certificate it replaces (`DeviceCert::renews`, one predicate shared by the
workspace, the fleet and the ceremony). It adds no device: the device was
already the person's, so a renewal never asks them to approve it, and it
cannot widen what the device may do. The hosts and workspaces take it in
place of the old certificate and keep the device the same member, with its
role and everything it wrote (the membership exchange,
`tilo_peer_state`'s `renewal_of`/`renew_member_certificate`).

## Who renews

The person's account key may sit whole on one device or be split across
several. A renewal is signed by exactly the devices that would sign a new
certificate, and by `threshold` of them when the key is split:

- **A device that holds the key** reissues its own certificate locally,
  through the same provisioning that first issued it. It needs no network.
- **A device that holds a share** gathers the other holders' signatures as a
  certification does (`Gathering::renewal`), so `threshold` shares sign.
- **A device that joined another person's identity** (an installed node, a
  laptop enrolled by a setup device, anything certified without the root)
  holds no part of the key. Nothing on it can sign anything, so it asks the
  person's devices that do, over the person's private device fabric, and
  installs the certificate that comes back. This is the same exchange a
  pairing uses, with `Step::Renew` in place of `Step::Approve`: the key
  holder checks the proposal (`check_renewal`) against the certificate its
  own fleet holds for that device and signs, without putting a question to
  the person.

Every device of the person finds the others on the fabric whether or not a
workspace is shared (`docs/device-reach.md`); the fleet, not a workspace, is
what a joined device renews through.

## When

A certificate is due halfway through its lifetime (`renewal_due_at`). Every
session checks at startup and then sleeps until the next one is due, waking
early when the network, custody or a host comes back so a device that slept
past the deadline renews as soon as it can. A joined device's session also
asks its person's devices on its own pass, off the loop, from the first pass
the fabric is up. The certificate is written before anything presents it, so
a join or an enrollment always shows the renewal.

## What is written

A renewal replaces, never copies:

- the system-facing certificate (`device/cert.bin`), for the identity that
  owns the device;
- that identity's own copy under `device/certs/<root>.bin`;
- the identity's fleet (`fleets/<identity id>.json`), which is where a host
  or a listing reads the certificate a device acts with.

`persist::install_device_cert` refuses a certificate that is not this
device's key, does not verify, grants device admission, or belongs to another
identity; a joined device has no root on disk, so it can never widen its own
certificate. Nothing here deletes a person's identity or changes an id.

## Failure

A renewal that fails is retried when what it failed on changes — the fabric
or a host becoming reachable, custody becoming available — never on a fixed
timer, and a joined device's ask is not repeated on every pass while the
person's devices are simply away. A certificate that has already run out
leaves the identity intact: startup reports it (`tilo::join_flow`) and the
session keeps trying, rather than replacing the place this device holds.
