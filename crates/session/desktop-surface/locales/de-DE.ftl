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
topbar-sharing-window-in-workspace = { $window } in { $workspace }
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
topbar-open-notifications = Benachrichtigungen
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
topbar-waved-at-you = { $name } hat dir zugewunken
topbar-arrived-one = { $name } ist da
topbar-arrived-more = { $name } und { $count ->
    [one] eine weitere Person
   *[other] { $count } weitere
} sind da
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
topbar-strip-name = Leisten zeigen, gerade { $strip }
topbar-workspace-name-placeholder = Arbeitsbereich benennen
topbar-workspace-menu = { $workspace }: Arbeitsbereiche, Übersicht und Leisten
topbar-profile = { $name } (du): dein Profil
topbar-face-agent = { $name }, Agent
topbar-faces-more = { $count } weitere
workspace-menu-hosted-by = Gehostet von { $host }
workspace-menu-rename = Arbeitsbereich umbenennen
workspace-menu-overview = Übersicht
workspace-menu-new-strip = Neue Leiste
profile-card = Dein Profil
profile-name = Dein Name
profile-name-placeholder = Dein Name
profile-name-hint = So sehen dich alle hier
profile-watching-hint = Du schaust zu. Niemand hier sieht dich
profile-add-your-name = Deinen Namen hinzufügen
profile-section-identity = Deine Identität
profile-section-devices = Geräte
profile-section-linked = Verknüpfte Konten
profile-link = { $service }-Konto verknüpfen
profile-unlink = Trennen
profile-copy = Kopieren
profile-sharing = Teilen, woran du arbeitest
profile-sharing-auto = Automatisch
profile-sharing-manual = Eigene Worte
profile-sharing-off = Gar nicht
profile-section-status = Status
profile-available = Verfügbar
profile-focusing = Konzentriert
profile-away = Abwesend
profile-status = Was du gerade machst
profile-status-placeholder = Was du gerade machst
profile-section-colour = Deine Farbe
profile-section-visibility = Was andere in { $workspace } sehen
profile-visibility-where = Wo ich bin
profile-visibility-here = Dass ich da bin
profile-visibility-nothing = Nichts
profile-visibility-where-detail = Dein Gesicht, dein Fenster und dein Zeiger
profile-visibility-here-detail = Dein Gesicht, nie dein Fenster oder dein Zeiger
profile-visibility-nothing-detail = Du erscheinst nicht und siehst auch die Zeiger der anderen nicht
profile-control-hints = Bedienhinweise zeigen
control-hint-open = Alles öffnen
control-hint-move = Zum Verschieben ziehen
control-hint-resize = Größe per Ziehen ändern
control-hint-pan = Freie Fläche ziehen

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
a11y-shell-activity-open-tasks-description = Öffnet die App Aufgaben mit den Aufgaben dieses Arbeitsbereichs
a11y-shell-activity-time-range = Zeitraum
a11y-shell-activity-person-filter = Personenfilter
a11y-shell-activity-brief-description = { $detail }; zuletzt geändert { $when }; { $count } { $count ->
        [one] Änderung
       *[other] Änderungen
    } von { $authors }
a11y-shell-activity-history-lens = Verlaufsansicht
a11y-shell-activity-undo-task-description = Jeden betroffenen Eintrag dieser Agent-Aufgabe rückgängig machen; spätere Änderungen anderer bleiben erhalten
a11y-shell-activity-history-action = Aktion für den ausgewählten Verlauf
a11y-shell-activity-explorer-panel = Explorer-Bereich
a11y-shell-activity-timeline-description = Verlaufseinträge; mit den Pfeiltasten navigieren
a11y-shell-activity-checkpoints-description = Durch die Zwischenstände des Arbeitsbereichs gehen
a11y-shell-activity-replay-description = Die Wiedergabe des Arbeitsbereichs abspielen oder pausieren
a11y-shell-activity-view-catch-up = Eine Zusammenfassung der Ergebnisse, seit dieses Gerät den Arbeitsbereich zuletzt angesehen hat
a11y-shell-activity-view-explore = Der vollständige Verlauf des Arbeitsbereichs mit Filtern und Herkunft
a11y-shell-activity-view-replay = Eine kausale Wiedergabe der Anordnung und der Aktivität der Beteiligten
a11y-shell-activity-entry-author = Von { $author }
a11y-shell-activity-selected-detail = Details zum ausgewählten Verlauf
a11y-shell-activity-selected-detail-description = { $title }; { $count } { $count ->
        [one] Aktion
       *[other] Aktionen
    }; von { $authors }
