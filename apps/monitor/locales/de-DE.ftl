## Verlauf der geteilten Prozesstabelle

monitor-monitor-sort = Die Prozesse nach { $new ->
    [pid] PID
    [name] Namen
    [user] Benutzer
    [mem] Speicher
    [threads] Threads
   *[cpu] CPU
} sortiert
monitor-monitor-ascending = { $new ->
    [true] Die Prozesse aufsteigend sortiert
   *[false] Die Prozesse absteigend sortiert
}
monitor-monitor-flat = { $new ->
    [true] Die Prozesse flach aufgelistet
   *[false] Den Prozessbaum angezeigt
}

## Fenster- und Barrierefreiheits-Texte
monitor-a11y-processes = Prozesse

## Fakten, Eigenschaftsnamen und Bereichstitel
monitor-command = Befehl
monitor-running-for = Läuft seit
monitor-open-files = Offene Dateien
monitor-virtual-memory = Virtueller Speicher
monitor-threads = Threads
monitor-parent = Übergeordneter Prozess
monitor-app = App
monitor-status = Status
monitor-user = Benutzer
monitor-memory-children = Speicher mit Kindprozessen
monitor-cpu-children = CPU mit Kindprozessen
monitor-processor = Prozessor
monitor-kernel = Kernel
monitor-host-name = Rechnername

monitor-section-system = System
monitor-legend-in-use = In Benutzung: { $percent }
monitor-section-memory = Speicher

## What the window shows
monitor-cpu-total = Gesamt { $percent }
monitor-cpu-core = Kern { $core }
monitor-memory-swap = Auslagerung
monitor-section-network = Netzwerk
monitor-section-disk = Datenträger
monitor-usage-unavailable-message = Die Nutzung von Prozessor, Arbeitsspeicher, Netzwerk und Datenträgern dieses Computers konnte nicht gelesen werden.

## Die Plätze der Fensterleiste
monitor-tab-resources = Ressourcen
monitor-views = Ansicht
monitor-refresh = Aktualisieren
monitor-update-frequency = Aktualisierung: { $cadence }
monitor-tree-view = Baumansicht
monitor-include-children = Unterprozesse einbeziehen
monitor-kernel-threads = Kernel-Threads
monitor-cadence-paused = Pausiert
monitor-cadence-second = Jede Sekunde
monitor-cadence-seconds = Alle { $seconds } Sekunden

## Actions

monitor-effect-show-tab = Register anzeigen
monitor-effect-filter = Prozesse filtern
monitor-effect-inspect = Prozessdetails anzeigen
monitor-effect-copy-pid = PID kopieren
monitor-effect-end-process = Prozess beenden
monitor-effect-kill = Erzwungen beenden
monitor-effect-end-app = App beenden
monitor-processes = Prozesse
monitor-effect-refresh = Prozesse aktualisieren

## What the app is called in a window and in the launcher
monitor-app-title = Systemmonitor
