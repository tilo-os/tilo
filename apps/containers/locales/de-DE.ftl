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

## Status pills, sections, tabs and actions
containers-status-running = Läuft
containers-status-exited = Beendet
containers-status-paused = Angehalten
containers-status-restarting = Startet neu
containers-status-dead = Abgestürzt
containers-nav-images = Images
containers-nav-volumes = Volumes
containers-nav-distrobox = Distrobox
containers-tab-logs = Protokoll
containers-tab-stats = Auslastung
containers-tab-inspect = Prüfen
containers-tab-terminal = Terminal
containers-action-stop = Stoppen
containers-action-restart = Neu starten
containers-action-pause = Anhalten
containers-action-resume = Fortsetzen
containers-action-remove = Entfernen
containers-prompt-name = Name
containers-prompt-tag = Neues Tag
containers-prompt-image = Image

## What a removal leaves behind and what a pull is fetching
containers-remove-container-body = Der Container und die darin vorgenommenen Änderungen werden gelöscht. Seine Volumes bleiben.
containers-remove-image-body = Das Image wird von diesem Computer gelöscht. Du kannst es später erneut herunterladen.
containers-remove-volume-body = Das Volume und die darin gespeicherten Daten werden gelöscht.
containers-used-by = { $count ->
    [0] Von keinem Container verwendet
    [one] Von 1 Container verwendet
   *[other] Von { $count } Containern verwendet
}
containers-downloading = Ebenen werden heruntergeladen
containers-downloading-of = Ebenen werden heruntergeladen: { $done } von { $total }
containers-downloading-so-far = Ebenen werden heruntergeladen: bisher { $done }
containers-empty-containers-hint = Erstelle einen Container aus einem Image, um ihn hier laufen zu lassen.
containers-engine-missing-hint = Installiere Podman oder starte den Benutzerdienst, um Container hier zu verwalten.
containers-none-selected-hint = Wähle einen Container, um seine Protokolle, Nutzung und Einstellungen zu sehen.
containers-empty-images-hint = Lade ein Image, um daraus Container zu erstellen.
containers-empty-volumes-hint = Ein Volume bewahrt die Daten eines Containers auf, wenn der Container entfernt wird.
containers-empty-distroboxes-hint = Erstelle eine Box mit distrobox, um sie hier laufen zu lassen.

## Why an engine action cannot run
containers-engine-offline-message = Aktionen der Container-Engine sind im Demo-Modus nicht verfügbar

## Actions

containers-view-set-section = Bereich anzeigen
containers-view-select-container = Container wählen
containers-view-set-tab = Registerkarte wechseln
containers-view-create = Container anlegen
containers-view-read-container = Container lesen
containers-view-open-logs = Protokoll öffnen
containers-view-start = Starten
containers-view-stop = Stoppen
containers-view-restart = Neu starten
containers-view-pause = Pausieren
containers-view-unpause = Fortsetzen
containers-view-remove-container = Entfernen
containers-view-shell = Shell öffnen
containers-view-pull-image = Image pullen
containers-view-tag-image = Tag zum Image hinzufügen
containers-view-remove-image = Image entfernen
containers-view-ask-to-remove-image = Image nach Rückfrage entfernen
containers-view-remove-volume = Volume entfernen
containers-view-ask-to-remove-volume = Volume nach Rückfrage entfernen
containers-view-enter-distrobox = Distrobox betreten
containers-engine = Container
containers-engine-clear-create = Neuen Container verwerfen
