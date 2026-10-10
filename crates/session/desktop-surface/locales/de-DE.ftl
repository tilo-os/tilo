# Obere Leiste
continuity-dismiss = Ausblenden
topbar-workspace-fallback = Arbeitsbereich
topbar-calendar-none = Kein Kalender verbunden
topbar-sharing-agent-controls = AGENT STEUERT · { $name }
topbar-sharing-agent-viewing = AGENT SIEHT ZU · { $name }
topbar-sharing-person-controls = STEUERT · { $name }
topbar-sharing-person-viewing = SIEHT ZU · { $name }
topbar-sharing-session-controls = SITZUNG STEUERT · { $name }
topbar-sharing-session-viewing = SITZUNG SIEHT ZU · { $name }
topbar-sharing-count-controlling = { $controllers } { $controllers ->
        [one] STEUERT
       *[other] STEUERN
    } · { $viewers } { $viewers ->
        [one] SIEHT ZU
       *[other] SEHEN ZU
    }
topbar-sharing-count = { $viewers } { $viewers ->
        [one] SIEHT ZU
       *[other] SEHEN ZU
    }
topbar-sharing-surface = Oberfläche { $surface }
topbar-open-overview = Übersicht öffnen
topbar-previous-workspace = Vorheriger Arbeitsbereich
topbar-next-workspace = Nächster Arbeitsbereich
topbar-workspace-settings = Einstellungen für Arbeitsbereich { $workspace }
topbar-show-clock = Uhr anzeigen
topbar-show-system-monitor = Systemmonitor anzeigen
topbar-show-calendar = Kalender anzeigen
topbar-open-quick-settings = Schnelleinstellungen öffnen
topbar-show-collaborators = Mitwirkende anzeigen
topbar-viewer-can-control = kann { $targets } steuern
topbar-viewer-view-only = sieht nur { $targets }
topbar-viewer-entry = { $name } ({ $kind ->
        [agent] Agent
        [person] Person
       *[session] unbekannte Sitzung
    }): { $access }
topbar-collaborators-visible = Bildschirm sichtbar für { $viewers }. Mitwirkende anzeigen
topbar-capture-privacy = Datenschutzeinstellungen für aktive Aufnahme
topbar-control = Steuerelement der oberen Leiste
topbar-editor-control = Bearbeitungselement der oberen Leiste
topbar-editor-remove-clock = Uhr aus der oberen Leiste entfernen
topbar-editor-remove-system-monitor = Systemmonitor aus der oberen Leiste entfernen
topbar-editor-remove-calendar = Kalender aus der oberen Leiste entfernen
topbar-editor-add-clock = Uhr zur oberen Leiste hinzufügen
topbar-editor-add-system-monitor = Systemmonitor zur oberen Leiste hinzufügen
topbar-editor-add-calendar = Kalender zur oberen Leiste hinzufügen
topbar-editor-title = Obere Leiste bearbeiten
topbar-editor-hint = Zum Anordnen ziehen · „Entfernen“ klicken
topbar-widget-clock = Uhr
topbar-widget-system-monitor = Systemmonitor
topbar-widget-calendar = Kalender
topbar-editor-remove = Entfernen
topbar-editor-add-clock-chip = + Uhr
topbar-editor-add-monitor-chip = + Monitor
topbar-editor-add-calendar-chip = + Kalender
topbar-invite = Einladen
topbar-activity-unread = { $headline }  ·  { $count } neu
topbar-clock-resume = Fortsetzen
topbar-clock-pause = Pausieren
topbar-clock-lap = Runde
topbar-clock-add-minute = +1 Min.
topbar-clock-close-stopwatch = Schwebende Stoppuhr schließen
topbar-clock-close-timer = Schwebenden Timer schließen
topbar-clock-floating-stopwatch = Schwebende Stoppuhr
topbar-clock-floating-timer = Schwebender Timer
topbar-clock-floating-description = Zum Verschieben ziehen. Mit der Tastatur Alt gedrückt halten und die Pfeiltasten verwenden; zusätzlich Umschalt für größere Schritte.
topbar-clock-stopwatch = Stoppuhr
topbar-clock-timer = Timer
topbar-clock-paused = Pausiert
topbar-clock-running = Läuft
topbar-clock-record-lap = Runde erfassen
topbar-clock-add-one-minute = Eine Minute hinzufügen
topbar-workspace-position = Arbeitsbereich { $position } von { $total }
topbar-desktop-navigation = Desktop-Navigation
topbar-display-navigation = Bildschirmnavigation

# Arbeitsbereichsaktivität
a11y-shell-workspace-activity = Aktivität im Arbeitsbereich
a11y-shell-activity-everyone = Alle
a11y-shell-activity-more-people = Weitere Personen
a11y-shell-activity-more-people-description = Weitere Filter für Mitwirkende anzeigen
a11y-shell-activity-resume = { $target } fortsetzen
a11y-shell-activity-resume-description = Die zuletzt bearbeitete Ressource öffnen: { $target }
a11y-shell-activity-boundary-newer = Die letzte Aktion in die neuere Arbeitssitzung verschieben
a11y-shell-activity-boundary-older = Die erste Aktion in die ältere Arbeitssitzung verschieben
a11y-shell-activity-boundary-newer-description = Diese Grenze neu ziehen, indem eine Aktion in die neuere Arbeitssitzung wandert
a11y-shell-activity-boundary-older-description = Diese Grenze neu ziehen, indem eine Aktion in die ältere Arbeitssitzung wandert
a11y-shell-activity-more-sessions = Weitere Arbeitssitzungen
a11y-shell-activity-more-sessions-description = Weitere Arbeitssitzungen anzeigen
a11y-shell-activity-new-task = Neue Aufgabe
a11y-shell-activity-new-task-description = Eine Aufgabe im Arbeitsbereich erstellen, ohne ein Modell zu benötigen
a11y-shell-activity-create-task = Aufgabe erstellen
a11y-shell-activity-cancel-task = Erstellen der Aufgabe abbrechen
a11y-shell-activity-everything = Alles
a11y-shell-activity-agent-tasks = Agent-Aufgaben
a11y-shell-activity-unclassified = Nicht zugeordnet
a11y-shell-activity-more-lenses = Weitere Ansichten
a11y-shell-activity-more-lenses-description = Weitere Verlaufsansichten anzeigen
a11y-shell-activity-history-entries = Verlaufseinträge
a11y-shell-activity-previous-checkpoint = Vorheriger Zwischenstand
a11y-shell-activity-next-checkpoint = Nächster Zwischenstand
a11y-shell-activity-pause-replay = Wiedergabe pausieren
a11y-shell-activity-play-replay = Wiedergabe abspielen
a11y-shell-activity-history-scope = Verlaufsumfang
a11y-shell-activity-history-view = Verlaufsansicht
a11y-shell-activity-time-range = Zeitraum
a11y-shell-activity-person-filter = Personenfilter
a11y-shell-activity-brief-description = { $detail }; zuletzt geändert { $when }; { $count } { $count ->
        [one] Änderung
       *[other] Änderungen
    } von { $authors }
a11y-shell-activity-task-unassigned = nicht zugewiesen
a11y-shell-activity-task-assigned-agent = einem Agenten zugewiesen
a11y-shell-activity-task-no-resource = keine angehängte Ressource
a11y-shell-activity-task-route = Ausführungsweg der Aufgabe
a11y-shell-activity-task-composer-action = Aktion zum Erstellen der Aufgabe
a11y-shell-activity-task-action = Aktion für die ausgewählte Aufgabe
a11y-shell-activity-history-lens = Verlaufsansicht
a11y-shell-activity-undo-task-description = Jeden betroffenen Eintrag dieser Agent-Aufgabe rückgängig machen; spätere Änderungen anderer bleiben erhalten
a11y-shell-activity-history-action = Aktion für den ausgewählten Verlauf
a11y-shell-activity-explorer-panel = Explorer-Bereich
a11y-shell-activity-timeline-description = Verlaufseinträge; mit den Pfeiltasten navigieren
a11y-shell-activity-checkpoints-description = Durch die Zwischenstände des Arbeitsbereichs gehen
a11y-shell-activity-replay-description = Die Wiedergabe des Arbeitsbereichs abspielen oder pausieren
a11y-shell-activity-view-catch-up = Eine Zusammenfassung der Ergebnisse, seit dieses Gerät den Arbeitsbereich zuletzt angesehen hat
a11y-shell-activity-view-tasks = Dauerhafte Aufgaben im Arbeitsbereich mit Ressourcenlinks, Ausführungswegen und Prüfprotokollen
a11y-shell-activity-view-explore = Der vollständige Verlauf des Arbeitsbereichs mit Filtern und Herkunft
a11y-shell-activity-view-replay = Eine kausale Wiedergabe der Anordnung und der Aktivität der Beteiligten
a11y-shell-activity-entry-author = Von { $author }
a11y-shell-activity-tasks = Aufgaben im Arbeitsbereich
a11y-shell-activity-selected-detail = Details zum ausgewählten Verlauf
a11y-shell-activity-selected-detail-description = { $title }; { $count } { $count ->
        [one] Aktion
       *[other] Aktionen
    }; von { $authors }
