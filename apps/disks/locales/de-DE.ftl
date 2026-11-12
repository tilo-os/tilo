## Verlauf

disks-name-drive = Ein Laufwerk „{ $name }“ benannt
disks-disks-names = Den Namen eines Laufwerks geändert

disks-title = Laufwerke
disks-partition-map = Partitionsübersicht
disks-loading = Wird geladen
disks-select-whole-disk = Gesamte Platte auswählen

## Fenster- und Barrierefreiheits-Texte
disks-drive-empty = Dieses Laufwerk hat keine Partitionen.
disks-a11y-smart = SMART-Attribute
disks-none-mounted = Es sind keine Dateisysteme eingehängt.
disks-a11y-drives = Laufwerke

## Fakten, Eigenschaftsnamen und Bereichstitel
disks-section-details = Details
disks-used-for = Verwendet für
disks-free = Frei
disks-used = Belegt
disks-mounted-at = Eingehängt unter
disks-encryption = Verschlüsselung
disks-filesystem = Dateisystem
disks-type = Typ
disks-size = Größe
disks-device = Gerät
disks-serial = Seriennummer
disks-interface = Schnittstelle
disks-capacity = Kapazität
disks-section-filesystems = Dateisysteme
disks-partition-a11y = Partition { $device } auswählen

disks-section-attributes = Attribute
disks-section-drive = Laufwerk
disks-section-partition-map = Partitionskarte

## What the window shows
disks-open-in-files = In Dateien öffnen
disks-space-left = Wie viel Platz ist noch frei?
disks-swap = Auslagerungsspeicher
disks-filesystem-at = { $filesystem } unter { $path }
disks-in-use = In Benutzung
disks-not-in-use = Nicht in Benutzung
disks-purpose-root = Laufendes System
disks-purpose-home = Home-Ordner
disks-purpose-boot = Startet dieses Gerät
disks-partition-encrypted = verschlüsselt
disks-partition-encrypted-locked = verschlüsselt und gesperrt
disks-partition-free = { $room } frei
disks-not-mounted = Nicht eingehängt
disks-encryption-unlocked = { $scheme }, entsperrt
disks-encryption-locked = { $scheme }, gesperrt

disks-severity-warning = Warnung
disks-severity-critical = Kritisch
disks-severity-healthy = OK
disks-empty-drives-hint = Schließe ein Laufwerk an und suche dann erneut.

## Was das Formatieren eines Laufwerks oder einer Partition löscht, in der Frage davor
disks-format-loses-drive = { $count ->
    [0] Alles darauf wird endgültig gelöscht.
    [one] Die Partition und alle Dateien darauf werden endgültig gelöscht.
   *[other] Die { $count } Partitionen und alle Dateien darauf werden endgültig gelöscht.
}
disks-format-loses-files = Alle Dateien darauf werden endgültig gelöscht.
disks-format-loses-files-on = Alle Dateien auf „{ $label }“ werden endgültig gelöscht.
disks-format-loses-used = Die { $size } an Dateien darauf werden endgültig gelöscht.
disks-format-loses-used-on = Die { $size } an Dateien auf „{ $label }“ werden endgültig gelöscht.
## Die Plätze der Fensterleiste
disks-rescan-drives = Laufwerke erneut suchen
disks-views = Ansicht


## Wie die Operationen eines Laufwerks heißen und was SMART darüber sagt
disks-action-mount = Einhängen
disks-action-unmount = Aushängen
disks-action-format = Formatieren…
disks-action-benchmark = Benchmark
disks-action-eject = Auswerfen
disks-health-healthy = In Ordnung
disks-health-attention = Braucht Aufmerksamkeit
disks-health-failing = Fehlerhaft
disks-health-unknown = Unbekannt

## Actions

disks-device-rescan = Neu einlesen
disks-device-select = Auswählen
disks-device-show-tab = Register anzeigen
disks-device-format = Format
disks-device-disk-usage = Speicherbelegung anzeigen
disks-system = Speichergeräte
disks-system-name-drive = Laufwerk benennen
