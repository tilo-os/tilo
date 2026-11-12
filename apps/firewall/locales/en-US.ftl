## History

firewall-firewall-default-zone = Made { $new } the active zone
firewall-stored-zone-target = { $new ->
    [accept] Set a zone to accept unmatched traffic
    [reject] Set a zone to reject unmatched traffic
    [drop] Set a zone to drop unmatched traffic
   *[default] Set a zone to firewalld’s default policy
}
firewall-stored-zone-interfaces = Changed the interfaces of a zone
firewall-stored-zone-services = { $op ->
    [insert] Allowed a service in a zone
   *[remove] Stopped allowing a service in a zone
}
firewall-stored-zone-rules = { $op ->
    [insert] Added a rule
    [move-to] Moved a rule
   *[other] Changed the rules
}
firewall-stored-zone-masquerade = { $new ->
    [true] Turned masquerading on in a zone
   *[false] Turned masquerading off in a zone
}
firewall-stored-rule-enabled = { $new ->
    [true] Turned a rule on
   *[false] Turned a rule off
}
firewall-stored-rule-source = Changed where a rule applies from
firewall-stored-rule-matcher = Changed what a rule matches
firewall-stored-rule-action = { $new ->
    [deny] Made a rule deny its traffic
   *[allow] Made a rule allow its traffic
}
firewall-start = Started the firewall
firewall-stop = Stopped the firewall
firewall-enter-panic-mode = Dropped all traffic (panic mode)
firewall-exit-panic-mode = Left panic mode
firewall-delete-rule = Deleted a rule from the “{ $zone }” zone
firewall-import = Took in the firewall configuration of a device

## Accessibility
firewall-a11y-window = Firewall
firewall-source-anywhere = Anywhere

## Window and accessibility copy
firewall-a11y-services = Services
firewall-a11y-dropped = Dropped traffic, grouped by rule
firewall-reading-dropped-stopped = Open Blocked again to read the system journal.
firewall-reading-dropped = Reading dropped traffic from the system journal…
firewall-a11y-rules = Rules
firewall-zone-empty = This zone has no rules yet.
firewall-applying-rules = Applying the stored rules to this device…
firewall-a11y-zones = Firewall zones

## Facts, property labels and section titles
firewall-a11y-allow-service = Allow { $service }
firewall-packets-window = packets in the last { $minutes } min
firewall-a11y-enable-rule = Enable the { $rule } rule
firewall-masquerading = Masquerading
firewall-unmatched-traffic = Unmatched traffic
firewall-section-details = Details
firewall-interfaces = Interfaces

## Facts, property labels and section titles
firewall-delete-rule-body = { $rule } over { $protocol } from { $source } will no longer be { $action }.
firewall-state-allowed = allowed
firewall-state-denied = denied

## What the window shows
firewall-make-active = Make active zone
firewall-masquerade-on = On
firewall-masquerade-off = Off
firewall-edit-rule = Edit the { $rule } rule
firewall-delete-rule-action = Delete the { $rule } rule
firewall-edit-rule-in-zone = Edit the rule in { $zone }
firewall-add-rule-to-zone = Add a rule to { $zone }
firewall-refresh = Refresh
firewall-stop-title = Turn off the firewall?
firewall-stop-body = No rules will be enforced until you turn it on again. If this machine is reachable from a network you do not control, that connection can reach it without the rules you set.
firewall-stop-confirm = Turn off the firewall
firewall-panic-title = Enter panic mode?
firewall-panic-body = All incoming and outgoing traffic is dropped, including the remote session you may be using to fix this machine. Anything already connected is cut.
firewall-panic-confirm = Drop all traffic

## What each zone is for
firewall-zone-drop-description = Incoming traffic is dropped without a reply. Outgoing traffic is allowed.
firewall-zone-block-description = Incoming connections are refused. Outgoing traffic is allowed.
firewall-zone-public-description = For untrusted public networks. Only the services you allow are reachable.
firewall-zone-external-description = For the outside of a router that masquerades your network.
firewall-zone-dmz-description = For public-facing hosts with limited access to the internal network.
firewall-zone-work-description = For work networks where you mostly trust the other computers.
firewall-zone-home-description = For home networks where you mostly trust the other computers.
firewall-zone-internal-description = For internal networks where you mostly trust the other computers.
firewall-zone-trusted-description = All network connections are accepted.
firewall-service-unavailable-hint = Start firewalld, then try again. No rules were changed.
firewall-panic-mode-message = All incoming and outgoing network traffic is dropped.
firewall-stopped-message = No rules are enforced until you turn it on.
firewall-nothing-dropped-message = The kernel logged no dropped or rejected packets in the last { $minutes } minutes.

## The window bar's slots
firewall-tab-blocked = Blocked
firewall-turn-on = Turn on the firewall
firewall-bar-applying = Applying the rules to the system
firewall-bar-apply = Apply the rules to the system
firewall-add-rule = Add rule
firewall-panic-mode = Panic mode
firewall-views = View

## Actions

firewall-effect-apply-config = Apply changes
firewall-effect-retry = Try again
firewall-effect-read-blocked = Show dropped traffic
firewall-config = Firewall
firewall-config-start = Start firewall
firewall-config-stop = Stop firewall
firewall-config-enter-panic-mode = Enter panic mode
firewall-config-exit-panic-mode = Exit panic mode
firewall-config-delete-rule = Delete rule
firewall-config-import = Import configuration
firewall-applied = Applied configuration
firewall-applied-applied = Store applied configuration

## What the window names
firewall-zone-drop-title = Drop
firewall-zone-block-title = Block
firewall-zone-public-title = Public
firewall-zone-external-title = External
firewall-zone-dmz-title = DMZ
firewall-zone-work-title = Work
firewall-zone-home-title = Home
firewall-zone-internal-title = Internal
firewall-zone-trusted-title = Trusted
firewall-action-allow-title = Allow
firewall-action-deny-title = Deny
firewall-target-accepted = Accepted
firewall-target-rejected = Rejected
firewall-target-dropped = Dropped
firewall-rule-port-title = Port { $port }
firewall-blocked-action-drop = Drop
firewall-blocked-action-reject = Reject
firewall-blocked-title-port = { $action } { $proto } port { $port }
firewall-blocked-title-traffic = { $action } { $proto } traffic
firewall-blocked-title-plain = { $action }
firewall-panic-banner = Panic mode is on
firewall-stopped-banner = The firewall is stopped
