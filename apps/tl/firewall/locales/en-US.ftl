# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-firewall = Firewall

d-so-tilo-firewall-keywords = firewall, firewalld, network, rules, security, ports, zones, sicherheit, netzwerk

d-scb6s6ci = Rule
    .doc = One rule of a zone: a service or a port, what it does with matching traffic, and where that traffic comes from.
    .noun = { $count ->
        [one] rule
       *[other] rules
    }

d-eeqhnuyu = Title
    .doc = What the rule reads as: a service's name, or its port.

d-3jndsc7i = Service
    .doc = The service it matches (`https`), or empty for a port rule.

d-dlj2kbdr = Port
    .doc = The port or port range it matches (`8080`, `8000-8100`), or empty for a service rule.

d-nconpw3q = Proto
    .doc = `tcp` or `udp`.

d-2ov2rg7l = Verdict
    .doc = `allow` or `deny`.

d-adadcwgr = Source
    .doc = The source address or network; empty for anywhere.

d-td35s7rd = Enabled
    .doc = Whether firewalld applies it on each device.

d-scb6s6ci-filter-enabled = Enabled

d-3jxrri5o = Zone
    .doc = One firewalld zone: what happens to unmatched traffic, the interfaces and services it allows, and its rules.
    .noun = { $count ->
        [one] zone
       *[other] zones
    }

d-kzbzenxe = Kind
    .doc = `drop`, `block`, `public`, `external`, `dmz`, `work`, `home`, `internal` or `trusted`.

d-xthxweeb = Target
    .doc = What happens to traffic no rule matched: `default`, `accept`, `reject` or `drop`.

d-6oz67tou = Interfaces
    .doc = The interfaces it claims.

d-fcur4ope = Services
    .doc = The services it lets through.

d-vyfun5ze = Rules
    .doc = Its rules, in the order they apply.

d-bqtwzcsu = Masquerade
    .doc = Whether it masquerades outbound traffic.

d-3jxrri5o-filter-services = Services

d-3jxrri5o-filter-masquerade = Masquerade

d-a3e4xeyn = Firewall
    .doc = The workspace's firewall configuration: one set of zones and rules every device applies to its own firewalld.
    .noun = { $count ->
        [one] firewall
       *[other] firewalls
    }

d-fzjbpclp = Enabled
    .doc = Whether firewalld runs.

d-lsn43itt = Panic mode
    .doc = Panic mode: all traffic in and out is dropped.

d-qzperbb7 = Default zone
    .doc = The active zone.

d-fjnbtnf2 = Zones
    .doc = The configured zones.

d-a3e4xeyn-filter-enabled = Enabled

d-a3e4xeyn-filter-panic_mode = Panic mode

d-z4c7ibjw = Apply config
    .doc = A configuration as the device's `apply` verb reads it: the same fields the reading carries, so one shape goes out and comes back.
    .noun = { $count ->
        [one] apply config
       *[other] apply configs
    }

d-z4rwsmkt = Enabled

d-hfx4peji = Panic mode

d-wpzvhcny = Default zone

d-fa3zy4tg = Zones

d-z4c7ibjw-filter-enabled = Enabled

d-z4c7ibjw-filter-panic_mode = Panic mode

d-gemauwoj = Apply zone
    .noun = { $count ->
        [one] apply zone
       *[other] apply zones
    }

d-ki3bym3t = Kind

d-npflzozj = Target

d-yll5vuns = Interfaces

d-kasq6ajf = Services

d-rsyraw6p = Rules

d-mi6vac6j = Masquerade

d-gemauwoj-filter-masquerade = Masquerade

d-iknj6ivk = Apply rule
    .noun = { $count ->
        [one] apply rule
       *[other] apply rules
    }

d-iddy2jy5 = Service

d-rukhcwu4 = Port

d-46qdyqd7 = Proto

d-b7tu4bhi = Verdict

d-v2uf2efw = Source

d-pqajeuq5 = Enabled

d-iknj6ivk-filter-enabled = Enabled

d-5iss4ixn = Service def
    .doc = A well-known service firewalld knows, offered in the Services tab and the rule form.
    .noun = { $count ->
        [one] service def
       *[other] service defs
    }

d-splkq5cx = Key

d-um6wchuo = Name

d-3lp2tu4x = Ports

d-lr6vcvpw = Proto

d-dguxp5s2 = Description

