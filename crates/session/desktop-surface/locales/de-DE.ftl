# Obere Leiste
continuity-dismiss = Ausblenden
topbar-workspace-fallback = Arbeitsbereich
topbar-sharing-agent-controls = Agent steuert: { $name }
topbar-sharing-agent-viewing = Agent sieht zu: { $name }
topbar-sharing-person-controls = Steuert: { $name }
topbar-sharing-person-viewing = Sieht zu: { $name }
topbar-sharing-session-controls = Steuert: { $name }
topbar-sharing-session-viewing = Sieht zu: { $name }
topbar-sharing-count-controlling = { $viewers } { $viewers ->
        [one] sieht
       *[other] sehen
    } zu, { $controllers } { $controllers ->
        [one] steuert
       *[other] steuern
    }
topbar-sharing-count = { $viewers } { $viewers ->
        [one] sieht
       *[other] sehen
    } zu
topbar-sharing-unknown-window = ein Fenster
topbar-watching-count = +{ $count } { $count ->
        [one] schaut zu
       *[other] schauen zu
    }
topbar-watching-description = { $count } { $count ->
        [one] Person schaut
       *[other] Personen schauen
    } zu, ohne teilzunehmen
topbar-tilo-menu = Tilo-Menü
topbar-open-quick-settings = Schnelleinstellungen öffnen
topbar-bell = { $unread ->
    [0] Mitteilungen
    [one] Mitteilungen, 1 ungelesen
   *[other] Mitteilungen, { $unread } ungelesen
}
topbar-bell-silenced = { $unread ->
    [0] Mitteilungen stummgeschaltet
    [one] Mitteilungen stummgeschaltet, 1 ungelesen
   *[other] Mitteilungen stummgeschaltet, { $unread } ungelesen
}
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
topbar-strips = Leisten in diesem Arbeitsbereich
topbar-strip-go = Zu { $name } wechseln, { $count ->
    [0] niemand dort
    [one] eine Person dort
   *[other] { $count } Personen dort
}
topbar-strip-name = Leisten zeigen, gerade { $strip }
topbar-strip-add = Leiste hinzufügen
topbar-strips-more-label = +{ $count }
topbar-strips-more = { $count ->
    [one] Eine weitere Leiste
   *[other] { $count } weitere Leisten
}, zu { $name } wechseln
topbar-strip-name-placeholder = Leiste benennen
topbar-workspace-name-placeholder = Arbeitsbereich benennen
topbar-workspace-menu = { $workspace }: Arbeitsbereiche, Übersicht und Schreibtische
topbar-profile = { $name }: dein Profil
workspace-menu-hosted-by = Gehostet von { $host }
workspace-menu-rename = Arbeitsbereich umbenennen
workspace-menu-overview = Übersicht
workspace-menu-new-desk = Neuer Schreibtisch
profile-card = Dein Profil
profile-name = Dein Name
profile-name-placeholder = Dein Name
profile-name-hint = So sehen dich alle hier
profile-section-identity = Deine Identität
profile-section-devices = Geräte
profile-section-linked = Verknüpfte Konten
profile-link = { $service }-Konto verknüpfen
profile-unlink = Trennen
profile-copy = Kopieren

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
activity-view-catch-up = Neues
activity-view-tasks = Aufgaben
activity-view-explore = Verlauf
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
activity-resume-heading = Zuletzt bearbeitet
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
activity-brief-open-explorer = Diese Arbeitssitzung im Verlauf öffnen →
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
activity-receipt-ended = { $status }: { $what }
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
activity-tasks-subtitle = { $workspace } · { $active ->
        [0] nichts offen
        [one] 1 offene Aufgabe
       *[other] { $active } offene Aufgaben
    }