a11y-shell-activity-agent-run-description = Anweisung: { $instruction }; zugewiesen von { $issuer }; ausgeführt von { $agent }; Begründung: { $reason }
a11y-shell-activity-reason-not-recorded = nicht erfasst
a11y-shell-activity-agent-step = { $app } in { $record }: { $summary }
a11y-shell-activity-agent-review = Prüfung der Agent-Aufgabe
a11y-shell-activity-task-description = { $status }; { $route }; { $assignee }; { $count } { $count ->
        [one] Prüfereignis
       *[other] Prüfereignisse
    }
a11y-shell-activity-task-latest = zuletzt: { $latest }
a11y-shell-activity-task-instruction = Anweisung: { $instruction }
activity-task-instruction = Anweisung
activity-task-read-instruction = Vollständige Anweisung lesen
activity-task-full-instruction = Vollständige Anweisung
activity-task-close-instruction = Zurück zur Aufgabe
a11y-shell-activity-task-close-instruction = Zur ausgewählten Aufgabe zurückkehren
activity-view-catch-up = Aufholen
activity-view-tasks = Aufgaben
activity-view-explore = Erkunden
activity-view-replay = Wiedergabe
activity-title-catch-up = Während du weg warst
activity-title-tasks = Aufgabenzentrale
activity-title-explore = Arbeitsbereich-Aktivität
activity-title-replay = Arbeitsbereich wiedergeben
activity-workspace-fallback = Dieser Arbeitsbereich
activity-source-workspace = Arbeitsbereich
activity-scope-this-workspace = Dieser Arbeitsbereich
activity-scope-all-workspaces = Alle Arbeitsbereiche
activity-scope-workspace-short = Arbeitsbereich
activity-scope-all-short = Alle
activity-scope-here = Hier
activity-period-all-time = Gesamter Zeitraum
activity-period-today = Heute
activity-period-week = 7 Tage
activity-period-month = 30 Tage
activity-people-everyone = Alle

activity-filter-time = Zeit
activity-filter-people = Personen
activity-filter-apps = Apps
activity-filter-kind = Art
activity-filter-all = Alle
activity-detail-restore = Wiederherstellen
activity-history-compare = Diese Änderung vergleichen
activity-history-undo = Diese Änderung rückgängig machen
activity-history-restore = Diese Version wiederherstellen
activity-first-people = Erste Personen…
activity-more = Mehr…
activity-pane-timeline = Zeitleiste
activity-pane-browse = Durchsuchen
activity-pane-details = Details
activity-resume-heading = ZULETZT BEARBEITET
activity-resume-context = { $app } · { $when }
activity-resume-button = Fortsetzen →
activity-pending-edits = Warte auf laufende Bearbeitungen
activity-catch-up-quiet = { $workspace } · Nichts Neues braucht deine Aufmerksamkeit
activity-catch-up-one = { $workspace } · { $name } hat die Arbeit vorangebracht
activity-catch-up-two = { $workspace } · { $first } und { $second } haben die Arbeit vorangebracht
activity-catch-up-many = { $workspace } · { $first }, { $second } und { $others } { $others ->
        [one] weitere Person waren
       *[other] weitere Personen waren
    } aktiv
activity-changes-workspace = { $workspace } · { $changes } { $changes ->
        [one] Änderung
       *[other] Änderungen
    } von { $people } { $people ->
        [one] Person
       *[other] Personen
    }
activity-changes-everywhere = Alle Arbeitsbereiche auf diesem Gerät · { $changes } { $changes ->
        [one] Änderung
       *[other] Änderungen
    } von { $people } { $people ->
        [one] Person
       *[other] Personen
    }
activity-coverage-excluded = Verlauf nicht verfügbar für { $apps }
activity-coverage-missing = { $count } { $count ->
        [one] Eintrag
       *[other] Einträge
    } nicht verfügbar
activity-coverage-capped = { $count } { $count ->
        [one] weiterer Eintrag
       *[other] weitere Einträge
    } abgeschnitten
activity-replay-checkpoints-unavailable = { $count } { $count ->
        [one] Prüfpunkt
       *[other] Prüfpunkte
    } nicht verfügbar
activity-latest = Zuletzt · { $headline }
activity-history-pending = Der Verlauf wartet auf laufende Bearbeitungen
activity-caught-up = Du bist auf dem neuesten Stand
activity-summary-on-device = Zusammenfassung auf dem Gerät
activity-session-move-newer = ↑  1 Aktion
activity-session-move-older = 1 Aktion  ↓
activity-more-sessions = Weitere Arbeitssitzungen…
activity-first-sessions = Erste Arbeitssitzungen…
activity-brief-underlying = { $count } { $count ->
        [one] zugrunde liegende Aktion
       *[other] zugrunde liegende Aktionen
    }
activity-brief-open-explorer = Diese Arbeitssitzung im Explorer öffnen →
activity-brief-show-affected = Betroffene Ressourcen und Aktionen anzeigen
activity-browse-by = Durchsuchen nach
activity-more-lenses = Weitere Ansichten…
activity-first-lenses = Erste Ansichten…
activity-lens-everything = Alles
activity-lens-agent-tasks = Agent-Aufgaben  { $count }
activity-lens-unclassified = Nicht zugeordnet  { $count }
activity-provenance = Herkunft
activity-select-prompt = Wähle eine Arbeitssitzung oder einen Bereich von Aktionen
activity-select-hint = Sieh, wer beteiligt war, welche Ressourcen betroffen sind, und vergleiche genau vorher und nachher.
activity-detail-open = Öffnen
activity-detail-open-app = App öffnen
activity-detail-compare = Vergleichen
activity-detail-undo = Rückgängig
activity-detail-undo-task = Ganze Aufgabe rückgängig
activity-detail-split = Teilen
activity-detail-merge = Zusammenführen
activity-detail-action-count = { $count } { $count ->
        [one] Aktion
       *[other] Aktionen
    }
activity-no-resource = Keine Ressource angegeben
activity-what-changed = Was sich geändert hat
activity-comparison-counts = { $actions } { $actions ->
        [one] Aktion
       *[other] Aktionen
    } · { $changes } { $changes ->
        [one] Dokumentänderung
       *[other] Dokumentänderungen
    }
activity-no-differences = Keine inhaltlichen Unterschiede in dieser Auswahl
activity-comparison-more = +{ $count } weitere
activity-preview-unavailable = Vorschau nicht verfügbar
activity-preview-unavailable-detail = Die Herkunft dieser Aktion ist bekannt, aber es gibt keinen gespeicherten Vorher-nachher-Stand.
activity-change-resolved-field = Gleichzeitige Änderungen an { $field } aufgelöst
activity-change-updated-field = { $field } aktualisiert
activity-change-removed-field = { $field } entfernt
activity-change-resolved-list = Gleichzeitige Listenänderungen aufgelöst
activity-change-updated-list-item = Listeneintrag aktualisiert
activity-change-added-items = { $count } { $count ->
        [one] Eintrag
       *[other] Einträge
    } hinzugefügt
activity-change-removed-items = { $count } { $count ->
        [one] Eintrag
       *[other] Einträge
    } entfernt