d-rmbzih5s = Read this device's firewalld
    .doc = Brings in what this device's firewalld holds: the zones, their services and rules, and the switches.

d-wvp77mci = Start firewall
    .doc = Starts firewalld.
    .w-there-is-no-firewall-configuration-yet = There is no firewall configuration yet

d-2jbc472l = Stop firewall
    .doc = Stops firewalld.

d-7fjzxy2n = Enter panic mode
    .doc = Enters panic mode: all traffic in and out is dropped.
    .w-there-is-no-firewall-configuration-yet = There is no firewall configuration yet

d-t5pt7a72 = Exit panic mode
    .doc = Leaves panic mode.

d-fas2qr4c = Make active zone
    .doc = Makes a zone the active one.

d-fas2qr4c-zone = Zone

d-t5sxzg6g = Turn a rule on or off
    .doc = Turns a rule on or off in a zone.

d-t5sxzg6g-zone = Zone

d-t5sxzg6g-rule = Rule

d-t5sxzg6g-on = On

d-3hwynxa2 = Allow a service
    .doc = Allows a service in a zone, or stops allowing it.

d-3hwynxa2-zone = Zone

d-3hwynxa2-service = Service

d-3hwynxa2-on = On

d-ipsf2opk = Add rule
    .doc = Adds a rule to a zone, or saves the one being edited.
    .w-choose-a-service = Choose a service
    .w-that-is-not-a-port-or = That is not a port or a port range this firewall accepts
    .w-that-is-not-an-ipv4-address = That is not an IPv4 address or network

d-ipsf2opk-zone = Zone

d-ipsf2opk-editing = Editing

d-ipsf2opk-as_service = As service

d-ipsf2opk-service = Service

d-ipsf2opk-port = Port

d-ipsf2opk-proto = Proto

d-ipsf2opk-verdict = Verdict

d-ipsf2opk-source = Source

d-qf5szroz = Delete rule
    .doc = Deletes a rule from a zone.

d-qf5szroz-zone = Zone

d-qf5szroz-rule = Rule

d-bnokiyga = Show dropped traffic
    .doc = Reads what the kernel dropped in the last hour.

d-jrxpvoqv = Arrive
    .doc = Opening the app reads this device's firewalld into the workspace's configuration when it holds none yet.

d-xcdaj2hg = Rule title of
    .w-port = Port { $port }

d-sydm4ev3 = Zone title
    .w-drop = Drop
    .w-block = Block
    .w-public = Public
    .w-external = External
    .w-dmz = DMZ
    .w-work = Work
    .w-home = Home
    .w-internal = Internal
    .w-trusted = Trusted

d-o7s22iek = Zone description
    .w-incoming-traffic-is-dropped-without-a = Incoming traffic is dropped without a reply. Outgoing traffic is allowed.
    .w-incoming-connections-are-refused-outgoing-traffic = Incoming connections are refused. Outgoing traffic is allowed.
    .w-for-untrusted-public-networks-only-the = For untrusted public networks. Only the services you allow are reachable.
    .w-for-the-outside-of-a-router = For the outside of a router that masquerades your network.
    .w-for-public-facing-hosts-with-limited = For public-facing hosts with limited access to the internal network.
    .w-for-work-networks-where-you-mostly = For work networks where you mostly trust the other computers.
    .w-for-home-networks-where-you-mostly = For home networks where you mostly trust the other computers.
    .w-for-internal-networks-where-you-mostly = For internal networks where you mostly trust the other computers.
    .w-all-network-connections-are-accepted = All network connections are accepted.

d-hmkq2bbk = Target words
    .w-accepted = Accepted
    .w-rejected = Rejected
    .w-dropped = Dropped
    .w-rejected-by-firewalld-s-default-policy = Rejected by firewalld’s default policy

d-bpd4g2ib = Port error
    .w-enter-a-port-or-a-port = Enter a port or a port range.
    .w-use-a-port-or-a-range = Use a port or a range such as 8000-8100.
    .w-use-a-port-from-1-to = Use a port from 1 to 65535.
    .w-start-a-port-range-with-the = Start a port range with the lower port.

d-d5qxe6az = Source error
    .w-use-a-network-prefix-from-0 = Use a network prefix from /0 to /32.
    .w-enter-an-ipv4-address-or-network = Enter an IPv4 address or network, such as 192.168.1.0/24.

d-nxrvlfzu = Trend scale
    .w-per-min = { $top } per { $minutes } min

