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

## Fenster- und Barrierefreiheits-Texte
firewall-a11y-services = Dienste
firewall-a11y-dropped = Verworfener Datenverkehr, nach Regel gruppiert
firewall-reading-dropped-stopped = Öffne Blockiert erneut, um das Systemjournal zu lesen.
firewall-reading-dropped = Verworfener Datenverkehr wird aus dem Systemjournal gelesen…
firewall-a11y-rules = Regeln
firewall-zone-empty = Diese Zone hat noch keine Regeln.
firewall-applying-rules = Die gespeicherten Regeln werden auf dieses Gerät angewendet…
firewall-a11y-zones = Firewall-Zonen

## Fakten, Eigenschaftsnamen und Bereichstitel
firewall-a11y-allow-service = { $service } zulassen
firewall-packets-window = Pakete in den letzten { $minutes } Minuten
firewall-a11y-enable-rule = Regel { $rule } aktivieren
firewall-masquerading = Maskierung
firewall-unmatched-traffic = Nicht zugeordneter Datenverkehr
firewall-section-details = Details
firewall-interfaces = Schnittstellen

## Fakten, Eigenschaftsnamen und Bereichstitel
firewall-delete-rule-body = { $rule } über { $protocol } von { $source } wird nicht mehr { $action }.
firewall-state-allowed = zugelassen
firewall-state-denied = abgelehnt

## What the window shows
firewall-make-active = Zur aktiven Zone machen
firewall-masquerade-on = An
firewall-masquerade-off = Aus
firewall-edit-rule = Die Regel { $rule } bearbeiten
firewall-delete-rule-action = Die Regel { $rule } löschen
firewall-edit-rule-in-zone = Die Regel in { $zone } bearbeiten
firewall-add-rule-to-zone = Eine Regel zu { $zone } hinzufügen
firewall-refresh = Neu laden
firewall-stop-title = Firewall ausschalten?
firewall-stop-body = Bis du sie wieder einschaltest, wird keine Regel durchgesetzt. Ist dieser Rechner aus einem Netz erreichbar, das du nicht kontrollierst, kommt eine Verbindung von dort ohne deine Regeln an ihn heran.
firewall-stop-confirm = Firewall ausschalten
firewall-panic-title = Panikmodus einschalten?
firewall-panic-body = Der gesamte ein- und ausgehende Verkehr wird verworfen, auch die entfernte Sitzung, mit der du diesen Rechner vielleicht gerade reparierst. Bestehende Verbindungen werden getrennt.
firewall-panic-confirm = Gesamten Verkehr verwerfen

## What each zone is for
firewall-zone-drop-description = Eingehender Datenverkehr wird ohne Antwort verworfen. Ausgehender Datenverkehr ist erlaubt.
firewall-zone-block-description = Eingehende Verbindungen werden abgelehnt. Ausgehender Datenverkehr ist erlaubt.
firewall-zone-public-description = Für nicht vertrauenswürdige öffentliche Netzwerke. Nur die von dir erlaubten Dienste sind erreichbar.
firewall-zone-external-description = Für die Außenseite eines Routers, der dein Netzwerk maskiert.
firewall-zone-dmz-description = Für öffentlich erreichbare Hosts mit begrenztem Zugang zum internen Netzwerk.
firewall-zone-work-description = Für Arbeitsnetzwerke, in denen du den anderen Computern meist vertraust.
firewall-zone-home-description = Für Heimnetzwerke, in denen du den anderen Computern meist vertraust.
firewall-zone-internal-description = Für interne Netzwerke, in denen du den anderen Computern meist vertraust.
firewall-zone-trusted-description = Alle Netzwerkverbindungen werden angenommen.