activity-change-edited-text = Text bearbeitet
activity-change-a-counter = einen Zähler
activity-change-decreased = { $field } verringert
activity-change-increased = { $field } erhöht
activity-change-concurrent-field = Gleichzeitige Änderungen an { $field }
activity-change-concurrent-list = Gleichzeitige Listenänderungen
activity-change-formatting = Textformatierung geändert
activity-change-content = Inhalt
activity-agent-task = Agent-Aufgabe
activity-agent-task-people = Zugewiesen von { $issuer }  ·  Ausgeführt von { $agent }
activity-agent-fallback = Agent
activity-unknown-collaborator = Unbekannte Person
activity-why-this-work = Warum diese Arbeit
activity-no-decision = Es wurde kein Entscheidungsweg aufgezeichnet.
activity-affected-records = Betroffene Einträge
activity-undo-task-needs-open = Öffne alle betroffenen Dokumente, um diese Aufgabe gemeinsam rückgängig zu machen.
activity-undo-task-needs-documents = Nichts geändert. Öffne zuerst alle betroffenen Dokumente.
activity-undo-task-partial = Aufgabe teilweise rückgängig gemacht; spätere Bearbeitungen anderer wurden behalten.
activity-undo-task-done = Ganze Aufgabe als neuer Verlaufseintrag rückgängig gemacht.
activity-undo-task-gone = Die Auswirkungen der Aufgabe waren bereits entfernt.
activity-removed-workspace = ein entfernter Arbeitsbereich
activity-receipt-completed = Abgeschlossen
activity-receipt-failed = Fehlgeschlagen
activity-receipt-cancelled = Abgebrochen
activity-replay-pending = Die Wiedergabe wartet auf laufende Bearbeitungen
activity-replay-unavailable = Für diese Prüfpunkte ist keine Wiedergabe des Arbeitsbereichs verfügbar
activity-replay-empty = Noch keine wiedergebbaren Prüfpunkte
activity-replay-pause = Pause
activity-replay-play = Abspielen
activity-replay-position = Prüfpunkt { $position } von { $total } · { $when }
activity-replay-no-workspace = Der Arbeitsbereich existierte zu diesem Prüfpunkt nicht
activity-replay-empty-window = Leeres Fenster
activity-replay-layout-not-recorded = Fenstergrößen und ausgewählte Tabs wurden nicht aufgezeichnet
activity-presence-trail = Anwesenheitsverlauf
activity-tasks-subtitle = { $workspace } · { $active } { $active ->
        [one] aktive Aufgabe
       *[other] aktive Aufgaben
    } · { $agents } { $agents ->
        [one] zugewiesener Agent
       *[other] zugewiesene Agents
    }
activity-tasks-tagline = Native Arbeitsbereich-Aufgaben · Modell optional · jede Aktion prüfbar
activity-tasks-pending = Aufgaben warten auf laufende Bearbeitungen
activity-tasks-empty = Noch keine Aufgaben
activity-tasks-empty-hint = Lege eine manuell an oder hänge die in „Erkunden“ ausgewählte Ressource an.
activity-task-new = + Neue Aufgabe
activity-task-queue = WARTESCHLANGE DES ARBEITSBEREICHS
activity-task-composer-title = Arbeit anlegen, ohne den Schreibtisch zu verlassen
activity-task-placeholder = Was muss erledigt werden?
activity-task-executor-manual = Ausführung · Du oder eine andere Person · kein Modell
activity-task-executor-none = Ausführung · Kein Agent verfügbar · Aufgabe bleibt unzugewiesen
activity-task-executor-agent = Ausführung · Agent · { $agent }
activity-task-no-attachment = Keine Ressource angehängt · Aufgabe gilt für den ganzen Arbeitsbereich
activity-task-attached = Angehängt · { $app } · { $resource }
activity-task-attached-workspace = Arbeitsbereich
activity-task-cancel-compose = Abbrechen
activity-task-create = Anlegen
activity-task-open-resource = Ressource öffnen
activity-task-start-agent = Agent starten
activity-task-launch-agent = Agent ausführen
activity-task-mark-started = Als begonnen markieren
activity-task-mark-done = Als erledigt markieren
activity-task-cancel = Aufgabe abbrechen
activity-task-retry-agent = Agent erneut versuchen
activity-task-audit-trail = PRÜFPROTOKOLL
activity-task-unassigned-by = Nicht zugewiesen · angelegt von { $creator }
activity-task-agent-badge = Agent · { $name }
activity-task-status-queued = In der Warteschlange
activity-task-status-in-progress = In Arbeit
activity-task-status-waiting = Wartet
activity-task-status-done = Erledigt
activity-task-status-cancelled = Abgebrochen
activity-task-route-manual = Manuell · kein Modell
activity-task-route-rules-only = Agent · nur Regeln
activity-task-route-model = Agent · mit Modell
activity-task-notice-no-agent = Kein aktivierter Agent ist zugewiesen · diese Aufgabe hat nicht begonnen
activity-task-notice-ready = Agent verbunden · bereit für diese Aufgabe
activity-task-notice-starting = Verwalteter Agent startet · wartet auf sein Broker-Abonnement
activity-task-notice-offline = Agent offline · { $detail }
activity-task-notice-no-runtime = Agent-Laufzeit nicht verbunden · diese Aufgabe hat nicht begonnen
activity-agent-connect-timeout = Der Agent hat sich nicht mit dem Arbeitsbereich verbunden. Versuche es erneut, um eine neue verwaltete Sitzung zu starten.
activity-agent-stopped = Der verwaltete Agent wurde vor dem Verbinden beendet.
activity-agent-exited = Der Starter des verwalteten Agents wurde mit Status { $code } beendet.
activity-agent-dispatch-failed = Aufgabe konnte nicht an den Agent übergeben werden
activity-agent-dispatch-unavailable = Übergabe an Agents ist nicht verfügbar
activity-agent-launch-unavailable = Agents können nicht gestartet werden

# Ereignisse im Arbeitsbereich-Verlauf
activity-event-removed-workspaces = { $count } { $count ->
        [one] Arbeitsbereich
       *[other] Arbeitsbereiche
    } entfernt
activity-event-recorded-workspaces = { $count } { $count ->
        [one] Arbeitsbereich
       *[other] Arbeitsbereiche
    } erfasst
activity-event-recorded-workspace = Arbeitsbereich „{ $name }“ erfasst
activity-event-recorded-named-workspaces = { $count } Arbeitsbereiche erfasst: { $names }
activity-event-granted = { $capability } gewährt
activity-event-granted-capabilities = { $count } { $count ->
        [one] Berechtigung
       *[other] Berechtigungen
    } gewährt
activity-event-revoked-capabilities = { $count ->
        [one] Eine Berechtigung entzogen
       *[other] { $count } Berechtigungen entzogen
    }
activity-event-created = Arbeitsbereich erstellt
activity-event-created-named = Arbeitsbereich „{ $name }“ erstellt
activity-event-updated = Arbeitsbereich aktualisiert
activity-event-owner = Eigentümer des Arbeitsbereichs festgelegt
activity-event-identity = Identität des Arbeitsbereichs zugewiesen
activity-event-renamed = Arbeitsbereich umbenannt
activity-event-named = Arbeitsbereich „{ $name }“ benannt
activity-event-opened-windows = { $count } { $count ->
        [one] Fenster
       *[other] Fenster
    } geöffnet
activity-event-opened = { $titles } geöffnet
activity-event-closed-windows = { $count } { $count ->
        [one] Fenster
       *[other] Fenster
    } geschlossen
activity-event-rearranged = Leiste neu angeordnet
activity-event-resized = Fenstergruppe in der Größe geändert
activity-event-switched = Arbeitsbereich gewechselt
activity-titles-two = { $first } und { $second }
activity-titles-more = { $first }, { $second } und { $count } weitere
activity-capability = { $effect ->
        [restrict] eingeschränktes { $verb ->
            [read] Lesen
            [comment] Kommentieren
            [write] Schreiben
            [invite] Einladen
           *[admin] Verwalten
        }
       *[grant] { $verb ->
            [read] Lesen
            [comment] Kommentieren
            [write] Schreiben
            [invite] Einladen
           *[admin] Verwalten
        }
    } für { $scope } an { $subject }
activity-capability-unknown = eine Berechtigung
activity-capability-this-workspace = diesen Arbeitsbereich
activity-capability-every-type = jede Ressource vom Typ { $kind }
activity-capability-action-on-kind = { $app }:{ $action } auf jeder Ressource vom Typ { $kind }
activity-capability-one-artifact = ein Artefakt
activity-capability-one-shared-file = eine geteilte Datei
activity-capability-network = Netzwerk { $host }
activity-capability-role = { $role ->
        [owner] Eigentümer
        [editor] Bearbeitende
        [commenter] Kommentierende
        [viewer] Betrachtende
       *[denied] gesperrte Mitglieder
    }