activity-tasks-tagline = Was Personen und Agenten in diesem Arbeitsbereich erledigen
activity-tasks-pending = Aufgaben warten auf laufende Bearbeitungen
activity-tasks-empty = Noch keine Aufgaben
activity-tasks-empty-hint = Lege hier eine an oder bitte in einer beliebigen App einen Agenten darum.
activity-task-new = + Neue Aufgabe
activity-task-queue = Warteschlange des Arbeitsbereichs
activity-task-composer-title = Arbeit anlegen, ohne den Schreibtisch zu verlassen
activity-task-placeholder = Was muss erledigt werden?
activity-task-executor-manual = Erledigt von dir oder einer anderen Person
activity-task-executor-none = Kein Agent verfügbar. Die Aufgabe bleibt unzugewiesen.
activity-task-executor-agent = Agent: { $agent }
activity-task-no-attachment = Keine Ressource angehängt · Aufgabe gilt für den ganzen Arbeitsbereich
activity-task-attached = Angehängt: { $resource } in { $app }
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
activity-task-audit-trail = Prüfprotokoll
activity-task-audit-noted = { $what }: { $note }
workspace-create-task = { $manual ->
    [true] Aufgabe „{ $title }“ zum Selbermachen angelegt
   *[false] Aufgabe „{ $title }“ angelegt
}
workspace-attach-resource = { $resource } an die Aufgabe angehängt
workspace-set-execution-budget = Laufzeit der Aufgabe festgelegt
workspace-approve-run = Lauf { $run } freigegeben
workspace-start-run = Lauf { $run } gestartet
workspace-add-direction = Anweisung { $revision } gegeben
workspace-apply-direction = Anweisung { $revision } befolgt
workspace-finish = { $outcome ->
    [done] Aufgabe erledigt
    [waiting] Angehalten, um zu warten
   *[cancelled] Aufgabe abgebrochen
}
workspace-abandon = Lauf aufgegeben
workspace-retry = Aufgabe erneut eingereiht
workspace-cancel = Aufgabe abgebrochen
workspace-report = Fortschritt gemeldet
workspace-set-status = { $status ->
    [queued] Aufgabe eingereiht
    [in-progress] Aufgabe begonnen
    [waiting] Aufgabe auf Warten gesetzt
    [done] Aufgabe als erledigt markiert
   *[cancelled] Aufgabe abgebrochen
}
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
           *[write] Schreiben
        }
       *[grant] { $verb ->
            [read] Lesen
           *[write] Schreiben
        }
    } für { $scope } an { $subject }
activity-capability-unknown = eine Berechtigung
activity-capability-this-workspace = diesen Arbeitsbereich
activity-capability-one-shared-file = eine geteilte Datei
activity-capability-network = Netzwerk { $host }
activity-capability-role = { $role ->
        [owner] Eigentümer
        [moderator] Moderierende
        [editor] Bearbeitende
        [commenter] Kommentierende
       *[viewer] Betrachtende
    }
activity-grant = { $kinds ->
        [read] Lesen
        [undoable] Änderungen
        [outside] Wirkungen nach außen
        [read-undoable] Lesen und Änderungen
        [read-outside] Lesen und Wirkungen nach außen
        [undoable-outside] Änderungen und Wirkungen nach außen
        [none] nichts
       *[all] alles
    } { $answer ->
        [deny] verweigert
        [ask] nach Rückfrage
       *[allow] erlaubt
    } für { $node } an { $subject }
