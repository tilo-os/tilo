## Die Ressourcen und ihre Verben.

firewall-config = Firewall
firewall-config-start = Firewall starten
firewall-config-stop = Firewall stoppen
firewall-config-enter-panic-mode = Panikmodus einschalten
firewall-config-exit-panic-mode = Panikmodus verlassen
firewall-config-delete-rule = Regel löschen
firewall-config-import = Konfiguration importieren
firewall-applied = Angewendete Konfiguration
firewall-applied-applied = Angewandte Konfiguration speichern

## Die Namen der Zonen und was eine Regel trifft.

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
    [one] 1 Regel
   *[other] { $count } Regeln
}
firewall-rule-port-title = Port { $port }
