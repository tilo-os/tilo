# Your devices as a VPN

One of a person's devices can route another of theirs out to the internet:
a laptop on hotel Wi-Fi goes out through the desktop at home, or reaches the
home network. The two devices are already certified peers of one identity
(`docs/device-mesh.md`) with an iroh connection between their shells, so the
tunnel rides that connection: NAT traversal and relay fallback come with it,
and nobody configures keys, ports or endpoints.

## What the person sees

- **Exit side.** Settings → Network → VPN has "Let my other devices use this
  device as a VPN", off by default and per device. While one of their
  devices routes through it, a row names that device, with Stop.
- **Client side.** Each of their devices that offers an exit appears as
  "Through <device name>" in the VPN list and in quick settings, beside the
  person's own VPN profiles. Options: all traffic or only the exit's home
  network(s); DNS through the exit; a kill switch that blocks traffic while
  the tunnel is down.
- The same state and controls are actions on the `network.device-vpn`
  resources (connect through a device, stop, offer this device, stop a device
  using it), so automation, agents and assistive technology reach exactly
  what the switches reach.

## Pieces

| Piece | Where | Owns |
| --- | --- | --- |
| Helper | `crates/system/device-vpn` (`tilo-device-vpn`, root system service) | TUN devices, routes and policy rules, NetworkManager DNS for the tunnel, IPv4 forwarding, the nftables kill switch and masquerade |
| Tunnel | same crate, feature `tunnel` | the `tilo-device-vpn/1` ALPN: handshake, exit authorisation, IP packets as QUIC datagrams, MTU, the exit's DNS relay |
| Controller | `crates/session/shell/src/shell/device_vpn/` | the person's choices, the client and exit state machines, the status Settings and quick settings show, the actions |
| Choices and status | `settings_store::device_vpn` | the settings leaves Settings writes and the shell follows; the status file the shell publishes |

### The helper

A socket-activated root service (`tilo-device-vpn.socket`), the same shape as
the NTFS mount service: one JSON request per connection on
`/run/tilo-device-vpn/control.sock`, decided per connection from the kernel's
account of the caller. Only the compositor of the person at the seat may ask
(the seat's active uid, `/usr/bin/shell` read through a pidfd-pinned
`/proc/<pid>/exe`, sharing the service's user and mount namespaces, which no
sandboxed app does). Every other caller is refused before its request is read.

The protocol is typed and closed: `status`, `open_client`, `close_client`,
`open_exit`, `close_exit`. A request names addresses, prefixes, ports and
switches, never an interface name, command, path or rule; every value is
range-checked before use, and the helper builds each `ip`/`nft` invocation
itself from the checked values. An opened tunnel's TUN descriptor travels back
over the socket (`SCM_RIGHTS`); the helper keeps no copy, so the device and
its routes vanish when the shell lets go of it or dies.

Client (`tilovpn0`, `100.100.64.N/24`):

- Routes go in table 5270, reached through policy rules that match only the
  owner's uid (`uidrange`): another person logged in on the same computer
  is never routed through someone else's device. Before the tunnel rule, the
  carrier is exempted: UDP from the shell's iroh sockets (`sport`) and TCP/UDP
  to the iroh relays' addresses, so the tunnel's own packets never loop into
  it. All traffic keeps the local network reachable
  (`suppress_prefixlength 0`); IPv6 to the internet is unreachable for the
  owner while all traffic goes through the exit, since the tunnel carries
  IPv4 only, so nothing leaks around it.
- DNS through the exit sets NetworkManager's global DNS to the tunnel's
  gateway. NetworkManager keeps that across restarts, so the helper records
  it in `/var/lib/tilo/device-vpn` and `tilo-device-vpn-reset.service` puts it
  back at boot if the helper could not.
- The kill switch is an nftables output chain for the owner's uid that lets
  through only the tunnel, loopback, the local network and the carrier. It
  stays in place when the tunnel drops (the TUN is gone, the chain is not)
  until the shell reconnects or the person stops; it goes when the owner's
  session ends.

