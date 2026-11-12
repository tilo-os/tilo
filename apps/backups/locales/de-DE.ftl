# Backups: gezählte Texte.
#
# Jeder Text mit einer Anzahl steht hier, damit jede Sprache ihre eigene
# Einzahl und Mehrzahl formuliert. `$files` und `$snapshots` sind die für die
# Anzeige formatierten Anzahlen; `$count` wählt die Pluralform.

## Zustand, wie die Statusblase ihn meldet
backups-health-ok = Keine fehlgeschlagenen Sicherungen
backups-health-attention = { $count ->
    [one] Eine Sicherung braucht Aufmerksamkeit
   *[other] { $snapshots } Sicherungen brauchen Aufmerksamkeit
}

## Die Änderung einer Sicherung, in der Zeitachse und in ihren Details
backups-changed-files = { $count ->
    [one] { $size } in einer Datei geändert
   *[other] { $size } in { $files } Dateien geändert
}
backups-size-in-files = { $count ->
    [one] { $size } in einer Datei
   *[other] { $size } in { $files } Dateien
}
backups-progress-files = { $count ->
    [one] Eine Datei kopiert, { $size }
   *[other] { $files } Dateien kopiert, { $size }
}

## Übersichtskacheln: die Einheit hinter einer Zahl
backups-unit-minutes = { $count ->
    [one] Minute
   *[other] Minuten
}
backups-unit-minutes-ago = { $count ->
    [one] Minute her
   *[other] Minuten her
}
backups-unit-hours-ago = { $count ->
    [one] Stunde her
   *[other] Stunden her
}
backups-unit-days-ago = { $count ->
    [one] Tag her
   *[other] Tage her
}

## Aufbewahrung
backups-retention-total = { $count ->
    [one] Jede Quelle behält höchstens eine Sicherung, dazu die, die bis zum Entfernen aufbewahrt wird.
   *[other] Jede Quelle behält höchstens { $snapshots } Sicherungen, dazu die, die bis zum Entfernen aufbewahrt werden.
}

## Aufbewahrung: was die Anzahlen auf dem Bildschirm entfernen würden
backups-retention-removes-none = Nichts würde entfernt: Die nächste Sicherung behält jede Sicherung.
backups-retention-removes-one = Die nächste Sicherung würde eine Sicherung entfernen, aufgenommen { $when }.
backups-retention-removes-many = { $count ->
    [one] Die nächste Sicherung würde { $snapshots } Sicherung entfernen: { $when }
   *[other] Die nächste Sicherung würde { $snapshots } Sicherungen entfernen: { $when }
}

## Verlauf

backups-retention-policy-hourly = { $new ->
    [0] Keine stündlichen Sicherungen aufbewahren
    [one] Eine stündliche Sicherung aufbewahren
   *[other] { $new } stündliche Sicherungen aufbewahren
}
backups-retention-policy-daily = { $new ->
    [0] Keine täglichen Sicherungen aufbewahren
    [one] Eine tägliche Sicherung aufbewahren
   *[other] { $new } tägliche Sicherungen aufbewahren
}
backups-retention-policy-weekly = { $new ->
    [0] Keine wöchentlichen Sicherungen aufbewahren
    [one] Eine wöchentliche Sicherung aufbewahren
   *[other] { $new } wöchentliche Sicherungen aufbewahren
}
backups-retention-policy-monthly = { $new ->
    [0] Keine monatlichen Sicherungen aufbewahren
    [one] Eine monatliche Sicherung aufbewahren
   *[other] { $new } monatliche Sicherungen aufbewahren
}

## Fenster- und Barrierefreiheits-Texte
backups-file-list-pending = Die Dateiliste dieser Sicherung ist noch nicht verfügbar.
backups-restore-note = Die ganze Sicherung wird in einen neuen oder leeren Ordner deiner Wahl zurückgespielt, und jede Datei wird geprüft. Nichts, was schon vorhanden ist, wird ersetzt.
backups-heading-snapshots = Sicherungen
backups-a11y-order = Reihenfolge der Sicherungen
backups-a11y-window = Sicherungen

## Fakten, Eigenschaftsnamen und Bereichstitel
backups-section-contents = Inhalt
backups-stored-on = Gespeichert auf
backups-size = Größe
backups-taken = Erstellt
backups-encryption = Verschlüsselung
backups-folder = Ordner
backups-source = Quelle
backups-failure = Fehler
backups-changed = Geändert
backups-retention = Aufbewahrung
backups-type = Typ
backups-status = Status
backups-last-backup = Letzte Sicherung
backups-capacity = Kapazität
backups-free = Frei
backups-used = Belegt
backups-kind = Art
backups-schedule = Zeitplan

## Felder der Aufbewahrungsregel
backups-retention-heading = Aufzubewahrende Sicherungen
backups-keep-hourly = Stündliche Sicherungen aufbewahren
backups-keep-daily = Tägliche Sicherungen aufbewahren
backups-keep-weekly = Wöchentliche Sicherungen aufbewahren
backups-keep-monthly = Monatliche Sicherungen aufbewahren

## What the window shows
backups-verifying-on = Wird auf { $destination } geprüft
backups-copy-failed = Kopieren nach { $destination } fehlgeschlagen: { $reason }
backups-copy-failed-unrecorded = Kopieren nach { $destination } fehlgeschlagen; es wurde kein Grund festgehalten
backups-restore = Wiederherstellen…
backups-verify = Dateien prüfen
backups-restore-labelled = „{ $snapshot }“ wiederherstellen
backups-restore-from = { $snapshot } von { $when } wiederherstellen

## Was ein Auftrag gerade tut und wann der nächste läuft
backups-job-backing-up = Sicherung läuft
backups-job-verifying = Prüfung läuft
backups-job-restoring = Wiederherstellung läuft
backups-snapshot = Sicherung
backups-schedule-on-demand = Wenn du sicherst
backups-schedule-hourly = Jede Stunde zur vollen Stunde
backups-schedule-hourly-at = Jede Stunde { $minute } Minuten nach der vollen Stunde
backups-schedule-daily = Jeden Tag um { $time }
backups-schedule-weekly = Jeden { $weekday } um { $time }
backups-empty-snapshots-hint = Sichere jetzt, um die erste zu erstellen.
backups-setup-start-hint = Wähle einen Ordner, um von jeder Datei eine Kopie zu behalten, geprüft, damit du weißt, dass sie unversehrt ist.
backups-setup-destination-hint = Wähle ein Laufwerk oder einen Ordner außerhalb dessen, was du sicherst.

## Die Schaltfläche „Jetzt sichern“ in der Fensterleiste
backups-bar-back-up-now = Jetzt sichern
backups-bar-backing-up = Sicherung läuft…

## Wie die Seiten des Fensters heißen
backups-page-all = Alle Sicherungen
backups-page-destination = Ziel

## Actions

backups-operation-backup-now = Jetzt sichern
backups-operation-setup = Sicherungen einrichten
backups-operation-verify = Schnappschuss prüfen
backups-operation-restore-snapshot = Schnappschuss in neuen oder leeren Ordner wiederherstellen …
backups-operation-show-source = Quelle anzeigen
backups-operation-show-destination = Zielordner anzeigen
backups-operation-show-snapshot = Schnappschuss anzeigen
backups-operation-order-snapshots = Schnappschüsse ordnen
backups-operation-read-snapshot = Schnappschuss lesen
backups-operation-show-schedule = Globale Aufbewahrungsrichtlinie anzeigen
backups-workspace = Sicherungen