activity-capability-a-person = eine Person
activity-capability-agent = Agent { $agent }
activity-capability-invite-token = ein Einladungstoken

# Schreibtischflächen
desktop-system-status = Systemstatus
desktop-app-unavailable = Anwendung nicht verfügbar
desktop-app-identity-unavailable = Anwendungsidentität nicht verfügbar
desktop-app-integration-unavailable = { $app }: Anwendungsintegration oder benötigter Dienst nicht verfügbar
desktop-app-no-adapter = Kein semantischer Anwendungsadapter; prozesslokale Pixel lassen sich nicht wiederherstellen
desktop-view-switch-workspace = Zu { $workspace } wechseln
desktop-view-review-activity = Arbeitsbereich-Aktivität ansehen
desktop-view-explore-activity = Arbeitsbereich-Aktivität erkunden
desktop-view-replay-activity = Arbeitsbereich-Aktivität wiedergeben

## Desktop window search
search-provider-desktop-windows = Fenster auf dem Desktop
search-badge-tab = Tab
search-badge-window-group = Fenstergruppe
search-window-move-tab = Tab „{ $tab }“ in den Arbeitsbereich „{ $workspace }“ verschieben
search-window-move-tab-detail = Aus dem Arbeitsbereich „{ $workspace }“
search-window-copy-tab = Tab „{ $tab }“ in den Arbeitsbereich „{ $workspace }“ kopieren
search-window-group-untitled = Unbenanntes Fenster
search-window-close-group = Fenstergruppe „{ $group }“ schließen
search-window-close-group-detail = Arbeitsbereich „{ $workspace }“ · Leiste „{ $strip }“ · { $count ->
    [one] 1 Tab
   *[other] { $count } Tabs
}
search-window-close-tab = Tab „{ $tab }“ schließen
search-window-close-tab-detail = Arbeitsbereich „{ $workspace }“ · Leiste „{ $strip }“
search-window-focus-place = Geöffnet in diesem Arbeitsbereich
search-window-focus-place-strip = Geöffnet in der Leiste „{ $strip }“
search-window-focus-detail-shared = { $place } · { $count ->
    [one] 1 Tab
   *[other] { $count } Tabs
}
# Wörter, die nach Schließen- bzw. Verschieben/Kopieren-Zeilen fragen;
# Englisch bleibt als Rückfall erhalten. Leerzeichengetrennt, klein.
search-window-verb-close = schließen beenden close quit
search-window-verb-relocate = verschieben kopieren senden move copy send

# Notifications: banner stack and notification centre
drill-in-back = Zurück
a11y-notifications-none = Keine aufbewahrten Benachrichtigungen.
a11y-notifications-description = Neueste Zeilen { $first } bis { $last } von { $total }, nach App gruppiert. Mit Pfeil nach oben und unten wählst du eine Benachrichtigung, mit Bild auf und Bild ab springst du um eine Seite, mit Pos1 oder Ende zur neuesten oder ältesten aufbewahrten Benachrichtigung. Eingabe öffnet die gewählte Benachrichtigung oder klappt ihre App-Gruppe ein, Entf verwirft sie.
a11y-notifications-list = Aufbewahrte Benachrichtigungen
a11y-notifications-clear = Alle Benachrichtigungen löschen
a11y-notifications-clear-description = Löscht den aufbewahrten Benachrichtigungsverlauf dieser Sitzung. Acht Sekunden lang kannst du das rückgängig machen.
a11y-notifications-cleared = Benachrichtigungen gelöscht
a11y-notifications-undo = Löschen der Benachrichtigungen rückgängig machen
a11y-notifications-undo-description = Stellt die gelöschten Benachrichtigungen in ihrer ursprünglichen Reihenfolge wieder her. Drücke Eingabe oder Leertaste.
a11y-notifications = Benachrichtigungen
notification-action-open = Öffnen
notification-center-title = Benachrichtigungen
notification-center-empty = Keine neuen Benachrichtigungen
notification-center-cleared = Benachrichtigungen gelöscht
notification-center-clear-all = Alle löschen
notification-center-position = Neueste { $first }–{ $last } von { $total }
notification-center-count = { $count ->
        [one] { $count } Benachrichtigung
       *[other] { $count } Benachrichtigungen
    }
notification-center-newer = ↑ Neuere
notification-center-at-newest = Neueste erreicht
notification-center-older = ↓ Ältere
notification-center-at-oldest = Älteste erreicht
history-severity-info = Information
history-severity-warning = Warnung
history-severity-error = Fehler
history-source-system = System
history-notification-description = { $severity }. Von { $source }. { $time }
history-notification-description-body = { $severity }. Von { $source }. { $time }. { $body }
notification-center-group-description = { $count } Benachrichtigungen. Eingabe zum { $expanded ->
        [yes] Einklappen
       *[no] Ausklappen
    }, Entf, um alle zu verwerfen.
notification-center-dismiss-all = Alle von { $app } verwerfen
notification-center-dismiss = Verwerfen
notification-center-hand-to = An { $agent } übergeben
notification-center-hand-to-description = Übergibt diese Benachrichtigung als Aufgabe an den Agenten des Arbeitsbereichs.
notification-center-group-count = { $count ->
        [one] { $count } Benachrichtigung
       *[other] { $count } Benachrichtigungen
    }
notification-lock-count = { $count ->
    [one] 1 Benachrichtigung
   *[other] { $count } Benachrichtigungen
}
notification-dnd-held = { $status } · { $count } zurückgehalten
notification-stack-a11y = Benachrichtigungen
notification-group-system = System
notification-center-undo = Rückgängig
notification-app-failed-title = Eine App konnte nicht geöffnet werden
notification-service-unavailable = Das ist auf diesem Desktop noch nicht verfügbar

