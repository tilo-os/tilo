## The resources and their verbs.

firewall-config = Firewall
firewall-config-start = Start firewall
firewall-config-stop = Stop firewall
firewall-config-enter-panic-mode = Enter panic mode
firewall-config-exit-panic-mode = Exit panic mode
firewall-config-delete-rule = Delete rule
firewall-config-import = Import configuration
firewall-applied = Applied configuration
firewall-applied-applied = Store applied configuration

## The zones' names, and what a rule matches.

firewall-zone-drop-title = Drop
firewall-zone-block-title = Block
firewall-zone-public-title = Public
firewall-zone-external-title = External
firewall-zone-dmz-title = DMZ
firewall-zone-work-title = Work
firewall-zone-home-title = Home
firewall-zone-internal-title = Internal
firewall-zone-trusted-title = Trusted
firewall-zone-rules = { $count ->
    [one] 1 rule
   *[other] { $count } rules
}
firewall-rule-port-title = Port { $port }
