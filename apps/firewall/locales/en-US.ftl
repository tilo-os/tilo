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