# Workspace settings
workspace-settings-roles-resource = Berechtigungsrollen von { $workspace }
workspace-settings-role-save = Berechtigungsrolle erstellen oder aktualisieren
workspace-settings-role-save-placeholder = Rollenname
workspace-settings-role-save-description = Speichere eine wiederverwendbare Rolle mit minimalen Rechten aus App-, Aktions-, Datei-, Ressourcen- oder Arbeitsbereichsregeln.
workspace-settings-role-assign = Berechtigungsrolle zuweisen
workspace-settings-role-assign-placeholder = Rolle und Mitwirkende
workspace-settings-role-assign-description = Weise einer Person, Anwendung oder einem Agenten eine gespeicherte Rolle mit minimalen Rechten zu.
workspace-settings-role-assign-named = { $role } zuweisen
workspace-settings-role-assign-named-placeholder = Identität der mitwirkenden Person
workspace-settings-app-action-placeholder = App, Ressource und Aktion
workspace-settings-app-action-grant = Einer App die Aktion einer anderen App erlauben
workspace-settings-app-action-grant-description = Erlaube einer Anwendung in diesem Arbeitsbereich, eine Aktion auf einer Ressource zu verwenden, die eine andere Anwendung bereitstellt.
workspace-settings-app-action-revoke = Einer App die Aktion einer anderen App entziehen
workspace-settings-app-action-revoke-description = Nimm in diesem Arbeitsbereich jede Freigabe zurück, mit der eine Anwendung eine Aktion auf einer Ressource einer anderen Anwendung verwenden darf.
workspace-settings-ownership-resource = Eigentum an { $workspace }
workspace-settings-ownership-propose = Eigentumsübertragung vorschlagen
workspace-settings-ownership-propose-placeholder = Identität der empfangenden Person
workspace-settings-ownership-propose-description = Deine Signatur erlaubt die sofortige Übertragung des Eigentums, wenn genau diese Administratorin oder dieser Administrator vor Ablauf annimmt.
workspace-settings-ownership-accept = Eigentum am Arbeitsbereich annehmen
workspace-settings-ownership-accept-description = Mit der Annahme wird das Eigentum sofort übertragen; allgemeine Rollensteuerungen können diese Aktion nicht ausführen.
workspace-settings-ownership-accept-phrase = Eigentum annehmen
workspace-settings-ownership-cancel = Eigentumsübertragung abbrechen
workspace-settings-ownership-cancel-description = Ziehe das offene Angebot zurück, bevor die empfangende Person es annimmt.
workspace-settings-title = Arbeitsbereichseinstellungen
workspace-settings-close = Einstellungen schließen
workspace-settings-tab-general = Allgemein
workspace-settings-tab-people = Personen
workspace-settings-tab-agents = Agenten
workspace-settings-tab-privacy-security = Datenschutz & Sicherheit
workspace-settings-tab-privacy = Datenschutz
workspace-settings-tab-apps-files = Apps & Dateien
workspace-settings-tab-apps = Apps
workspace-settings-close-hint = Esc zum Schließen
workspace-settings-edit-hint = Eingabetaste zum Speichern · Esc zum Abbrechen
workspace-settings-role-owner = Eigentümer
workspace-settings-role-editor = Bearbeiter
workspace-settings-role-commenter = Kommentator
workspace-settings-role-viewer = Betrachter
workspace-settings-role-blocked = Gesperrt
workspace-settings-role-admin = Admin
workspace-settings-role-guest = Gast
workspace-settings-role-owner-preview = Kann Personen, Berechtigungen und Einstellungen des Arbeitsbereichs verwalten.
workspace-settings-role-editor-preview = Kann geteilte Arbeit ansehen, kommentieren und bearbeiten.
workspace-settings-role-commenter-preview = Kann geteilte Arbeit ansehen und kommentieren, aber nicht bearbeiten.
workspace-settings-role-viewer-preview = Kann geteilte Arbeit ansehen, aber weder kommentieren noch bearbeiten.
workspace-settings-role-blocked-preview = Kein Zugriff auf diesen Arbeitsbereich.
workspace-settings-scope-workspace = dieser Arbeitsbereich
workspace-settings-scope-app = die App { $app }
workspace-settings-scope-workspace-files = Dateien des Arbeitsbereichs
workspace-settings-scope-workspace-folder = „{ $folder }“ in diesem Arbeitsbereich
workspace-settings-scope-path = Dateien unter { $path }
workspace-settings-scope-record = eine geteilte Datei ({ $id })
workspace-settings-scope-camera = die Kamera
workspace-settings-scope-microphone = das Mikrofon
workspace-settings-scope-screencast = Bildschirmaufnahme
workspace-settings-scope-remote-desktop = Fernsteuerung von Zeiger und Tastatur
workspace-settings-scope-any-host = ein beliebiger Netzwerkhost
workspace-settings-scope-host = Netzwerkhost { $host }
workspace-settings-scope-action = „{ $action }“ auf { $resource } in { $app }
workspace-settings-scope-action-on-kind = „{ $action }“ auf jeder Ressource vom Typ { $kind } in { $app }
workspace-settings-scope-resource-type = { $kind ->
    [note] jede Notiz
    [file] jede Datei
    [cluster] jeder gespeicherte Cluster
    [kanban] jedes Board
   *[chat-log] jeder Chatverlauf
}
workspace-settings-scope-resource-instance = ein Dokument ({ $id })
workspace-settings-scope-workspace-object = Diesen Arbeitsbereich
workspace-settings-scope-app-object = Die App { $app }
workspace-settings-scope-workspace-files-object = Dateien des Arbeitsbereichs
workspace-settings-scope-workspace-folder-object = „{ $folder }“ in diesem Arbeitsbereich
workspace-settings-scope-path-object = Dateien unter { $path }
workspace-settings-scope-record-object = Eine geteilte Datei ({ $id })
workspace-settings-scope-camera-object = Die Kamera
workspace-settings-scope-microphone-object = Das Mikrofon
workspace-settings-scope-screencast-object = Bildschirmaufnahme
workspace-settings-scope-remote-desktop-object = Fernsteuerung von Zeiger und Tastatur
workspace-settings-scope-any-host-object = Einen beliebigen Netzwerkhost
workspace-settings-scope-host-object = Netzwerkhost { $host }
workspace-settings-scope-action-object = „{ $action }“ auf { $resource } in { $app }
workspace-settings-scope-action-on-kind-object = „{ $action }“ auf jeder Ressource vom Typ { $kind } in { $app }
workspace-settings-scope-resource-type-object = { $kind ->
    [note] Jede Notiz
    [file] Jede Datei
    [cluster] Jeden gespeicherten Cluster
    [kanban] Jedes Board
   *[chat-log] Jeden Chatverlauf
}
workspace-settings-scope-resource-instance-object = Ein Dokument ({ $id })
workspace-settings-allows-control = { $what } steuern
workspace-settings-allows-use = { $what } verwenden
workspace-settings-allows-reach = { $what } erreichen
workspace-settings-allows-run = { $what } ausführen
workspace-settings-allows-administer = { $what } verwalten
workspace-settings-allows-edit = { $what } bearbeiten
workspace-settings-allows-comment = { $what } kommentieren
workspace-settings-allows-invite = Andere einladen: { $what }
workspace-settings-allows-view = { $what } ansehen
workspace-settings-expiry-once = Einmal
workspace-settings-expiry-session = Sitzung
workspace-settings-expiry-until = Bis
workspace-settings-expiry-always = Immer
workspace-settings-granted-by-person = erteilt von { $id }
workspace-settings-granted-by-you = von dir erteilt
workspace-settings-granted-by-app = erteilt von der App { $app }
workspace-settings-granted-by-agent = erteilt von Agent { $id }
workspace-settings-granted-by-token = erteilt über Token { $id }
workspace-settings-granted-by-workspace = erteilt vom Arbeitsbereich { $id }
workspace-settings-permission-detail = { $allows } · { $grantor }
workspace-settings-section-apps = Apps
workspace-settings-section-app-actions = App-Aktionen
workspace-settings-section-people = Personen
workspace-settings-section-agents = Agenten
workspace-settings-section-roles = Rollen
workspace-settings-section-other = Sonstige
workspace-settings-subject-role = alle mit der Rolle { $role }
workspace-settings-subject-token = Token { $id }
workspace-settings-subject-workspace = Arbeitsbereich { $id }
workspace-settings-grant-camera = Kamera
workspace-settings-grant-microphone = Mikrofon
workspace-settings-grant-screencast = Bildschirmaufnahme
workspace-settings-grant-remote-control = Fernsteuerung
workspace-settings-grant-network = Netzwerkzugriff
workspace-settings-grant-read-files = Dateien des Arbeitsbereichs lesen
workspace-settings-grant-edit-files = Dateien des Arbeitsbereichs bearbeiten
workspace-settings-grant-create-roles = Berechtigungsrollen erstellen
workspace-settings-grant-assign-roles = Berechtigungsrollen zuweisen
workspace-settings-preset-observer = Beobachter
workspace-settings-preset-contributor = Mitwirkender
workspace-settings-preset-operator = Operator
workspace-settings-preset-custom = Benutzerdefiniert
workspace-settings-preset-observer-hint = Nur Lesen auf Bereichen, die du auswählst
workspace-settings-preset-contributor-hint = Lesen und Schreiben nur auf Bereichen, die du auswählst
workspace-settings-preset-operator-hint = Nur freigegebene App-Aktionen ausführen
workspace-settings-preset-custom-hint = Jede eingeschränkte Berechtigung prüfen
workspace-settings-members-count = { $count ->
    [one] 1 Person teilt diesen Arbeitsbereich
   *[other] { $count } Personen teilen diesen Arbeitsbereich
}
workspace-settings-members-help = Wer entfernt wird, erhält keine Synchronisierung und Änderungen mehr; bereits heruntergeladene Kopien bleiben bei der Person.
workspace-settings-ownership = Eigentum
workspace-settings-ownership-action-pending = Übertragung ausstehend
workspace-settings-you-manage = Von dir verwaltet
workspace-settings-view-only = Nur ansehen
workspace-settings-members-empty-title = Sonst ist niemand in diesem Arbeitsbereich
workspace-settings-members-empty-detail = Erstelle im Tab „Allgemein“ eine signierte Einladung, um jemanden hinzuzufügen.
workspace-settings-people-header = PERSONEN
workspace-settings-ownership-offered = { $owner } hat dir das Eigentum angeboten. Sieh dir den Arbeitsbereich an und nimm ausdrücklich an, bevor das Angebot abläuft.
workspace-settings-ownership-waiting = Warte darauf, dass { $recipient } das Eigentum ausdrücklich annimmt. Bis dahin kannst du abbrechen.
workspace-settings-ownership-pending = Die Eigentumsübertragung an { $recipient } wartet auf die Annahme durch diese Person.
workspace-settings-you = Du
workspace-settings-member-detail = { $count ->
    [0] { $role } · keine zusätzlichen Berechtigungen
    [one] { $role } · 1 zusätzliche Berechtigung
   *[other] { $role } · { $count } zusätzliche Berechtigungen
}
workspace-settings-remove = Entfernen
workspace-settings-owner-only = Nur der Eigentümer des Arbeitsbereichs kann Berechtigungen ändern.
workspace-settings-agents-count = { $count ->
    [one] 1 mitwirkender Agent
   *[other] { $count } mitwirkende Agenten
}
workspace-settings-agents-help = Agenten sind sichtbar nicht-menschliche Mitwirkende. Die Zulassung gewährt keine allgemeinen Rechte im Arbeitsbereich.
workspace-settings-add-agent = Agent hinzufügen
workspace-settings-agents-empty-title = Keine Agenten haben Zugriff
workspace-settings-agents-empty-detail = Beginne mit einem sicheren Profil und prüfe dann jeden einzelnen Bereich.
workspace-settings-agents-header = AGENTEN
workspace-settings-service-member = Dienstmitglied · { $access ->
    [admin] verwaltet
    [write] liest und bearbeitet
    [comment] liest und kommentiert
   *[read] liest
} Ende-zu-Ende-verschlüsselte Inhalte · { $until } · Gerät { $device }
workspace-settings-service-until-removed = bis zur Entfernung
workspace-settings-service-ends-soon = Zugriff endet in weniger als einer Minute
workspace-settings-service-ends-minutes = Zugriff endet in { $minutes } Min.
workspace-settings-service-ends-hours = Zugriff endet in { $hours } Std.
workspace-settings-badge-service = DIENST
workspace-settings-badge-agent = AGENT
workspace-settings-agent-detail = { $count ->
    [0] { $admission } · keine eingeschränkten Berechtigungen – Prüfung erforderlich
    [one] { $admission } · Profil { $profile } · 1 eingeschränkte Berechtigung
   *[other] { $admission } · Profil { $profile } · { $count } eingeschränkte Berechtigungen
}
workspace-settings-agent-available = Für Aufgaben verfügbar
workspace-settings-agent-unavailable = Derzeit nicht für Aufgaben verfügbar
workspace-settings-agent-availability-unknown = Aufgabenverfügbarkeit unbekannt
workspace-settings-agent-phase-locating = Freigegebenes Ziel wird gesucht
workspace-settings-agent-phase-applying = Freigegebene Aktion wird angewendet
workspace-settings-agent-phase-progress = Freigegebene Aktion wird bearbeitet
workspace-settings-agent-phase-waiting = Wartet auf eine freigegebene Aktion
workspace-settings-agent-phase-committed = Freigegebene Aktion abgeschlossen
workspace-settings-agent-phase-failed = Freigegebene Aktion erfordert Aufmerksamkeit
workspace-settings-agent-phase-cancelled = Freigegebene Aktion abgebrochen
workspace-settings-agent-availability-phase = { $availability }. { $phase }
workspace-settings-agent-last-activity = Letzte Aktivität { $when }
workspace-settings-agent-no-activity = Keine Aktivität aufgezeichnet
workspace-settings-agent-activity-announcement = { $status } { $activity }.
workspace-settings-agent-profile-summary = { $authority }. { $activity }
workspace-settings-admitted = Zugelassen
workspace-settings-review-access = Zugriff prüfen
workspace-settings-revoke = Entziehen
workspace-settings-admission-admin = Zulassung als Admin
workspace-settings-admission-write = Zulassung zum Schreiben
workspace-settings-admission-comment = Zulassung zum Kommentieren
workspace-settings-admission-read = Zulassung zum Lesen
workspace-settings-unavailable-app = Nicht verfügbare App
workspace-settings-apps-empty-title = Keine Dateiverknüpfungen installiert
workspace-settings-apps-empty-detail = Apps erscheinen hier, sobald sie unterstützte Dateitypen angeben.
workspace-settings-apps-intro-title = Standards ohne Bindung
workspace-settings-apps-intro-detail = Der Arbeitsbereich gibt eine Grundlage vor; jede Person kann ausdrücklich etwas anderes wählen.
workspace-settings-apps-header = STANDARDANWENDUNGEN
workspace-settings-workspace-choice = Wahl des Arbeitsbereichs
workspace-settings-your-choice = Deine Wahl
workspace-settings-resource-extension = .{ $ext }-Dateien
workspace-settings-resource-scheme = { $scheme }:-Links
workspace-settings-follow-personal = Persönlichen Standards folgen
workspace-settings-use-workspace-choice = Dem Arbeitsbereich folgen
workspace-settings-use-personal-default = Meinen persönlichen Standard verwenden
workspace-settings-apps-file-types = { $count ->
    [one] 1 Dateityp
   *[other] { $count } Dateitypen
}
workspace-settings-apps-custom-rules = { $count ->
    [one] 1 eigene Regel
   *[other] { $count } eigene Regeln
}
workspace-settings-permissions-intro-title = Zugriff ist ausdrücklich und widerrufbar
workspace-settings-permissions-intro-detail = Freigaben werden von der App-Sandbox durchgesetzt und bleiben auf diesen Arbeitsbereich beschränkt.
workspace-settings-active-grants = { $count ->
    [one] 1 aktive Freigabe
   *[other] { $count } aktive Freigaben
}
workspace-settings-permissions-empty-title = Keine Berechtigungen erteilt
workspace-settings-permissions-empty-detail = Apps erhalten nichts, bis es hier erteilt oder in einer Abfrage erlaubt wird.
workspace-settings-more = +{ $count } weitere
workspace-settings-who = Wer
workspace-settings-who-agent = Wer · AGENT (nur dieser Bereich; kein allgemeiner Zugriff auf den Arbeitsbereich)
workspace-settings-nobody-to-grant = Noch niemand, dem du etwas erteilen kannst – lade jemanden ein oder öffne hier eine App.
workspace-settings-what-it-allows = Was es erlaubt
workspace-settings-grant-header = BERECHTIGUNG ERTEILEN
workspace-settings-saved-roles = Rollen verwalten ({ $count })
workspace-settings-expiry-caption = Gültigkeit
workspace-settings-grant = Erteilen
workspace-settings-role-sheet-title = Zugriff auf den Arbeitsbereich wählen
workspace-settings-role-sheet-description = { $person } ist derzeit { $role }. Wähle genau, was diese Person tun kann.
workspace-settings-remove-sheet-title = Diese Person entfernen?
workspace-settings-remove-sheet-description = Die Person erhält keine Synchronisierung und Änderungen mehr. Heruntergeladene Kopien bleiben auf ihren Geräten.
workspace-settings-revoke-sheet-title = Diese Berechtigung entziehen?
workspace-settings-revoke-sheet-description = Die ausgewählte Berechtigung gilt sofort nicht mehr. Prüfe zuerst ihre Aktivität, wenn du unsicher bist.
workspace-settings-keep-access = Zugriff behalten
workspace-settings-review-activity = Zuerst Aktivität prüfen
workspace-settings-remove-person = Person entfernen
workspace-settings-no-changes = Es werden keine Änderungen vorgenommen.
workspace-settings-open-history = Öffne vor der Entscheidung den Verlauf des Arbeitsbereichs.
workspace-settings-remove-person-detail = Dadurch verliert die Person ihre Berechtigungen im Arbeitsbereich.
workspace-settings-keep-permission = Berechtigung behalten
workspace-settings-revoke-permission = Berechtigung entziehen
workspace-settings-revoke-permission-detail = Dadurch wird die ausgewählte Berechtigung sofort entfernt.
workspace-settings-workspace-fallback = Arbeitsbereich
workspace-settings-identity-caption = Arbeitsbereich · { $role }
workspace-settings-current-workspace = AKTUELLER ARBEITSBEREICH
workspace-settings-role-policy-line = { $role } · Einstellungen und Richtlinien bleiben bei diesem Arbeitsbereich
workspace-settings-identity-title = Identität des Arbeitsbereichs
workspace-settings-identity-description = Dieser Name erscheint in der oberen Leiste, in Einladungen und in geteilten Aktivitäten.
workspace-settings-rename-admin-only = Nur Administratoren des Arbeitsbereichs können ihn umbenennen.
workspace-settings-invite-hint = Wähle eine Person und eine Rolle; Tilo signiert eine Einladung, die eine Stunde gilt.
workspace-settings-invite-admin-only = Nur verwaltende Administratoren können Einladungen in den Arbeitsbereich erstellen.
workspace-settings-invite-create = Erstellen
workspace-settings-devices-title = Deine Geräte
workspace-settings-device-here = { $device } (dieses Gerät)
workspace-settings-device-shown = Angezeigt
workspace-settings-device-hidden = Ausgeblendet
workspace-settings-move-earlier = Nach oben
workspace-settings-move-later = Nach unten
workspace-settings-delete = Workspace löschen
workspace-settings-invite-title = Personen einladen
workspace-settings-invite-description = Einladungen binden Arbeitsbereich, Rolle der empfangenden Person, Ablauf und die Identität dieses Geräts.
workspace-settings-glance-title = Auf einen Blick
workspace-settings-stat-members = { $count ->
    [one] Mitglied
   *[other] Mitglieder
}
workspace-settings-stat-grants = { $count ->
    [one] gezielte Freigabe
   *[other] gezielte Freigaben
}
workspace-settings-stat-app-rules = { $count ->
    [one] App-Regel
   *[other] App-Regeln
}

