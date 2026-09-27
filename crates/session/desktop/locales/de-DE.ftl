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