d-b3fbncak = Blocked title
    .w-reject = Reject
    .w-drop = Drop
    .w-port = { $said } { $proto } port { $port }
    .w-traffic = { $said } { $proto } traffic

d-fudc3znt = Sources label
    .w-from-1-address = from 1 address
    .w-from-addresses = from { $sources } addresses

d-7fs3cjxx = Start~present

d-4r3hrblj = Enter panic~present

d-vxibtymz = Add rule~present

d-okktoj56 = Window
    .w-firewall = Firewall
    .w-zones = Zones
    .w-panic-mode-is-on = Panic mode is on
    .w-the-firewall-is-stopped = The firewall is stopped
    .w-stop = Stop
    .w-start = Start
    .w-leave-panic-mode = Leave panic mode
    .w-panic-mode = Panic mode
    .w-apply = Apply
    .w-apply-the-rules-to-the-system = Apply the rules to the system
    .w-add-rule = Add rule
    .w-read-firewalld = Read firewalld
    .w-read-this-device-s-firewalld-again = Read this device's firewalld again
    .w-make-active-zone = Make active zone
    .w-allowed-services = Allowed services
    .w-allow-rules = Allow rules
    .w-deny-rules = Deny rules
    .w-details = Details
    .w-interfaces = Interfaces
    .w-none = None
    .w-unmatched-traffic = Unmatched traffic
    .w-masquerading = Masquerading
    .w-on = On
    .w-off = Off
    .w-rules = Rules
    .w-this-zone-has-no-rules-yet = This zone has no rules yet.
    .w-allow = Allow
    .w-deny = Deny
    .w-edit = Edit { $name }
    .w-delete = Delete { $name }
    .w-services = Services
    .w-dropped-traffic = Dropped traffic
    .w-applying-the-rules-to-this-system = Applying the rules to this system
    .w-adding-a-rule-to = Adding a rule to { $zone_label }
    .w-add-a-rule-to = Add a rule to { $zone_label }
    .w-edit-the-rule-in = Edit the rule in { $zone_label }
    .w-save-rule = Save rule
    .w-cancel = Cancel
    .w-match = Match
    .w-service = Service
    .w-port = Port
    .w-8080-or-8000-8100 = 8080 or 8000-8100
    .w-protocol = Protocol
    .w-tcp = TCP
    .w-udp = UDP
    .w-action = Action
    .w-source = Source
    .w-anywhere = Anywhere
    .w-an-ipv4-address-or-network-such = An IPv4 address or network, such as 192.168.1.0/24
    .w-delete-this-rule = Delete this rule?
    .w-delete-rule = Delete rule
    .w-over-from-will-no-longer-be = { $name } over { $protocol } from { $source } will no longer be { $word }.
    .w-turn-off-the-firewall = Turn off the firewall?
    .w-enter-panic-mode = Enter panic mode?
    .w-turn-off-the-firewall-2 = Turn off the firewall
    .w-drop-all-traffic = Drop all traffic
    .w-no-rules-will-be-enforced-until = No rules will be enforced until you turn it on again. If this machine is reachable from a network you do not control, that connection can reach it without the rules you set.
    .w-all-incoming-and-outgoing-traffic-is = All incoming and outgoing traffic is dropped, including the remote session you may be using to fix this machine. Anything already connected is cut.

d-vc4u4tnw = Unavailable
    .w-firewalld-isn-t-available = Firewalld isn't available
    .w-this-computer-s-firewall-service-is = This computer's firewall service is not running. Start it, then try again.
    .w-try-again = Try again

d-jydyulkn = Services page
    .w-allowed = Allowed
    .w-blocked = Blocked

d-qehgteth = Blocked page
    .w-dropped-traffic-couldn-t-be-read = Dropped traffic couldn't be read
    .w-try-again = Try again
    .w-read-what-the-kernel-dropped-in = Read what the kernel dropped in the last hour from this device's journal.
    .w-read-dropped-traffic = Read dropped traffic
    .w-reading-dropped-traffic-from-the-system = Reading dropped traffic from the system journal…
    .w-packets-in-the-last-min = packets in the last { $blocked_window_minutes } min
    .w-refresh = Refresh
    .w-nothing-was-dropped = Nothing was dropped
    .w-the-kernel-logged-no-dropped-or = The kernel logged no dropped or rejected packets in the last { $blocked_window_minutes } minutes.
    .w-a-full-bar-is = A full bar is { $scale }
