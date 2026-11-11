## Verlauf

firewall-firewall-default-zone = { $new } zur aktiven Zone gemacht
firewall-stored-zone-target = { $new ->
    [accept] Eine Zone auf das Annehmen nicht zugeordneten Datenverkehrs gesetzt
    [reject] Eine Zone auf das Ablehnen nicht zugeordneten Datenverkehrs gesetzt
    [drop] Eine Zone auf das Verwerfen nicht zugeordneten Datenverkehrs gesetzt
   *[default] Eine Zone auf die Standardrichtlinie von firewalld gesetzt
}
firewall-stored-zone-interfaces = Die Schnittstellen einer Zone geändert
firewall-stored-zone-services = { $op ->
    [insert] Einen Dienst in einer Zone erlaubt
   *[remove] Einen Dienst in einer Zone nicht mehr erlaubt
}
firewall-stored-zone-rules = { $op ->
    [insert] Eine Regel hinzugefügt
    [move-to] Eine Regel verschoben
   *[other] Die Regeln geändert
}
firewall-stored-zone-masquerade = { $new ->
    [true] Masquerading in einer Zone eingeschaltet
   *[false] Masquerading in einer Zone ausgeschaltet
}
firewall-stored-rule-enabled = { $new ->
    [true] Eine Regel eingeschaltet
   *[false] Eine Regel ausgeschaltet
}
firewall-stored-rule-source = Geändert, woher eine Regel gilt
firewall-stored-rule-matcher = Geändert, was eine Regel trifft
firewall-stored-rule-action = { $new ->
    [deny] Eine Regel verweigert ihren Datenverkehr
   *[allow] Eine Regel erlaubt ihren Datenverkehr
}
firewall-start = Die Firewall gestartet
firewall-stop = Die Firewall gestoppt
firewall-enter-panic-mode = Den gesamten Datenverkehr verworfen (Panikmodus)
firewall-exit-panic-mode = Den Panikmodus verlassen
firewall-delete-rule = Eine Regel aus der Zone „{ $zone }“ gelöscht
firewall-import = Die Firewall-Konfiguration eines Geräts übernommen

## Barrierefreiheit
firewall-a11y-window = Firewall
firewall-source-anywhere = Überall
