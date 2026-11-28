# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-disks = Laufwerke

d-so-tilo-disks-keywords = laufwerke, festplatten, partitionen, formatieren, einhängen, smart, disks, drives

d-nnkcw24e = Laufwerke
    .doc = Die Namen, die eine Person ihren Laufwerken gibt, nach Seriennummer: die des Arbeitsbereichs, damit ein Name dem Laufwerk an jedes Gerät folgt, an das es angeschlossen wird.
    .noun = { $count ->
        [one] disk
       *[other] disks
    }

d-6mf2skfm = Namen

d-pezu5gyc = Laufwerk benennen
    .doc = Der Name eines Laufwerks in den Worten der Person, die es liest, nach seiner Seriennummer.
    .did = Ein Laufwerk benannt
    .w-a-drive-s-name-is-not = Der Name eines Laufwerks ist nicht leer und steht ohne Leerzeichen

d-pezu5gyc-serial = Seriennummer

d-pezu5gyc-name = Name

d-cno6mkpj = Erste Partition einhängen
    .doc = Hängt die erste Partition des Laufwerks ein, die ein Laufwerk ohne eigenes Dateisystem hält.

d-cno6mkpj-drive = Laufwerk

d-gs5s5jw4 = In Dateien öffnen
    .doc = Öffnet einen eingehängten Ordner in Dateien.

d-gs5s5jw4-path = Pfad

d-pts77k2e = Speicherplatznutzung öffnen
    .doc = Öffnet die Speicherplatznutzung, wo der Verbrauch eines Ordners gemessen wird.

d-hagdje6k = Gerät formatieren
    .doc = Löscht das in der Bestätigung genannte Gerät und schreibt das gewählte Dateisystem.

d-hagdje6k-device_name = Gerätename

d-hagdje6k-filesystem = Dateisystem

d-3cndvboj = Größenwörter
    .w-b = { $bytes } B
    .w-kb = { $value } kB
    .w-mb = { $value } MB
    .w-gb = { $value } GB
    .w-tb = { $value } TB

d-mtgfincv = Ratenwörter
    .w-kb-s = { $to_int } kB/s
    .w-mb-s = { $to_int } MB/s
    .w-gb-s = { $to_int } GB/s

d-q7p23wal = Gesundheitswörter
    .w-predicted-to-fail = Voraussichtlicher Ausfall
    .w-healthy = Gesund
    .w-its-health-is-not-reported = Seine Gesundheit wird nicht gemeldet

d-bh4trza3 = Schnittstellenwörter
    .w-nvme = NVMe
    .w-sata = SATA
    .w-usb = USB
    .w-not-reported = Nicht gemeldet

d-oxp2isum = Artwörter
    .w-virtual-block-device = Virtuelles Blockgerät
    .w-nvme-solid-state-drive = NVMe-Solid-State-Laufwerk
    .w-removable-usb-drive = Wechselbares USB-Laufwerk
    .w-hard-disk-drive = Festplattenlaufwerk
    .w-solid-state-drive = Solid-State-Laufwerk
    .w-block-drive = Blockgerät

d-torb2dxz = Einhänge-Wörter
    .w-swap = Auslagerung
    .w-not-mounted = Nicht eingehängt

d-a4llajli = Zweckwörter
    .w-swap-space = Auslagerungsspeicher
    .w-running-system = Laufendes System
    .w-home-folders = Home-Ordner
    .w-starts-this-device = Startet dieses Gerät

d-iwcvc5lh = Systemnutzung
    .w-holds-the-running-system = Enthält das laufende System
    .w-holds-the-home-folders = Enthält die Home-Ordner

d-cprxqi3t = Löschgrund
    .w-formatting-would-remove-its-encryption = Formatieren würde seine Verschlüsselung entfernen

d-uirmn2hc = Dateisystemzeile
    .w-swap-space = Auslagerungsspeicher
    .w-at = { $fstype } unter { $mount }
    .w-mounted-at = Eingehängt unter { $mount }

d-5c5yeqp2 = Freier Platz
    .w-free = { $size_words } frei

d-vgpm5svw = Löschen verliert
    .w-everything-on-it-is-erased-for = Alles darauf wird endgültig gelöscht.
    .w-its-partition-and-every-file-on = Seine Partition und jede Datei darauf werden endgültig gelöscht.
    .w-its-partitions-and-every-file-on = Seine { $count } Partitionen und jede Datei darauf werden endgültig gelöscht.
    .w-the-of-files-on-are-erased = Die { $size_words } an Dateien auf „{ $label }“ werden endgültig gelöscht.
    .w-the-of-files-on-it-are = Die { $size_words } an Dateien darauf werden endgültig gelöscht.
    .w-every-file-on-is-erased-for = Jede Datei auf „{ $label }“ wird endgültig gelöscht.
    .w-every-file-on-it-is-erased = Jede Datei darauf wird endgültig gelöscht.