activity-grant-task = Aufgabe { $task }
activity-grant-shortcut = Kurzbefehl { $shortcut }
activity-grant-bot = Bot { $bot }
activity-grant-foreign-device = Geräte, die nicht die eigenen sind
activity-member = { $person } als { $role ->
        [owner] Eigentümer
        [moderator] Moderierende
        [editor] Bearbeitende
        [commenter] Kommentierende
       *[viewer] Betrachtende
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
search-window-move-tab = Tab „{ $tab }“ in den Arbeitsbereich „{ $workspace }“ verschieben
search-window-move-tab-detail = Aus dem Arbeitsbereich „{ $workspace }“
search-window-copy-tab = Tab „{ $tab }“ in den Arbeitsbereich „{ $workspace }“ kopieren
search-window-move-tab-to = „{ $tab }“ in einen anderen Arbeitsbereich verschieben
search-window-copy-tab-to = „{ $tab }“ in einen anderen Arbeitsbereich kopieren
search-window-group-untitled = Unbenanntes Fenster
search-window-close-group = Fenstergruppe „{ $group }“ schließen
search-window-close-group-detail = Arbeitsbereich „{ $workspace }“ · Leiste „{ $strip }“ · { $count ->
    [one] 1 Tab
   *[other] { $count } Tabs
}
search-window-close-tab = Tab „{ $tab }“ schließen
search-window-close-tab-detail = Arbeitsbereich „{ $workspace }“ · Leiste „{ $strip }“
search-window-focus-place-strip = Leiste „{ $strip }“
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
a11y-notifications-newer = Neuere Benachrichtigungen
a11y-notifications-older = Ältere Benachrichtigungen
a11y-notifications = Benachrichtigungen
notification-center-title = Benachrichtigungen
notification-center-empty = Keine neuen Benachrichtigungen
notification-center-cleared = Benachrichtigungen gelöscht
notification-center-clear-all = Alle löschen
notification-center-position = { $first }–{ $last } von { $total }
notification-center-close = Benachrichtigungen schließen
notification-center-back-to-list = Zurück zu den Benachrichtigungen
notification-center-newer = ↑ Neuere
notification-center-older = ↓ Ältere
history-severity-info = Information
history-severity-warning = Warnung
history-severity-error = Fehler
history-notification-description = { $severity }. { $time }
history-notification-description-body = { $severity }. { $time }. { $body }
history-notification-description-sourced = { $severity }. Von { $source }. { $time }
history-notification-description-sourced-body = { $severity }. Von { $source }. { $time }. { $body }
host-action-source-identity = Identität
host-action-source-devices = Geräte
host-action-source-workspace = Arbeitsbereich
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
notification-more = { $count } weitere
notification-stack-a11y = Benachrichtigungen
notification-group-system = System
notification-center-undo = Rückgängig
notification-not-now = Später
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
workspace-settings-role-moderator = Moderator
workspace-settings-role-editor = Bearbeiter
workspace-settings-role-commenter = Kommentator
workspace-settings-role-viewer = Betrachter
workspace-settings-role-blocked = Gesperrt
workspace-settings-role-admin = Admin
workspace-settings-role-guest = Gast
workspace-settings-role-owner-preview = Kann Personen, Berechtigungen und Einstellungen des Arbeitsbereichs verwalten.
workspace-settings-role-moderator-preview = Kann geteilte Arbeit bearbeiten, Nachrichten anderer entfernen, Kanäle sperren und Bearbeitende und darunter entfernen oder sperren.
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
workspace-settings-grant-allows = Erlaubt: { $kinds } in { $where }
workspace-settings-grant-asks = Fragt zuerst: { $kinds } in { $where }
workspace-settings-grant-denies = Verweigert: { $kinds } in { $where }
workspace-settings-kind-read = Lesen
workspace-settings-kind-undoable = Änderungen
workspace-settings-kind-outside = Wirkungen nach außen
workspace-settings-subject-foreign-device = Geräte, die nicht die eigenen sind
workspace-settings-grant-placeholder = Wer, wo und was
workspace-settings-grant-description = Jemandem, einem Agenten oder einer App eine Antwort für Arten von Aktionen in einem Teil dieses Arbeitsbereichs geben.
workspace-settings-revoke-placeholder = Welche Berechtigung
workspace-settings-revoke-description = Eine Berechtigung anhand ihrer Kennung zurücknehmen.
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
workspace-settings-subject-bot = Bot { $id }
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
workspace-settings-you-manage = Von dir verwaltet
workspace-settings-view-only = Nur ansehen
workspace-settings-members-empty-title = Sonst ist niemand in diesem Arbeitsbereich
workspace-settings-members-empty-detail = Erstelle im Tab „Allgemein“ eine signierte Einladung, um jemanden hinzuzufügen.
workspace-settings-people-header = Personen
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
workspace-settings-agents-header = Agenten
workspace-settings-service-member = { $access ->
    [admin] Verwaltet
    [write] Liest und bearbeitet
    [comment] Liest und kommentiert
   *[read] Liest
} verschlüsselte Inhalte · { $until }
workspace-settings-service-until-removed = bis zur Entfernung
workspace-settings-service-ends-soon = Zugriff endet in weniger als einer Minute
workspace-settings-service-ends-minutes = Zugriff endet in { $minutes } Min.
workspace-settings-service-ends-hours = Zugriff endet in { $hours } Std.
workspace-settings-badge-service = Dienst
workspace-settings-badge-agent = Agent
workspace-settings-agent-detail = { $count ->
    [0] { $admission } · Noch keine Berechtigungen, Prüfung nötig
    [one] { $admission } · { $profile }, 1 Berechtigung
   *[other] { $admission } · { $profile }, { $count } Berechtigungen
}
workspace-settings-admitted = Zugelassen
workspace-settings-review-access = Zugriff prüfen
workspace-settings-agent-access-back = Alle Agenten
workspace-settings-agent-access-note = Lege fest, was dieser Agent in jeder App tun darf. Senden, Veröffentlichen und endgültiges Löschen fragen dich trotzdem zuerst, außer du hast für diese Aktion „Immer erlauben“ gewählt.
workspace-settings-agent-access-read = Lesen
workspace-settings-agent-access-change = Änderungen
workspace-settings-agent-access-outside = Wirkungen nach außen
workspace-settings-agent-access-summary-part = { $kind }: { $answer }
workspace-settings-agent-access-custom = Angepasst
workspace-settings-agent-access-custom-detail = Für diesen Agenten festgelegt
workspace-settings-agent-access-inherited = Folgt der Zulassung
workspace-settings-agent-access-set-here = hier festgelegt
workspace-settings-agent-access-default = Standard
workspace-settings-agent-access-reset = Standard verwenden
workspace-settings-agent-access-always = Immer erlaubt · { $count }
workspace-settings-agent-access-stop = Wieder fragen
workspace-settings-agent-access-empty = Noch keine Apps in diesem Arbeitsbereich.
workspace-settings-answer-allow = Erlauben
workspace-settings-answer-ask = Erst fragen
workspace-settings-answer-deny = Verbieten
workspace-settings-revoke = Entziehen
workspace-settings-admission-admin = Zulassung als Admin
workspace-settings-admission-write = Zulassung zum Schreiben
workspace-settings-admission-comment = Zulassung zum Kommentieren
workspace-settings-admission-read = Zulassung zum Lesen
workspace-settings-unavailable-app = Nicht verfügbare App
workspace-settings-apps-empty-title = Keine Dateiverknüpfungen installiert
workspace-settings-apps-empty-detail = Noch keine App gibt einen unterstützten Dateityp an.
workspace-settings-apps-intro-title = Standards ohne Bindung
workspace-settings-apps-intro-detail = Der Arbeitsbereich gibt eine Grundlage vor; jede Person kann ausdrücklich etwas anderes wählen.
workspace-settings-apps-header = Standard-Apps
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
workspace-settings-who-agent = Wer: ein Agent, nur mit Zugriff auf dieses Fenster
workspace-settings-nobody-to-grant = Noch niemand, dem du etwas erteilen kannst – lade jemanden ein oder öffne hier eine App.
workspace-settings-what-it-allows = Was es erlaubt
workspace-settings-grant-header = Berechtigung erteilen
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
workspace-settings-current-workspace = Aktueller Arbeitsbereich
workspace-settings-role-policy-line = { $role } · Einstellungen und Richtlinien bleiben bei diesem Arbeitsbereich
workspace-settings-identity-title = Identität des Arbeitsbereichs
workspace-settings-identity-description = Dieser Name erscheint in der oberen Leiste, in Einladungen und in geteilten Aktivitäten.
workspace-settings-rename-admin-only = Nur Administratoren des Arbeitsbereichs können ihn umbenennen.
workspace-settings-invite-hint = Wähle eine Person und eine Rolle. Die Einladung ist signiert und gilt eine Stunde.
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
a11y-shell-people-recent = Zuletzt: { $summary }
a11y-shell-people-recent-inferred = Zuletzt, zusammengefasst: { $summary }
a11y-shell-people-recent-toggle = Was { $name } zuletzt gemacht hat
a11y-shell-people-recent-toggle-description = Eine kurze Zusammenfassung der letzten Minuten ein- oder ausblenden
a11y-shell-people-watching = { $count } { $count ->
        [one] Person schaut
       *[other] Personen schauen
    } zu, ohne teilzunehmen
a11y-shell-status-name = Dein Zusammenarbeitsstatus
a11y-shell-status-inferred = Auf diesem Gerät aus letzten Aktionen abgeleitet
a11y-shell-status-set = Lege fest, woran du gerade arbeitest
people-panel-sharing-line-auto = Du teilst, was du gerade machst
people-panel-sharing-line-manual = Du teilst nur, was du selbst schreibst
people-panel-sharing-line-off = Du teilst nicht, was du gerade machst
people-panel-sharing-change = Ändern
people-panel-sharing-choice-auto = Automatisch teilen
people-panel-sharing-choice-manual = Selbst schreiben
people-panel-sharing-choice-off = Nicht teilen
a11y-shell-rename = Deinen Namen ändern, { $name }
a11y-shell-rename-description = Wähle den Namen, den andere hier sehen
a11y-shell-sharing-change = Ändern, was du teilst
a11y-shell-status-automatic-description = Einen kurzen Status lokal aus den letzten Aktionen im Arbeitsbereich ableiten
a11y-shell-status-manual-description = Nur selbst eingegebenen Text teilen
a11y-shell-status-off-description = Keinen Status in diesem Arbeitsbereich veröffentlichen
a11y-shell-follow-collaborator = Mitwirkender Person folgen
a11y-shell-follow = { $name } folgen
a11y-shell-follow-description = Die Live-Ansicht dieser Person im Arbeitsbereich spiegeln
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
launchpad-keyboard-hint = Pfeiltasten bewegen, Eingabe öffnet, Super+N zeigt alle Apps

# Peers arriving in and leaving a workspace, batched into one toast

# Moved from the shell catalogue with the code that uses them
people-panel-here-now = { $count } gerade hier
people-panel-watching = +{ $count } { $count ->
        [one] schaut zu
       *[other] schauen zu
    }
people-panel-screen-visible = Bildschirm gerade sichtbar
people-panel-only-you = Nur du bist hier
people-panel-status-typing = Gib einen Status ein und drücke dann die Eingabetaste
people-panel-status-empty = Lege fest, woran du gerade arbeitest …
people-panel-own-status = Du · { $status }
people-panel-agent-tag = Agent
people-panel-invited-tag = eingeladen
people-panel-following = Du folgst
people-panel-following-activity = Du folgst · { $activity }
people-panel-viewing = Sieht sich { $title } an
people-panel-follow = Folgen
people-panel-stop-following = Beenden
people-panel-invite = Einladen
people-panel-join = Beitreten
launchpad-subtitle = Wähle, wie du beginnen möchtest. Inhalte des Arbeitsbereichs werden synchronisiert; deine Anordnung und Bedienelemente bleiben lokal.
workspace-settings-sheet-keyboard-hint = ↑ ↓ wählen, Eingabetaste bestätigt, Esc bricht ab

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
window-switcher-caption-paged = { $position } von { $count }

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

# The launchpad in the browser: a new member's first look at the desktop
browser-welcome-title = Willkommen bei Tilo
browser-welcome-subtitle = Der gemeinsame Computer deines Teams. Alles, was du machst, wird laufend gespeichert und Ende-zu-Ende-verschlüsselt.
browser-welcome-hint = Pfeiltasten bewegen, Eingabe öffnet, Strg+K findet jede App
browser-welcome-hint-mac = Pfeiltasten bewegen, Eingabe öffnet, ⌘K findet jede App

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

# Das Menü hinter dem Tilo-Zeichen der oberen Leiste und das Info-Blatt, das es öffnet
tilo-menu-about = Über diesen Computer
tilo-menu-feedback = Feedback geben…
tilo-about-title = Über diesen Computer
tilo-about-lead = Das ist Tilo, ein Betriebssystem.
tilo-about-body-browser = Was du gerade benutzt, läuft in deinem Browser. Tilo kommt bald nativ auf Computer.
tilo-about-body-native = Es läuft direkt hier, auf diesem Computer.
tilo-about-link-notify = Benachrichtigt werden
tilo-about-link-source = Quellcode
tilo-about-link-community = Community

notification-preview-hidden = Benachrichtigungsvorschau verborgen

activity-catch-up-goals = Aufgaben im Arbeitsbereich: { $done } abgeschlossen, { $unfinished } offen · aktualisiert { $when }
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

activity-task-review-unavailable = Der Arbeitsbereich ist gerade nicht erreichbar, daher lassen sich Aufgaben nicht ändern. Ihr Verlauf bleibt lesbar.
activity-task-review-history = Änderungen ansehen
activity-task-steer = Anweisungen ändern
activity-task-cancel-run = Aufgabe abbrechen
activity-task-retry-run = Erneut versuchen
activity-task-direction-send = Anweisungen senden
activity-task-direction-close = Zurück zur Aufgabe
activity-task-direction-label = Anweisungen für den Agenten
activity-task-direction-placeholder = Beschreibe, was der Agent ändern soll
activity-task-direction-observation = Sag dem Agenten, was er anders machen soll. Er folgt deinen Anweisungen ab seinem nächsten Schritt, und alle in diesem Arbeitsbereich können sie sehen.
activity-task-review-saving = Wird gespeichert…
activity-task-review-saved = Gespeichert. Warte auf die Antwort des Agenten.
activity-task-review-refused = Nicht gespeichert. Dein Entwurf bleibt erhalten. { $detail }

activity-confirm-waiting = Wartet auf deine Bestätigung
activity-task-step-searching = Sucht im Web…
activity-task-step-reading = Liest { $app }…
activity-task-step-editing = Bearbeitet { $app }…
activity-task-step-exploring = Schaut, was { $app } kann…
activity-confirm-confirmed = Bestätigt
activity-confirm-declined = Abgelehnt
activity-confirm-risk-sends = Sendet nach außen
activity-confirm-risk-deletes = Nicht rückgängig zu machen
activity-confirm-risk-privileged = Ändert Zugriffe
activity-confirm-risk-sensitive = Teilt private Daten
activity-confirm-risk-changes = Ändert deine Arbeit
activity-confirm-risk-reads = Liest deine Arbeit
activity-confirm-meta = { $agent } fragt zuerst · { $resource }
activity-confirm-withheld = Verborgen
activity-confirm-yes = Ja
activity-confirm-no = Nein
activity-confirm-list = { $count }: { $items }
activity-confirm-list-label = { $label } ({ $count })
activity-confirm-more = { $count } weitere
activity-confirm-to = An
activity-confirm-cc = Cc
activity-confirm-bcc = Bcc
activity-confirm-subject = Betreff
activity-confirm-body = Nachricht
activity-confirm-attachments = Anhänge
activity-confirm-confirm = Erlauben
activity-confirm-edit = Bearbeiten
activity-confirm-discard = Änderungen verwerfen
activity-confirm-decline = Ablehnen
activity-confirm-send-decline = Aktion ablehnen
activity-confirm-back = Zurück
activity-confirm-always = Immer erlauben
activity-confirm-always-note = „Immer erlauben“ ändert den Agentenzugriff dieses Arbeitsbereichs und funktioniert deshalb nur auf dem Gerät, das den Arbeitsbereich verwaltet. Du kannst es unter Arbeitsbereichseinstellungen › Agenten zurücknehmen.
activity-confirm-reason-label = Grund für den Agenten (optional)
activity-confirm-reason-placeholder = Sag, was stattdessen passieren soll
activity-confirm-field-description = Vor dem Bestätigen ändern
activity-confirm-on-its-way = Deine Antwort ist auf dem Weg zum Agenten.
activity-confirm-notice-title = { $agent } fragt zuerst: { $action }
activity-confirm-notice-body = { $task }
activity-confirm-notice-gist = { $task } · { $gist }
activity-confirm-notice-review = Ansehen

activity-task-next = Nächste Aufgabe
activity-task-previous = Vorherige Aufgabe
activity-task-review-source = Stand beim Arbeitsbereich, Version { $generation } · abgerufen { $time }
activity-task-review-status = { $status ->
        [queued] { $agent } hat noch nicht angefangen
        [in-progress] { $agent } arbeitet daran
        [waiting] { $agent } hat angehalten, ohne fertig zu werden
        [done] { $agent } ist damit fertig
       *[cancelled] Abgebrochen
    }
activity-task-review-frontier = Lauf { $epoch } · { $revision ->
        [one] 1 Anweisung gesendet
       *[other] { $revision } Anweisungen gesendet
    } · { $applied } vom Agenten übernommen
activity-task-review-direction = { $author }: „{ $text }“
activity-task-review-missing = Diese Aufgabe ist in diesem Arbeitsbereich nicht mehr verfügbar.

activity-task-review-status-unassigned = { $status ->
        [done] Erledigt
        [cancelled] Abgebrochen
       *[other] Noch arbeitet niemand daran
    }
activity-task-review-status-asking = { $agent } wartet auf deine Antwort
activity-task-details = Details
activity-task-hide-details = Details ausblenden
activity-task-position = { $index } von { $count }
activity-task-run-approved-by-you = Von dir freigegeben · { $time }
activity-task-approve-run = Freigeben
activity-task-run-approved = Freigegeben von { $person } · { $time }
activity-task-awaiting-acceptance = Wartet darauf, dass der Agent die Aufgabe übernimmt. Er ist vielleicht offline.
activity-task-run-accepted = Der Agent hat die Aufgabe übernommen · zuletzt { $time }
activity-task-run-planning = Der Agent plant die Schritte · zuletzt { $time }
activity-task-run-executing = Der Agent nimmt Änderungen vor · zuletzt { $time }

activity-morning-choose = Für den Morgen auswählen
activity-morning-remove = Morgenquelle entfernen
activity-morning-empty = Morgenquellen: Ressource unter Verlauf → Details auswählen. Bis zu acht Quellen; keine wird automatisch verbunden.
activity-morning-description = Ausgewählte Morgenquellen · Anbieter-Vorschauen aus autorisierten Ressourcen. Aktualisierung oder Zeitplan ausdrücklich wählen.
activity-morning-unavailable = Diese Quelle ist hier nicht verfügbar
activity-morning-source-unnamed = Gewählte Quelle
activity-morning-source = { $source }, letzte Aktivität { $time } · möglicherweise nicht aktuell

activity-morning-open = Morgenquelle öffnen

activity-morning-payload-unavailable = Anbieter-Zusammenfassung nicht verfügbar. Quelle öffnen, um sie zu prüfen oder zu aktualisieren.

activity-morning-refresh = Quelle aktualisieren
activity-morning-draft = Geplante Aktualisierung entwerfen…
activity-morning-draft-description = Öffnet einen Entwurf in Shortcuts. Zeitplan wählen und Berechtigungen vor dem Speichern prüfen; noch ist nichts aktiviert.
activity-morning-command-unavailable = Quellenaktion nicht verfügbar
activity-morning-command-pending = Anfrage läuft
activity-morning-command-accepted = Anfrage angenommen; Ergebnis in Aktionen prüfen
activity-morning-command-refused = Anfrage abgelehnt
activity-morning-recorded = { $source } · letzte aufgezeichnete Arbeitsbereichsaktivität: { $time }

activity-morning-next = Nächste ausgewählte Quelle