# People popover
a11y-shell-people = Personen
a11y-shell-people-description = Live-Anwesenheit im Arbeitsbereich, aktuelle Tätigkeit, Absicht und Datenschutz für den Status
a11y-shell-people-screen-visible = Jetzt sichtbarer Bildschirm: { $exposure }
a11y-shell-people-here-now = Jetzt anwesend
a11y-shell-people-agent = Agent
a11y-shell-people-invited = Eingeladen, derzeit nicht anwesend
a11y-shell-people-present = Jetzt anwesend
a11y-shell-people-now = Jetzt: { $activity }
a11y-shell-people-intent = Absicht: { $status }
a11y-shell-people-intent-inferred = Absicht, abgeleitet: { $status }
a11y-shell-status-name = Dein Zusammenarbeitsstatus
a11y-shell-status-inferred = Auf diesem Gerät aus letzten Aktionen abgeleitet
a11y-shell-status-set = Lege fest, woran du gerade arbeitest
a11y-shell-status-automatic = Automatischer Status
a11y-shell-status-manual = Nur manueller Status
a11y-shell-status-off = Status ausblenden
a11y-shell-status-automatic-description = Einen kurzen Status lokal aus den letzten Aktionen im Arbeitsbereich ableiten
a11y-shell-status-manual-description = Nur selbst eingegebenen Text teilen
a11y-shell-status-off-description = Keinen Status in diesem Arbeitsbereich veröffentlichen
a11y-shell-follow-collaborator = Mitwirkender Person folgen
a11y-shell-follow = { $name } folgen
a11y-shell-follow-description = Die Live-Ansicht dieser Person im Arbeitsbereich spiegeln
a11y-shell-open-member-settings = Mitgliedseinstellungen öffnen
a11y-shell-open-member-settings-for = Mitgliedseinstellungen für { $name } öffnen
a11y-shell-open-member-settings-description = Dauerhafte Mitgliedschaft und Rolle dieser Person im Arbeitsbereich prüfen
a11y-shell-open-agent-settings = Zugriff für { $name } prüfen
a11y-shell-open-agent-settings-description = Zulassung und ausdrücklich erteilte, eingeschränkte Berechtigungen dieses Agenten prüfen
a11y-shell-people-previous = Vorherige Personen
a11y-shell-people-previous-description = Die vorherigen Personen in diesem Arbeitsbereich anzeigen
a11y-shell-people-next = Weitere Personen
a11y-shell-people-next-description = Die nächsten Personen in diesem Arbeitsbereich anzeigen
a11y-shell-invite = Personen einladen
a11y-shell-invite-description = Eine authentifizierte Einladung in den Arbeitsbereich erstellen
a11y-shell-join = Arbeitsbereich beitreten
a11y-shell-join-description = Mit einer authentifizierten Einladung verbinden
a11y-shell-workspace-activity-description = Gruppierte Ergebnisse prüfen und sehen, was in deiner Abwesenheit passiert ist

