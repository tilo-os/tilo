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
activity-summary-glance = Auf einen Blick
activity-more-sessions = Weitere Arbeitssitzungen…
activity-first-sessions = Erste Arbeitssitzungen…
activity-brief-underlying = { $count } { $count ->
        [one] zugrunde liegende Aktion
       *[other] zugrunde liegende Aktionen
    }
activity-brief-open-explorer = Diese Arbeitssitzung im Explorer öffnen →
activity-brief-show-affected = Betroffene Ressourcen und Aktionen anzeigen ⌄
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
activity-task-executor-agent = Ausführung · ▣ AGENT · { $agent }
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
activity-task-agent-badge = ▣ AGENT · { $name }
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
search-window-move-tab-detail = App { $app } · aus dem Arbeitsbereich „{ $workspace }“ · Gruppe { $group } · Tab { $tab }
search-window-group-untitled = Fenstergruppe { $group }
search-window-close-group = Fenstergruppe „{ $group }“ schließen
search-window-close-group-detail = Arbeitsbereich „{ $workspace }“ · Leiste „{ $strip }“ · Gruppe { $group } · { $count ->
    [one] 1 Tab
   *[other] { $count } Tabs
}
search-window-close-tab = Tab „{ $tab }“ schließen
search-window-close-tab-detail = App { $app } · Arbeitsbereich „{ $workspace }“ · Leiste „{ $strip }“ · Gruppe { $group } · Tab { $tab }
