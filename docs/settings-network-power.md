# Network and power Settings architecture

The Settings panes are policy editors and status views, not replacement
daemons. NetworkManager, ModemManager, UPower, power-profiles-daemon, logind,
and the narrow battery-cap system service remain the runtime authorities. UI
widgets call typed `os-actions` operations; they do not keep a second profile,
SIM, or hardware-capability database.

## Network

- Everyday Wi-Fi, hotspot, LAN, APN and SIM-PIN flows stay on the main page.
  Saved-profile metering, Wi-Fi MAC policy and automatic proxy discovery are
  behind **Advanced** and write NetworkManager connection settings. Tilo offers
  PAC URLs because that is NetworkManager's portable per-connection proxy
  contract; it does not invent a parallel manual-proxy service.
- Advanced Wi-Fi and LAN editors identify profiles by UUID and seed every
  control from NetworkManager before editing. Applying policy sends only
  changed properties. Unknown MAC, proxy, or IP methods are left untouched,
  and multiple saved profiles get an explicit selector rather than silently
  editing the first one.
- Enterprise Wi-Fi supports PEAP, TTLS and EAP-TLS. A valid CA is mandatory;
  EAP-TLS additionally requires a client certificate and private key. Tilo
  inspects subject, expiry and SHA-256 fingerprint with Tilo's bounded Rust X.509 parser
  implementation and refuses expired certificates. Password and private-key
  passphrase flags are `not-saved`, leaving custody to NetworkManager's secret
  agent rather than Settings JSON.
- Multi-modem systems enumerate exact ModemManager SIM object paths. Unlocking
  an ambiguous "first SIM" is rejected, PINs are held in zeroizing transient
  buffers, and the selected object is called through ModemManager D-Bus.
- Diagnostics read global state/connectivity from NetworkManager D-Bus and IP,
  gateway and DNS data through its standard client. Recommendations are based
  only on that authoritative state and do not probe or upload user traffic.

Unsupported features do not get decorative controls: mobile controls require
a reported SIM, MAC policy applies only to Wi-Fi profiles, and certificate
connections fail before creating unsafe profiles when prerequisites are not
met.

## Power

Tilo leaves CPU governor and Intel/AMD P-state EPP ownership exclusively with
power-profiles-daemon. The legacy polling `tilo-power.service` and its unused
daemon were removed: two privileged writers racing over cpufreq sysfs would
make the selected user profile unreliable and can hurt both frame pacing and
battery life.

- Settings enumerates every kernel battery and UPS for a truthful overview.
  The primary battery additionally exposes cycles and health when the driver
  reports matching full/design capacity units. Health text is an estimate,
  explains the calculation, and does not claim a diagnosis.
- Charge-cap choices come from the live normalized capability contract. After
  the privileged service returns, Settings re-reads hardware state and reports
  firmware clamp/rejection instead of showing an optimistic value.
- Saver, balanced and performance can each have an idle-suspend timeout. The
  shell-owned power service subscribes to power-profiles-daemon's
  `ActiveProfile` property over D-Bus and pushes the resolved timeout into the
  live compositor state machine. Settings-file changes update both the session
  timer and the service's lid/battery policy; old files retain the legacy
  timeout.
- Idle-suspend waits while anything holds the machine awake: media, a running
  task, an app's inhibit request, and also another program's logind `block`
  lock, such as `systemd-inhibit --what=sleep` from a remote admin's SSH
  session. A `sleep` lock holds off suspend only, so the screen still blanks
  and locks; an `idle` lock keeps the whole session awake. Closing the lid and
  critical battery still suspend.
- Charge/health history is per-user state, contains only minute, percentage,
  health estimate and AC state, coalesces samples within 15 minutes, and is
  bounded to 30 days/2,880 points. The existing shell-owned UPower monitor is
  the sole writer; battery and UPS aggregates use separate files, written
  atomically with mode `0600`. It stores no device names, serials, application
  activity, or per-process data. Settings reads the same canonical battery
  history and never races it as a second writer.
