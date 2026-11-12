## Verlauf des Entwurfs für einen neuen Container

containers-create-draft-name = Den neuen Container benannt
containers-create-draft-image = Das Image des neuen Containers gewählt
containers-create-draft-ports = Die Ports des neuen Containers geändert
containers-create-draft-env = Die Umgebung des neuen Containers geändert
containers-create-draft-volumes = Die Volumes des neuen Containers geändert
containers-create-draft-command = Den Befehl des neuen Containers geändert
containers-clear-create = Den neuen Container verworfen

## Barrierefreiheit
containers-a11y-window = Container

## Fenster- und Barrierefreiheits-Texte
containers-pull-log = Jede Zeile, die Podman geschrieben hat, die älteste zuerst.
containers-image-ready = Das Image ist bereit, Container daraus zu erstellen.
containers-pull-stopped = Gestoppt. Was schon geladen wurde, bleibt im Image-Speicher.
containers-a11y-containers = Container
containers-sample-data = Beispieldaten
containers-a11y-sections = Bereiche

## Fakten, Eigenschaftsnamen und Bereichstitel
containers-section-labels = Labels
containers-section-environment = Umgebung
containers-section-mounts = Einhängungen
containers-host-port = Host-Port { $port }
containers-section-ports = Ports
containers-state = Zustand
containers-created = Erstellt
containers-command = Befehl
containers-action-open = { $name } öffnen
containers-section-details = Details
containers-section-activity = Aktivität
containers-section-usage = Auslastung
containers-images-summary = { $count ->
    [one] Ein Image belegt { $size } auf der Festplatte.
   *[other] { $count } Images belegen { $size } auf der Festplatte.
}
containers-volumes-in-use = { $in_use } von { $total } Volumes sind in Benutzung.

## What the window shows
containers-logs-unavailable = Die Protokollausgabe ist nicht verfügbar: { $detail }
containers-logs-empty = Der Container hat noch nichts ins Protokoll geschrieben.
containers-usage-waiting = Die Auslastung zeigt sich, solange der Container läuft.
containers-usage-unavailable = Die Auslastung ist nicht verfügbar. Podman hat keinen Messwert geliefert.
containers-cpu = CPU
containers-memory = Speicher (RSS)
containers-processes = Prozesse
containers-open-shell = Shell öffnen
containers-start = Starten
containers-stop-download = Den Download stoppen
containers-new = Neuer Container
containers-close-details = Details schließen
containers-demo-unavailable = Distrobox-Aktionen sind im Demo-Modus nicht verfügbar
