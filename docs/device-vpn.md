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
- The same state and controls are actions on the person's devices in the
  palette's continuity actions (`vpn.through.*`, `vpn.stop.*`,
  `vpn.exit.stop.*`, `vpn.exit.share`/`unshare`), dispatched through the same
  device-fabric action path as the other continuity controls, so automation,
  agents and assistive technology reach exactly what the switches reach.

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
`open_exit`, `close_exit`, and for the person's VPN profiles
`hold_profile_block`, `release_profile_block` (see "Profile kill switch"). A request names addresses, prefixes, ports and
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
  binds `tiloexit0` to the `tilo-device-vpn` zone with a policy
  (`tilo-device-vpn-out`; firewalld refuses a policy named like a zone) that accepts
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

- **Offers** are learnt by asking: when the person's online devices change,
  and every five minutes, the shell probes each one on the tunnel ALPN, and
  lists those that answer with an offer. (A fabric capability would spare the
  probes, but adding a field changes the fabric's Postcard schema, which
  devices on another version would then refuse whole.)
- **Client states:** connecting, connected (since when), reconnecting (with
  whether the kill switch blocks meanwhile), refused (the exit does not count
  this device as the person's, its switch is off, or it has no room), and
  failed (the helper is not installed, refused the route, or "only my home
  network" found none). A tunnel that drops reconnects with backoff (one
  second doubling to a minute); a refusal that may change by itself is asked
  again every thirty seconds. Once the tunnel has been up, the kill switch
  keeps the helper's link (and so its block) while it is down; without it, the
  link is undone at once so traffic flows normally. The decisions are pure
  functions (`controller.rs`), tested apart from the I/O.
- **Stop at the exit** ends that device's session with `stopped`; the client
  then clears its own choice, so it does not dial straight back, and says it
  was stopped from the exit.
- **Status** is published to `$XDG_RUNTIME_DIR/tilo/device-vpn.json` (with the
  publisher's pid and start time, like the display snapshot), which Settings
  reads with `DeviceVpnStatus::load`; quick settings and the actions read it
  in process.

### Settings

`settings_store::device_vpn` is the whole seam for Settings → Network → VPN:

- `DeviceVpnStatus::load()` and `rows()`: one `DeviceVpnRow` per device this
  one routes or could route through (the current one first), each titled
  "Through" and the device's name, with its `ClientState` while on. Turning a
  row on writes `THROUGH` (`Through::device(id)`, options in `routes`, `dns`,
  `kill_switch`); off clears it.
- The opt-in row is `EXIT` (off by default; offer it while
  `exit_available`), with `exit_users` listed under it, each with Stop, which
  writes `STOP` (`StopRequest::after(previous, device)`).

## Profile kill switch

The person's own `NetworkManager` VPN profiles (WireGuard, OpenVPN, the
plugin types) each have a kill switch on their page in Settings → Network →
VPN, off until turned on, with "Allow local network" under it. While a
profile with the switch on is set to connect and is not connected, nothing
leaves the computer but through a tunnel, to the profile's servers, and for
address configuration; the profile's state reads "Blocking traffic".

A profile is set to connect from when `NetworkManager` starts bringing it up
until the person turns it off on purpose: `NetworkManager` reports it went
down because the person asked (reason `USER_DISCONNECTED`, from Settings,
quick settings or `nmcli`) or the profile was removed, or Settings asks to
lift a profile that is not up (`PROFILE_RELEASE`, its switch turned off while
blocking). A tunnel that drops, or never comes up, keeps blocking.

The same root helper holds the block, rather than a sibling, so one root
service owns what the person's session adds to the firewall, behind the one
seat-peer check, record, housekeeping and reset:

- `hold_profile_block { servers, allow_local }` names at most 16 server
  addresses (no loopback, multicast or unspecified ones) and one switch; the
  helper writes an `inet` table `tilo_device_vpn_profile` in one `nft` script
  that replaces the previous one atomically. Its output and forward hooks
  accept loopback, interfaces of kind `tun`, `wireguard` or `ppp`, IPsec
  traffic, DHCP and DHCPv6, ICMPv6 neighbour discovery and MLD, and the
  servers; with `allow_local` also the private and link-local ranges and
  local multicast and broadcast. Everything else is rejected
  (administratively prohibited), so a program fails at once rather than
  waiting. The block is machine-wide: a VPN profile is the machine's.
- `release_profile_block` deletes the table. Another account cannot lift or
  replace a block while its owner is logged in. The block outlives the
  owner's session, since a crashed shell ends it and the block exists for
  that moment; once the owner is logged out the person at the seat may
  lift it. Only `reset` at boot (`tilo-device-vpn-reset.service`) lifts a
  block nobody released. The table needs no running process, so the helper
  still exits when idle and a restarted helper takes the block over from
  its record.

In the session (`shell/device_vpn/profile_block.rs`) a controller follows
the choices (`PROFILE_KILL_SWITCHES`, `PROFILE_RELEASE`) and, once any switch
is on, `NetworkManager`'s active VPNs and their `StateChanged` reasons
(`tilo_nm::NmClient::watch_vpns`). `device_vpn::profile::Intent` decides
what blocks; the servers of a profile are read from its settings (WireGuard
peers' endpoints, a plugin's `remote`/`gateway`) and resolved when it starts
connecting, while traffic still flows. A block the helper refused is asked
for again with backoff, and `DeviceVpnStatus.profile_block` tells Settings
what blocks and whether blocking failed.

The intent is durable. Before it asks the helper, the controller keeps
which profiles are set to connect, each with its servers' addresses, and
the last release it acted on, in the settings (`PROFILE_INTENT`, written
by the session alone). A shell starting after one crashed adopts it: a
profile still set to connect and down keeps blocking, with the servers
looked up before (nothing can be looked up while the block holds), and the
block is lifted only for a profile the person turned off meanwhile (a
newer `PROFILE_RELEASE`) or whose kill switch they turned off.

## Tests

- The helper's decisions against a fake backend (`service/tests.rs`): another
  account refused, out-of-bounds requests refused before anything runs, a
  failed step undone, the kill switch surviving a dropped tunnel, forwarding
  put back.
- The tunnel over two loopback iroh endpoints with in-memory devices
  (`tunnel/tests.rs`): another person's device refused, the person's device
  refused while the exit is off, packets both ways, spoofed sources dropped,
  stop and switch-off ending a running tunnel, DNS through the gateway, and a
  packet too large answered with "fragmentation needed".
- Two network namespaces through real TUN devices set up by the helper's
  Linux backend (`tests/netns_tunnel.rs`), skipped without `/dev/net/tun`,
  `ip`, `nft` and user namespaces unless `TILO_REQUIRE_TUN=1`; t's
  `device-vpn-namespaces` check provides them.
- The controller's kill switch and refusal handling (`controller.rs`).
- The profile kill switch: the decisions (`profile.rs`), the nftables script
  (`service/plan.rs`), and the helper holding, replacing, lifting and
  refusing blocks (`service/tests.rs`); in a user and network namespace on
  the Linux backend (`tests/netns_profile_block.rs`, in
  `device-vpn-namespaces`), the block coming and going with the profile's
  state as checked by real sends, a caller other than the seat's shell
  refused through the real socket server, a block left by a helper that
  aborted kept by the next one (also once the owner's session is gone)
  until reset or released from the seat, and a block held by a deciding
  process that aborted still up, then taken over from its kept intent with
  the server reachable and lifted only when turned off. Settings tests
  cover the switch, the `set-kill-switch` action, the blocking state and
  the release (`apps/settings/.../tests/vpn_kill_switch.rs`).

## Limits

- IPv4 through the tunnel. IPv6 is held back (all traffic) rather than leaked.
- DNS through the exit answers UDP queries; a truncated answer's TCP retry
  is not relayed.
- The carrier exemption names the relays' addresses at connect time. If a
  relay moves to another address mid-session, the relay path stops until the
  next reconnect (the direct path is unaffected).
- A profile kill switch lets through the servers' addresses as resolved
  when the profile started connecting. While it blocks, DNS goes nowhere
  unless the resolver is on an allowed local network, so a server that moves
  to another address is reached only after the person turns the profile off
  and on.
- A profile that blocks while it is down has nothing for `nmcli` to take
  down: only turning it off in Settings (or its kill switch) lifts the block.
- Traffic of other accounts on the client, and of system services, does not
  go through the tunnel; DNS through the exit applies to the whole machine,
  since NetworkManager's resolver is shared.