# Empty-workspace launchpad
launchpad-title = Gemeinsam etwas schaffen
launchpad-description = Wähle eine gemeinsame Unterhaltung, Notizen, Aufgaben, einen Anruf, eine Einladung oder die Aktivität im Arbeitsbereich.
launchpad-open-app = App öffnen
launchpad-discuss = Besprechen
launchpad-notes = Notizen
launchpad-tasks = Aufgaben
launchpad-meet = Anruf starten
launchpad-invite = Personen einladen
launchpad-activity = Auf dem Laufenden bleiben
launchpad-discuss-detail = Die gemeinsame Unterhaltung öffnen
launchpad-notes-detail = Gemeinsam Ideen aufschreiben
launchpad-tasks-detail = Die Aufgabenliste im Kalender öffnen
launchpad-meet-detail = Mit diesem Arbeitsbereich im Chat sprechen
launchpad-invite-detail = Jemanden in diesen Arbeitsbereich holen
launchpad-activity-detail = Sehen, was sich geändert hat und warum

# Empty-workspace launchpad: the keyboard hint under the cards
launchpad-keyboard-hint = Pfeiltasten bewegen  ·  Eingabe öffnet  ·  Super+N zeigt alle Apps

# Peers arriving in and leaving a workspace, batched into one toast
peer-toast-joined-one = { $name } ist beigetreten
peer-toast-left-one = { $name } hat verlassen
peer-toast-joined-many = { $count } Personen sind beigetreten
peer-toast-left-many = { $count } Personen haben verlassen
peer-toast-overflow = { $first }, { $second }, { $third } und { $rest } weitere

# Moved from the shell catalogue with the code that uses them
people-panel-here-now = { $count } gerade hier
people-panel-screen-visible = Bildschirm gerade sichtbar
people-panel-only-you = Nur du bist hier
people-panel-status-typing = Gib einen Status ein und drücke dann die Eingabetaste
people-panel-status-empty = Lege fest, woran du gerade arbeitest …
people-panel-own-status = Du · { $status }
people-panel-sharing-auto = Automatisch
people-panel-previous-page = Vorherige Personen
people-panel-next-page = Nächste Personen
people-panel-sharing-manual = Manuell
people-panel-sharing-hidden = Verborgen
people-panel-agent-tag = Agent
people-panel-agent-capability-preset = Berechtigungsvorlage: { $preset }
people-panel-invited-tag = eingeladen
people-panel-following = Du folgst
people-panel-following-activity = Du folgst · { $activity }
people-panel-viewing = Sieht sich { $title } an
people-panel-follow = Folgen
people-panel-stop-following = Beenden
people-panel-invite = Einladen
people-panel-join = Beitreten
launchpad-subtitle = Wähle, wie du beginnen möchtest. Inhalte des Arbeitsbereichs werden synchronisiert; deine Anordnung und Bedienelemente bleiben lokal.
workspace-settings-sheet-keyboard-hint = ↑ ↓ zum Auswählen · Eingabetaste zum Bestätigen · Esc zum Abbrechen

# The emoji picker (Super+. by default)
emoji-picker-title = Emoji
emoji-picker-search = Emoji suchen
emoji-picker-groups = Emoji-Kategorien
emoji-picker-recent = Zuletzt verwendet
emoji-picker-results = Passende Emoji
emoji-picker-no-results = Keine Emoji gefunden
emoji-picker-hint = Eingabe fügt ein · Esc schließt
emoji-picker-hint-tones = Eingabe fügt ein · Umschalt+Eingabe wählt einen Hautton
emoji-picker-tones = Hauttöne
emoji-picker-tones-hint = Rechtsklick, langes Drücken oder Umschalt+Eingabe für Hauttöne
emoji-picker-a11y-description = Tippe, um zu suchen. Die Pfeiltasten wechseln zwischen den Emoji, Tab wechselt die Kategorie, Eingabe fügt das markierte Emoji in das Textfeld ein, in dem du geschrieben hast, Umschalt+Eingabe wählt einen Hautton, Escape schließt.