a11y-shell-activity-agent-run-description = Anweisung: { $instruction }; zugewiesen von { $issuer }; ausgeführt von { $agent }; Begründung: { $reason }
a11y-shell-activity-reason-not-recorded = nicht erfasst
a11y-shell-activity-agent-step = { $app } in { $record }: { $summary }
a11y-shell-activity-agent-review = Prüfung der Agent-Aufgabe
activity-view-catch-up = Neues
activity-view-tasks = Aufgaben
activity-view-explore = Verlauf
activity-view-replay = Wiedergabe
activity-title-catch-up = Während du weg warst
activity-title-whats-new = Neues
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
activity-resume-button = Fortsetzen
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
activity-history-gone = Diese Änderungen sind nicht mehr im Verlauf.
activity-served-personal-only = Das kann nur die Person an diesem Gerät.
activity-served-read-only = Dieser Desktop zeigt den Verlauf, kann ihn aber nicht ändern.
activity-served-unavailable = Der Verlauf dieses Arbeitsbereichs wird hier nicht aufbewahrt.
activity-summary-on-device = Zusammenfassung auf dem Gerät
activity-session-move-newer = 1 Aktion neuer
activity-session-move-older = 1 Aktion älter
activity-more-sessions = Weitere Arbeitssitzungen…
activity-first-sessions = Erste Arbeitssitzungen…
activity-brief-underlying = { $count } { $count ->
        [one] Schritt
       *[other] Schritte
    }