Exit (`tiloexit0`, `100.100.64.1/24`):

- IPv4 forwarding on (put back off afterwards if the helper turned it on, and
  while it is the one that did, forwarding between other interfaces is
  dropped so the machine routes for nobody else), masquerade of the tunnel
  network out of whatever uplink routes it, source addresses checked against
  the tunnel network, no new connections from outside into the tunnel, TCP MSS
  clamped to the path.
- firewalld's forward chain rejects what no zone policy accepts, so the image
  binds `tiloexit0` to the `tilo-device-vpn` zone with a policy that accepts
  forwarding from it to any zone (`/usr/lib/firewalld`). NetworkManager leaves
  both interfaces alone (`managed=0`).

The helper checks every few seconds that its interfaces still exist and that
the owner still has a session, cleans up what lost its reason, and exits after
a minute with nothing open.

### The tunnel

`tilo-device-vpn/1` on the shell's peer endpoint. The client opens one
bidirectional control stream and says why it came (`probe` or `route`); the
exit answers with an offer (gateway, the client's address, MTU, its home
networks) or closes the connection with a reason code.

- **Authorisation is at the exit.** The remote endpoint key must be one of
  the person's own devices (`personal_device_peers`: enrolled or certified by
  their root, not revoked) and the exit switch must be on. A foreign device is
  closed with `not-yours` before the exit reads a byte from it; with the
  switch off, `exit-off`. The decision is checked again while a session runs,
  so turning the switch off or revoking a device ends its sessions.
- **Packets** are IPv4 packets, verbatim, one per QUIC datagram. The exit
  only accepts packets whose source is the address it gave that client, the
  client only packets for its own address. A client's address is stable per
  device for the exit's session.
- **MTU.** The client's TUN MTU is the connection's datagram size at connect
  (at least 576). A packet that no longer fits (the path changed to the relay,
  say) is answered with an ICMP "fragmentation needed" naming the current
  size, written back into the TUN on whichever side it entered, so TCP and
  path MTU discovery shrink to fit. The exit also clamps TCP MSS.
- **DNS through the exit** goes to the tunnel's gateway address. The exit's
  shell answers UDP queries to it itself, by asking its own resolver and
  writing the answer back, so the client resolves what the exit resolves,
  home network names included.

### The controller

The person's choices are settings leaves (device-local): `device_vpn.exit`
(off by default), `device_vpn.through` (the exit device and its options, or
none), and `device_vpn.stop` (a one-off request to stop a device using this
exit). Settings, quick settings and the actions all write those; the shell
follows them and makes the tunnel match.

- **Offers** are a presence fact: a device whose exit switch is on (and whose
  image has the helper) sets `vpn_exit` in its fabric capabilities, so the
  person's other devices list it without asking each one.
- **Client states:** connecting, connected (through which device, since
  when), reconnecting, blocked (kill switch on, tunnel down), and failed with
  the reason in the person's words. A tunnel that drops reconnects with
  backoff. With the kill switch the helper's block stays in place meanwhile;
  without it, routes and DNS are undone at once so traffic flows normally.
- **Stop at the exit** ends that device's session with `stopped`; the client
  then clears its own choice, so it does not dial straight back, and says it
  was stopped from the exit.
- **Status** is published to `$XDG_RUNTIME_DIR/tilo/device-vpn.json` (with the
  publisher's pid and start time, like the display snapshot), which Settings
  reads; quick settings and the actions read it in process.

## Limits

- IPv4 through the tunnel. IPv6 is held back (all traffic) rather than leaked.
- DNS through the exit answers UDP queries; a truncated answer's TCP retry
  is not relayed.
- The carrier exemption names the relays' addresses at connect time. If a
  relay moves to another address mid-session, the relay path stops until the
  next reconnect (the direct path is unaffected).
- Traffic of other accounts on the client, and of system services, does not
  go through the tunnel; DNS through the exit applies to the whole machine,
  since NetworkManager's resolver is shared.