# Ein Emoji in einem Hautton: „Daumen hoch, mittlerer Hautton“.
emoji-picker-toned = { $name }, { $tone }
emoji-picker-tone-light = heller Hautton
emoji-picker-tone-medium-light = mittelheller Hautton
emoji-picker-tone-medium = mittlerer Hautton
emoji-picker-tone-medium-dark = mitteldunkler Hautton
emoji-picker-tone-dark = dunkler Hautton

# Der Fensterwechsler (Alt+Tab).
window-switcher-title = Fenster wechseln
window-switcher-description = Fenster in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke Tab oder die Pfeiltasten zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
window-switcher-app-title = Fenster dieser App wechseln
window-switcher-app-description = Fenster dieser App in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke ` zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
window-switcher-untitled = Unbenanntes Fenster
window-switcher-caption-paged = { $title } ({ $position } von { $count })

# Workspace wheel
a11y-shell-wheel = Arbeitsbereichsrad
a11y-shell-wheel-switch = Zu { $workspace } wechseln
a11y-shell-wheel-add = Arbeitsbereich hinzufügen

# Workspace overview
overview-empty-window = Leeres Fenster
overview-untitled = Ohne Titel
overview-access-nobody = Niemand hat Zugriff
overview-access-private = Nur für dich
overview-access-one = Geteilt mit { $name }
overview-access-two = Geteilt mit { $name } und { $other }
overview-access-many = Geteilt mit { $name } und { $count } weiteren
overview-shares = { $count ->
        [one] 1 Element geteilt
       *[other] { $count } Elemente geteilt
    }
overview-presence-you = Du bist hier
overview-presence-you-and-one = Du und { $name } seid hier
overview-presence-you-and-many = Du und { $count } weitere seid hier
overview-presence-one = { $name } ist hier
overview-presence-many = { $count } Personen sind hier
overview-collaborators-here = { $count } Mitwirkende sind hier

# The launchpad in the browser: a new member's first look at the desktop
browser-welcome-title = Willkommen bei Tilo
browser-welcome-subtitle = Der gemeinsame Computer deines Teams. Alles, was du machst, wird laufend gespeichert und Ende-zu-Ende-verschlüsselt.
browser-welcome-hint = Pfeiltasten bewegen  ·  Eingabe öffnet  ·  Strg+K findet jede App
browser-welcome-hint-mac = Pfeiltasten bewegen  ·  Eingabe öffnet  ·  ⌘K findet jede App

# The overview panel a host without its own overview draws
overview-panel-title = Übersicht
overview-panel-you-are-here = Du bist hier
overview-panel-windows = Fenster hier
overview-panel-no-windows = Noch keine Fenster
overview-panel-more-windows = { $count ->
    [one] und 1 weiteres Fenster
   *[other] und { $count } weitere Fenster
}
overview-panel-window-count = { $count ->
    [one] 1 Fenster
   *[other] { $count } Fenster
}
overview-panel-go-to = Wechseln zu
overview-panel-new-workspace = Neuer Arbeitsbereich
browser-new-workspace-name = Arbeitsbereich { $number }

notification-center-read = Lesen
notification-center-read-description = Vollständige Benachrichtigung lesen
notification-preview-hidden = Benachrichtigungsvorschau verborgen
activity-task-pending-action-unavailable = Die Änderung konnte nicht bestätigt werden. Dein Entwurf bleibt erhalten. Gehe zurück und lies die aktuelle Aufgabe.
activity-task-edit-pending = Anweisung bearbeiten
activity-task-cancel-pending = Aufgabe in Warteschlange abbrechen
activity-task-pending-instruction = Vollständige Anweisung in der Warteschlange
activity-task-pending-instruction-placeholder = Was soll der Agent tun?
activity-task-pending-instruction-description = Die vollständige Anweisung für diese Aufgabe in der Warteschlange bearbeiten.
activity-task-save-pending = Speichern
activity-task-pending-saving = Aufgabe wird gespeichert…
activity-task-pending-cancelling = Aufgabe wird abgebrochen…
activity-task-cancel-edit = Bearbeiten abbrechen
activity-task-amend-saved = Anweisung gespeichert.
activity-task-pending-cancelled = Aufgabe in der Warteschlange abgebrochen.

activity-catch-up-goals = Aufgaben im Arbeitsbereich · { $done } abgeschlossen · { $unfinished } offen · aktualisiert { $when }
activity-catch-up-review-description = Prüfe wartende Aufgaben, Ziele und die Arbeit von Agenten in diesem Arbeitsbereich. Das Lesen der Aktivitäten schließt keine Aufgabe ab.
activity-catch-up-review-goals = Ziele und die Arbeit von Agenten prüfen
activity-acknowledge-viewed = Gesehene Aktivitäten bestätigen
activity-acknowledge-description = Markiere nur die angezeigten Zusammenfassungen als gelesen. Verborgene Änderungen und wartende Aufgaben bleiben offen.
activity-catch-up-needs-review = Deine Prüfung ist nötig · { $count ->
    [one] eine wartende Aufgabe
   *[other] { $count } wartende Aufgaben
    }
activity-catch-up-waiting-detail = { $task } · { $who }

notification-alarm-snoozed = Um 5 Minuten verschoben. Verwirf diesen Eintrag, um die ausstehende Erinnerung abzubrechen.
notification-alarm-dismiss = Verwerfen
notification-alarm-snooze = In 5 Minuten erneut erinnern
notification-alarm-action-failed = Dieser Wecker konnte nicht geändert werden
notification-alarm-action-title = { $action }: { $title }
notification-alarm-action-subtitle = Für dieses Weckereignis
notification-alarm-action-retry = Behalte diese Benachrichtigung und versuche es erneut.

activity-task-review-unavailable = Authentifizierte Aufgabensteuerung ist nicht verfügbar. Der bisherige Verlauf bleibt lesbar.
activity-task-review-history = Aktionsverlauf prüfen
activity-task-steer = Anweisungen ändern
activity-task-cancel-run = Diesen Lauf abbrechen
activity-task-retry-run = Aufgabe erneut versuchen
activity-task-direction-send = Anweisungen senden
activity-task-direction-close = Zurück zur Aufgabe
activity-task-direction-label = Öffentliche Aufgabenanweisungen
activity-task-direction-placeholder = Beschreibe, was der Agent ändern soll
activity-task-direction-observation = Diese Anweisungen gelten für den geprüften Lauf. Wenn die Aufgabe neu startet, prüfe sie erneut.
activity-task-review-saving = Warten auf das Speichern durch den Arbeitsbereich…
activity-task-review-saved = Im Arbeitsbereich gespeichert. Die Anwendung durch den Agenten wird separat angezeigt.
activity-task-review-refused = Nicht gespeichert. Dein Entwurf bleibt erhalten. { $detail }

activity-task-next = Nächste Aufgabe
activity-task-previous = Vorherige Aufgabe
activity-task-review-source = Vom Arbeitsbereich bestätigt · aktualisiert { $time }
activity-task-review-status = { $status } · Agent: { $agent }
activity-task-review-frontier = Lauf { $epoch } · Anweisungen { $revision } · vom Agenten angewendet { $applied }
activity-task-review-direction-waiting = Warten auf Agent
activity-task-review-direction-applied = Vom Agenten angewendet
activity-task-review-direction = { $author }: { $text }
activity-task-review-missing = Diese Aufgabe ist in diesem Arbeitsbereich nicht mehr verfügbar.
activity-task-review-stopping = Stopp angefordert; Bestätigung des Agenten steht aus
activity-task-current-instruction = Aktuelle Anweisung
activity-task-review-changed = Diese Aufgabe hat sich geändert. Prüfe ihren aktuellen Stand, bevor du sie änderst.
activity-task-original-instruction = Ursprüngliche Anfrage

activity-task-approve-run = Ausführung freigeben
activity-task-run-approved = Genehmigt von { $person } · { $time }
activity-task-awaiting-acceptance = Warte auf Annahme · Agent möglicherweise offline
activity-task-run-accepted = Agent hat diesen Lauf angenommen · letzter Bericht { $time }
activity-task-run-planning = Agent plant diesen Lauf · letzter Bericht { $time }
activity-task-run-executing = Agent bearbeitet diesen Lauf · letzter Bericht { $time }
