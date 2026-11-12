# Authenticator — deutsche Übersetzung (de-DE).

## Blatt zum Hinzufügen eines Kontos: sein Titel je Ablauf
totp-add-title-add = Konto hinzufügen
totp-add-title-edit = Konto bearbeiten
totp-add-title-provision = Auf diesem Gerät einrichten

## Wege, ein Konto zu übernehmen
totp-add-scan-title = QR-Bild scannen
totp-add-scan-description = Den Code aus einem Screenshot oder Foto lesen
totp-scan-unreadable = Aus diesem Bild war kein Code zu lesen
totp-add-link-title = Einrichtungslink einfügen
totp-add-link-description = Alles aus einem otpauth-Link übernehmen
totp-add-link-label = Einrichtungslink

## Felder zum Eintragen
totp-add-details = Oder Angaben eintragen
totp-add-placeholder-issuer = Name des Dienstes
totp-add-placeholder-account = E-Mail oder Benutzername
totp-add-placeholder-secret = Schlüssel vom Dienst
totp-add-label-issuer = Anbieter
totp-add-label-account = Kontoname
totp-add-label-secret = Geheimer Schlüssel
totp-add-advanced = Erweiterte Optionen
totp-add-label-algorithm = Algorithmus
totp-add-label-digits = Stellen
totp-add-label-period = Zeitraum in Sekunden
totp-add-invalid-period = Gib eine ganze Anzahl Sekunden zwischen 5 und 300 ein

## Fußzeile
totp-add-cancel = Abbrechen
totp-add-save-add = Konto hinzufügen
totp-add-save-edit = Änderungen übernehmen
totp-add-save-provision = Einrichten

## Verlauf

totp-stored-account-issuer = Das Konto { $item } umbenannt
totp-stored-account-label = Den Kontonamen von { $item } geändert
totp-stored-account-color = Die Farbe von { $item } geändert
totp-stored-account-algorithm = Geändert, wie { $item } seine Codes berechnet
totp-stored-account-digits = { $item } zeigt jetzt Codes mit { $new } Stellen
totp-stored-account-period = { $item } wechselt seinen Code jetzt alle { $new } Sekunden
totp-totp-accounts = { $op ->
    [insert] Ein Konto hinzugefügt
   *[other] Die Konten geändert
}
totp-move-up = { $item } nach oben verschoben
totp-move-down = { $item } nach unten verschoben
totp-delete = Das Konto { $item } gelöscht

## Wie das Fenster und seine Kontoliste für assistive Technik heißen

totp-a11y-app = Authenticator
totp-a11y-key-status = Schlüsselstatus
totp-a11y-retry-keys = Speichern der Schlüssel wiederholen
totp-a11y-hide-code = Code für { $name } verbergen
totp-a11y-reveal-code = Code für { $name } anzeigen
totp-a11y-codes-hidden = Codes bleiben verborgen, bis du einen anzeigst; dann sind sie einige Sekunden lesbar.
totp-a11y-copy-code = Code für { $name } kopieren
totp-a11y-copy-description = Kopiert den aktuellen Einmalcode in die Zwischenablage
totp-a11y-set-up-first = Richte den Schlüssel dieses Kontos auf diesem Gerät ein, bevor du einen Code kopierst
totp-a11y-edit = { $name } bearbeiten
totp-a11y-move-up = { $name } nach oben verschieben
totp-a11y-move-down = { $name } nach unten verschieben
totp-a11y-delete = { $name } löschen
totp-a11y-set-up = { $name } auf diesem Gerät einrichten
totp-a11y-set-up-description = Füge den Schlüssel ein oder scanne ihn, ohne ihn mit anderen im Arbeitsbereich zu teilen
totp-a11y-account = Authenticator-Konto
totp-a11y-accounts = Authenticator-Konten
totp-a11y-none = Keine Konten
totp-a11y-accounts-hint = { $total } Konten. Die Pfeiltasten wechseln das ausgewählte Konto.
totp-empty-message = Scanne einen QR-Code oder füge einen Einrichtungslink ein, um die Einmalcodes hier zu sehen.