d-s5r7sgxu = Partitionswörter
    .w-nothing-inside = nichts darin
    .w-no-filesystem = kein Dateisystem

d-cdzfm53h = Verschlüsselung
    .w-locked = { $scheme }, gesperrt

d-zns5ikhm = Laufwerk benennen~present

d-iq7enjwf = Fenster
    .w-disks = Laufwerke
    .w-drives = Drives
    .w-loading = Wird gelesen
    .w-no-drives-on-this-device = Keine Laufwerke auf diesem Gerät
    .w-attach-a-drive-then-rescan = Schließe ein Laufwerk an und lies dann neu.
    .w-read-the-drives-again = Die Laufwerke neu lesen
    .w-drive = Laufwerk
    .w-overview = Übersicht
    .w-health = Gesundheit
    .w-partitions = Partitionen
    .w-no-partitions = Keine Partitionen
    .w-this-drive-holds-no-partition-table = Dieses Laufwerk hat keine Partitionstabelle; öffne es als ein Dateisystem oder lösche es.
    .w-locked = Gesperrt
    .w-open = Öffnen
    .w-closed = Geschlossen
    .w-close = Schließen
    .w-locked-unlock-it-first = Gesperrt; zuerst entsperren
    .w-erase = Löschen…
    .w-of = { $size_words } von { $size_words2 }
    .w-how-it-is-divided = Wie es aufgeteilt ist
    .w-the-whole-drive = Das ganze Laufwerk
    .w-unallocated = Nicht zugeteilt
    .w-unallocated-2 = Nicht zugeordnet
    .w-the-drive-read = Das Laufwerk las { $rate_words }
    .w-random-reads-a-second = { $to_int } wahlfreie Lesezugriffe pro Sekunde
    .w-filesystems = Dateisysteme
    .w-no-filesystems-are-mounted = Keine Dateisysteme sind eingehängt.
    .w-open-at = Geöffnet unter { $mounted }
    .w-open-the-first-partition = Die erste Partition öffnen
    .w-measure = Messen
    .w-eject = Auswerfen
    .w-choose-a-drive = Wähle ein Laufwerk
    .w-pick-a-drive-on-the-left = Wähle links ein Laufwerk, um seine Karte, seine Gesundheit und seine Partitionen zu sehen.
    .w-erase-2 = { $erase_name } löschen?
    .w-erase-3 = Löschen
    .w-cancel = Abbrechen
    .w-it-is-erased-and-a-new = Es wird gelöscht und ein neues Dateisystem { $filesystem } darauf geschrieben.

d-4gjpbhve = Laufwerksdetails
    .w-details = Details
    .w-device = Gerät
    .w-dev = /dev/{ $name }
    .w-type = Typ
    .w-capacity = Kapazität
    .w-interface = Schnittstelle
    .w-serial-number = Seriennummer
    .w-filesystem = Dateisystem
    .w-mounted-at = Eingehängt unter

d-tffcn2l7 = Partitionsdetails
    .w-details = Details
    .w-device = Gerät
    .w-dev = /dev/{ $name }
    .w-size = Größe
    .w-type = Typ
    .w-filesystem = Dateisystem
    .w-encryption = Verschlüsselung
    .w-unlocked = { $scheme }, entsperrt
    .w-locked = { $scheme }, gesperrt
    .w-mounted-at = Eingehängt unter
    .w-used = Belegt
    .w-free = Frei
    .w-used-for = Verwendet für

d-34x2kxr2 = Freier Bereich

d-36baoxwj = Wege hinein
    .w-open-in-files = In Dateien öffnen
    .w-disk-usage = Speicherplatznutzung

d-ta2u27ch = Gesundheitsbereich
    .w-health = Gesundheit
    .w-verdict = Urteil
    .w-warnings = Warnungen
    .w-temperature = Temperatur
    .w-c = { $temperature } °C
    .w-powered-on = Eingeschaltet
    .w-hours = { $hours } Stunden
    .w-power-cycles = Einschaltvorgänge
    .w-life-used = Verbrauchte Lebensdauer
    .w-spare-capacity = Reservekapazität
    .w-written = Geschrieben
    .w-read = Gelesen
    .w-media-errors = Medienfehler
    .w-bad-sectors = Defekte Sektoren
    .w-last-self-test = Letzter Selbsttest
    .w-its-health-is-not-reported = Seine Gesundheit wird nicht gemeldet
    .w-this-drive-reports-no-health-of = Dieses Laufwerk meldet keine eigene Gesundheit.