activity-brief-open-explorer = Diese Arbeitssitzung im Verlauf öffnen
activity-brief-show-affected = Jeden Schritt zeigen
activity-browse-by = Durchsuchen nach
activity-more-lenses = Weitere Ansichten…
activity-first-lenses = Erste Ansichten…
activity-lens-everything = Alles
activity-lens-agent-tasks = Agent-Aufgaben  { $count }
activity-lens-unclassified = Nicht zugeordnet  { $count }
activity-provenance = Herkunft
activity-select-prompt = Wähle eine Arbeitssitzung oder einen Bereich von Aktionen
activity-select-hint = Sieh, wer es war, was es betraf, und vergleiche vorher und nachher.
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
activity-undo-task-partial-records = Aufgabe rückgängig gemacht, außer den unten genannten Einträgen.
activity-undo-task-record-left = { $record } in { $app } wurde nicht rückgängig gemacht.
activity-detail-undo-step = Diesen Schritt rückgängig
activity-undo-task-done = Ganze Aufgabe als neuer Verlaufseintrag rückgängig gemacht.
activity-undo-task-gone = Die Auswirkungen der Aufgabe waren bereits entfernt.
activity-undo-task-refused = Die Aufgabe konnte nicht rückgängig gemacht werden. { $detail }
activity-undo-task-unavailable = Aufgaben können hier nicht rückgängig gemacht werden.
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
search-window-nth = { $nth } von { $count }
search-window-placeholder = { $searched } durchsuchen…
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
notification-center-newer = Neuere
notification-center-older = Ältere
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
workspace-settings-tab-emoji = Emoji
workspace-settings-emoji-count = { $count ->
    [0] Eigene Emoji dieses Arbeitsbereichs
    [one] 1 Emoji dieses Arbeitsbereichs
   *[other] { $count } Emoji dieses Arbeitsbereichs
}
workspace-settings-emoji-help = Schreibe :Name: in einer beliebigen App dieses Arbeitsbereichs oder wähle eines bei den Emoji einer Reaktion.
workspace-settings-emoji-add = Emoji hinzufügen
workspace-settings-emoji-empty-title = Dieser Arbeitsbereich hat noch keine eigenen Emoji
workspace-settings-emoji-empty-detail = Wähle „Emoji hinzufügen“ und ein Bild: Sein Dateiname wird der Name des Emoji.
workspace-settings-emoji-empty-detail-viewer = Wer diesen Arbeitsbereich bearbeiten darf, kann welche hinzufügen.
workspace-settings-emoji-unavailable = Die Emoji dieses Arbeitsbereichs sind auf diesem Gerät nicht verfügbar.
workspace-settings-emoji-editors-add = Nur wer diesen Arbeitsbereich bearbeiten darf, kann Emoji hinzufügen.
workspace-settings-emoji-added-by = Hinzugefügt von { $name }
workspace-settings-emoji-added-by-you = Von dir hinzugefügt
workspace-settings-emoji-remove = :{ $name }: entfernen
workspace-settings-emoji-not-chosen = Es wurde kein Bild ausgewählt
workspace-settings-emoji-not-added = { $file } wurde nicht als Emoji hinzugefügt
workspace-settings-emoji-not-removed = :{ $name }: wurde nicht entfernt
workspace-settings-emoji-tools-loading = Die Bildwerkzeuge werden noch geladen. Versuche es gleich noch einmal.
workspace-settings-emoji-not-a-picture = Nur PNG-, JPEG- und WebP-Bilder können Emoji werden.
workspace-settings-emoji-too-detailed = Das Bild ist zu detailreich, um in Emoji-Größe scharf zu bleiben. Versuche ein einfacheres.
workspace-settings-emoji-no-name = Sein Dateiname enthält keine Buchstaben oder Ziffern, nach denen ein Emoji heißen könnte.
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
workspace-settings-ownership-cancel-short = Übertragung abbrechen
workspace-settings-ownership-transfer = Eigentum übertragen
workspace-settings-you-manage = Von dir verwaltet
workspace-settings-view-only = Nur ansehen
workspace-settings-members-empty-title = Sonst ist niemand in diesem Arbeitsbereich
workspace-settings-members-empty-detail = Erstelle im Tab „Allgemein“ eine signierte Einladung, um jemanden hinzuzufügen.
workspace-settings-people-header = Personen
workspace-settings-ownership-offered = { $owner } hat dir das Eigentum angeboten. Sieh dir den Arbeitsbereich an und nimm ausdrücklich an, bevor das Angebot abläuft.
workspace-settings-ownership-waiting = Warten, bis { $recipient } das Eigentum annimmt.
workspace-settings-ownership-pending = Die Eigentumsübertragung an { $recipient } wartet auf die Annahme durch diese Person.
workspace-settings-you = Du
workspace-settings-member-extra = { $count ->
    [one] 1 zusätzliche Berechtigung
   *[other] { $count } zusätzliche Berechtigungen
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
workspace-settings-bots-header = Bots
workspace-settings-add-bot = Bot hinzufügen
workspace-settings-bots-empty = In diesem Arbeitsbereich laufen keine Bots
workspace-settings-bot-tag = Bot
workspace-settings-bot-starting = Startet
workspace-settings-bot-paused = Angehalten
workspace-settings-bot-running = Läuft
workspace-settings-bot-version = Version { $version }
workspace-settings-remove-bot = Entfernen
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
workspace-settings-agent-model = Modell
workspace-settings-agent-model-default = Der Standard: { $model }
workspace-settings-agent-model-none = Kein Modell in den Einstellungen gewählt
workspace-settings-agent-model-use = Verwenden
workspace-settings-agent-model-in-use = In Verwendung
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
workspace-settings-nobody-to-grant = Noch niemand, dem du etwas erteilen kannst. Lade jemanden ein oder öffne hier eine App.
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
workspace-settings-current-workspace = Aktueller Arbeitsbereich
workspace-settings-identity-title = Identität des Arbeitsbereichs
workspace-settings-identity-description = Dieser Name erscheint in der oberen Leiste, in Einladungen und in geteilten Aktivitäten.
workspace-settings-rename-admin-only = Nur Administratoren des Arbeitsbereichs können ihn umbenennen.
workspace-settings-invite-admin-only = Nur Administratoren dieses Arbeitsbereichs können Personen einladen.
workspace-settings-invite-create = Einladung erstellen
workspace-settings-devices-title = Deine Geräte
workspace-settings-devices-local = Fensteranordnung und Bedienelemente bleiben auf jedem Gerät.
workspace-settings-public-title = Öffentlich
workspace-settings-public-asking = Der Host dieses Arbeitsbereichs wird gefragt…
workspace-settings-public-on = Alle können ihn im Verzeichnis seines Hosts finden und beitreten.
workspace-settings-public-off = Nur Eingeladene können beitreten. Mach ihn öffentlich, um ihn im Verzeichnis seines Hosts aufzuführen.
workspace-settings-public-switch-on = Öffentlich
workspace-settings-public-switch-off = Privat
workspace-settings-public-role = Beitretende: { $role }
workspace-settings-public-describe = Beschreibung…
workspace-settings-device-here = { $device } (dieses Gerät)
workspace-settings-device-shown = Angezeigt
workspace-settings-device-hidden = Ausgeblendet
workspace-settings-move-earlier = Nach oben
workspace-settings-move-later = Nach unten
workspace-settings-delete = Workspace löschen
workspace-settings-delete-description = Entfernt ihn von diesem Gerät und deinen anderen Geräten.
workspace-settings-delete-action = Löschen
workspace-settings-archived-title = Archivierte Workspaces
workspace-settings-archived-restore = Wiederherstellen
workspace-settings-archived-delete = Löschen
workspace-settings-invite-title = Personen einladen
workspace-settings-invite-description = Wähle eine Person und was sie hier tun darf. Jede Einladung gilt eine Stunde.
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
a11y-shell-people-description = Wer hier ist, wo und woran gerade gearbeitet wird
a11y-shell-people-here-now = Jetzt anwesend
a11y-shell-people-agent = Agent
a11y-shell-people-now = Jetzt: { $activity }
a11y-shell-people-recent-toggle = Was { $name } zuletzt gemacht hat
a11y-shell-people-recent-toggle-description = Ihre letzten Änderungen ein- oder ausblenden
a11y-shell-rename = Deinen Namen ändern, { $name }
a11y-shell-rename-description = Wähle den Namen, den andere hier sehen
a11y-shell-sharing-change = Ändern, was du teilst
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
launchpad-title-private = Etwas anfangen
launchpad-description = Wähle eine Unterhaltung, Notizen, Aufgaben, einen Anruf, eine Einladung oder die Aktivität im Arbeitsbereich.
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
launchpad-discuss-detail-private = Die Unterhaltung dieses Arbeitsbereichs öffnen
launchpad-notes-detail-private = Ideen aufschreiben
launchpad-meet-detail-private = Jemanden im Chat anrufen
launchpad-invite-detail-private = Gemeinsam mit jemandem daran arbeiten

# Empty-workspace launchpad: the keyboard hint under the cards
launchpad-keyboard-hint = Pfeiltasten bewegen, Eingabe öffnet, Super+N zeigt alle Apps

# Peers arriving in and leaving a workspace, batched into one toast

# Moved from the shell catalogue with the code that uses them
people-panel-agent-tag = Agent
people-panel-bot-tag = Bot
people-panel-viewing = Sieht sich { $title } an
people-panel-in-app = In { $app }
people-panel-here-count = { $count ->
    [0] Nur du
    [one] Du und 1 weitere Person
   *[other] Du und { $count } weitere
}
people-panel-here-watching = { $count ->
    [0] Nur du
    [one] Du und 1 weitere Person
   *[other] Du und { $count } weitere
}, { $watching } schauen zu
people-panel-here-spectating = { $count ->
    [0] Niemand hier
    [one] 1 Person hier
   *[other] { $count } Personen hier
}, { $watching } schauen zu, du auch
people-panel-section-people = Personen
people-panel-section-agents = Agenten
people-panel-section-devices = Deine Geräte
people-panel-wave = Winken
people-panel-waved = Gewunken
people-panel-join = Beitreten
people-panel-in-call = Im Kanal { $channel }
people-panel-in-a-call = Im Anruf
people-panel-focusing = Konzentriert
people-panel-away = Abwesend
people-panel-idle = Inaktiv
people-panel-empty = Gerade ist niemand sonst hier
people-panel-changes = Ihre Änderungen
people-panel-invite-people = Personen einladen
a11y-shell-wave = { $name } zuwinken
a11y-shell-waved = Du hast { $name } zugewunken
a11y-shell-wave-description = Ihr Gesicht zeigt, dass du gewunken hast
a11y-shell-join-call = { $name }s Anruf beitreten
a11y-shell-join-call-description = Tritt dem Sprachkanal bei, in dem sie sind
a11y-shell-people-changes = Was { $name } geändert hat
a11y-shell-people-changes-description = Den Verlauf bei ihren Änderungen öffnen
a11y-shell-people-change-description = Den Verlauf bei dieser Änderung öffnen
people-panel-follow = Folgen
people-panel-stop-following = Beenden

# The people overlay: the roster pinned out of the bar, floating over windows
people-panel-pin = Auf dem Bildschirm halten
a11y-people-pin = Wer hier ist, auf dem Bildschirm halten
people-overlay-title = { $count ->
    [0] Nur du
    [one] 1 hier
   *[other] { $count } hier
}
people-overlay-unpin = Zurück in die Leiste
people-overlay-follow-name = { $name } folgen
people-overlay-doing-nothing = Hier
people-overlay-screen-private = { $name } behält für sich, wo { $name } gerade ist
people-overlay-empty-desk = Auf dem Bildschirm von { $name } ist nichts offen
people-overlay-message-placeholder = Nachricht an { $name }
people-overlay-send = Senden
people-overlay-own-status = Was du gerade machst
people-overlay-own-status-placeholder = Woran arbeitest du?
people-overlay-own-status-clear = Leeren, was du gerade machst
people-overlay-no-messages = Sag { $name } Hallo
people-overlay-messages-unavailable = Direktnachrichten brauchen die Tilo-App auf diesem Gerät
people-overlay-message-failed = Die Nachricht an { $name } ging nicht raus: { $reason }
a11y-people-follow = { $name } folgen
a11y-people-stop-following = { $name } nicht mehr folgen
launchpad-subtitle = Wähle, wie du anfängst. Alle in diesem Arbeitsbereich sehen, was du hier machst.
launchpad-subtitle-private = Wähle, wie du anfängst. Nur du siehst, was du hier machst, bis du jemanden einlädst.
launchpad-subtitle-public = Wähle, wie du anfängst. Dieser Arbeitsbereich ist öffentlich: Alle, die beitreten, sehen, was du hier machst.

# The emoji picker (Super+. by default)
emoji-picker-title = Emoji
emoji-picker-search = Emoji suchen
emoji-picker-groups = Emoji-Kategorien
emoji-picker-recent = Zuletzt verwendet
emoji-picker-results = Passende Emoji
emoji-picker-no-results = Keine Emoji gefunden
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
overview-strip-empty = Keine Fenster
overview-panel-no-windows = Noch keine Fenster
overview-panel-window-count = { $count ->
    [one] 1 Fenster
   *[other] { $count } Fenster
}
overview-panel-go-to = Wechseln zu
overview-panel-new-workspace = Neuer Arbeitsbereich
browser-new-workspace-name = Arbeitsbereich { $number }
room-notice-joined = { $name } ist dazugekommen
room-notice-renamed = { $name } hat den Arbeitsbereich in { $workspace } umbenannt
browser-app-crashed = { $app } funktionierte nicht mehr und wurde geschlossen, damit der Rest des Desktops weiterläuft. Schließe dieses Fenster oder öffne { $app } erneut.
browser-app-crashed-reopen = Erneut öffnen

# Das Menü hinter dem Tilo-Zeichen der oberen Leiste und das Info-Blatt, das es öffnet
tilo-menu-about = Über diesen Computer
tilo-menu-about-browser = Über Tilo
tilo-menu-feedback = Feedback geben…
tilo-about-title = Über diesen Computer
tilo-about-title-browser = Über Tilo
tilo-about-lead = Das ist Tilo, ein Betriebssystem.
tilo-about-body-browser = Du benutzt Tilo in deinem Browser. Bald läuft es auch direkt auf deinem eigenen Computer.
tilo-about-version = Version { $version }
tilo-about-link-get = Tilo holen
tilo-about-link-home = tilo.so
tilo-about-body-native = Es läuft direkt hier, auf diesem Computer.
tilo-about-link-notify = Benachrichtigt werden
tilo-about-link-source = Quellcode
tilo-about-link-community = Community

notification-preview-hidden = Benachrichtigungsvorschau verborgen

activity-acknowledge-viewed = Als gesehen markieren
activity-acknowledge-description = Markiert die hier gezeigten Änderungen als gesehen. Wartende Aufgaben bleiben offen.
notification-alarm-snoozed = Um 5 Minuten verschoben. Verwirf diesen Eintrag, um die ausstehende Erinnerung abzubrechen.
notification-alarm-dismiss = Verwerfen
notification-alarm-snooze = In 5 Minuten erneut erinnern
notification-alarm-action-failed = Dieser Wecker konnte nicht geändert werden
notification-alarm-action-title = { $action }: { $title }
notification-alarm-action-subtitle = Für dieses Weckereignis
notification-alarm-action-retry = Behalte diese Benachrichtigung und versuche es erneut.

activity-morning-choose = Für den Morgen auswählen
activity-morning-remove = Morgenquelle entfernen
activity-morning-empty = Morgenquellen: Ressource unter Verlauf, dann Details auswählen. Bis zu acht Quellen; keine wird automatisch verbunden.
activity-morning-description = Ausgewählte Morgenquellen: Anbieter-Vorschauen aus autorisierten Ressourcen. Aktualisierung oder Zeitplan ausdrücklich wählen.
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

emoji-picker-hint-tone = Hautton wählen

workspace-settings-hint-close = Schließen
workspace-settings-hint-save = Speichern
workspace-settings-hint-cancel = Abbrechen
workspace-settings-hint-choose = Auswählen

workspace-settings-hint-apply = Übernehmen

# Jemand anderes hat ein Fenster außer Sicht geöffnet: der Hinweis am Rand
desktop-window-opened-elsewhere = { $person } hat { $app } geöffnet
# Strip switcher: the strip ribbon grown into labels, one per strip of the workspace.
strip-switcher-a11y = Leisten
strip-switcher-unnamed = Leiste { $number }
strip-switcher-new-strip = Neue Leiste
strip-switcher-rename-named = { $name } umbenennen
strip-switcher-name-placeholder = Name der Leiste
strip-switcher-refused = Die Leiste konnte nicht geändert werden
strip-switcher-remove-named = { $name } entfernen
strip-switcher-rename = Umbenennen
strip-switcher-remove = Entfernen

## Ein Problem melden, indem man darauf zeigt
feedback-report-problem = Problem melden…
feedback-picker-hint = Klicke auf das, was nicht funktioniert. Umschalt-Klick oder Ziehen wählt mehr, Scrollen erweitert, Esc verlässt.
feedback-picker-picked = { $count ->
    [one] 1 Teil gewählt. Umschalt-Klick oder Ziehen wählt mehr.
   *[other] { $count } Teile gewählt. Umschalt-Klick oder Ziehen wählt mehr.
}
feedback-picker-continue = Weiter
feedback-picker-cancel = Abbrechen

# Der Ansicht einer Person folgen
follow-label = Du folgst { $person }
follow-stopped = Folgen beendet
follow-stop = Beenden

people-follows = Folgt { $person }
people-follows-you = Folgt dir
people-follows-where = { $place }, folgt { $person }
people-follows-you-where = { $place }, folgt dir
