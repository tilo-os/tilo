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
