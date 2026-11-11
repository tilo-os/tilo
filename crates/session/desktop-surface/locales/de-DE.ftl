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
# A running application's icon in the bar, when the item that named it is gone.
topbar-app-tray-item = Laufende Anwendung
topbar-app-tray-unread = { $count } ungelesen
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
topbar-show-collaborators = Alle hier
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
workspace-menu-delete = Arbeitsbereich löschen
workspace-menu-workspace = Arbeitsbereich
profile-card = Dein Profil
profile-card-of = Profil von { $name }
profile-id = Personen-ID
profile-copy-id = ID kopieren
profile-copy-device-key = Schlüssel dieses Geräts kopieren
profile-device-this = Dieser Computer
profile-device-another = Ein anderes Gerät
profile-device-mark = dieses Gerät
profile-status-own-words = In eigenen Worten
profile-status-automatic = Automatisch aus dem, was die Person tut
profile-message = Nachricht
profile-follow = Folgen
profile-stop-following = Nicht mehr folgen
profile-photo-change = Bild ändern
profile-photo-remove = Bild entfernen
profile-name = Dein Name
profile-name-placeholder = Dein Name
profile-name-hint = So sehen dich alle hier
profile-add-your-name = Deinen Namen hinzufügen
profile-section-identity = Deine Identität
profile-section-devices = Geräte
profile-section-linked = Verknüpfte Konten
profile-link = Mit { $service } verbinden
profile-unlink = Trennen
profile-linked-as = Verknüpft als { $account }
profile-linked-to = Verknüpft mit { $handle } auf { $service }
profile-link-offer = { $service } { $account } mit { $name } verknüpfen?
profile-link-offer-why = Was du in { $service } schreibst, erscheint hier als du, und dort ebenso.
profile-link-accept = Verknüpfen
profile-link-decline = Abbrechen
profile-connecting = { $service } wird geöffnet…
profile-looking = Deine { $service }-Verknüpfung wird geprüft…
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
control-hint-pan = Seitwärts scrollen
control-hint-pan-touch = Links oder rechts wischen

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
activity-detail-undo-run = Diesen ganzen Lauf rückgängig
activity-detail-split = Teilen
activity-detail-merge = Zusammenführen
activity-detail-action-count = { $count } { $count ->
        [one] Aktion
       *[other] Aktionen
    }
activity-action-mark-viewed = Als gesehen markieren
activity-action-focused = Melden, was gezeigt wurde
activity-action-name = Änderungen benennen
activity-action-move-boundary = Grenze der Arbeitssitzung verschieben
activity-action-undo-record = Diese Änderung rückgängig machen
activity-action-choose-source = Morgenquelle wählen
activity-action-source = Morgenquelle verwenden
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
activity-undo-run-needs-open = Öffne jedes Dokument, das dieser Lauf geändert hat, um ihn gemeinsam rückgängig zu machen.
activity-undo-run-in-progress = Das Rückgängigmachen dieses Laufs läuft noch.
activity-undo-run-nothing-to-undo = Dieser Lauf hat kein Dokument geändert, das einen eigenen Verlauf führt.
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
desktop-search-copied = Kopiert
desktop-search-searching = Suche läuft…
desktop-view-switch-workspace = Zu { $workspace } wechseln
desktop-view-review-activity = Arbeitsbereich-Aktivität ansehen
desktop-view-explore-activity = Arbeitsbereich-Aktivität erkunden
desktop-view-replay-activity = Arbeitsbereich-Aktivität wiedergeben

## Desktop window search
search-window-nth = { $nth } von { $count }
search-window-placeholder = { $searched } durchsuchen…

# Notifications: banner stack and notification centre
drill-in-back = Zurück
a11y-notifications-none = Keine aufbewahrten Benachrichtigungen.
a11y-notifications-description = { $count ->
    [one] { $count } aufbewahrte Benachrichtigung, nach App gruppiert.
   *[other] { $count } aufbewahrte Benachrichtigungen, nach App gruppiert.
} Mit Pfeil hoch und runter wählen, mit Bild auf und Bild ab einen Bildschirm weiterspringen, mit Pos1 oder Ende zur neuesten oder ältesten springen. Eingabe öffnet die gewählte Benachrichtigung oder klappt ihre App-Gruppe auf, Entf verwirft sie.
a11y-notifications-list = Aufbewahrte Benachrichtigungen
a11y-notifications-clear = Alle Benachrichtigungen löschen
a11y-notifications-clear-description = Löscht den aufbewahrten Benachrichtigungsverlauf dieser Sitzung. Acht Sekunden lang kannst du das rückgängig machen.
a11y-notifications-cleared = Benachrichtigungen gelöscht
a11y-notifications-undo = Löschen der Benachrichtigungen rückgängig machen
a11y-notifications-undo-description = Stellt die gelöschten Benachrichtigungen in ihrer ursprünglichen Reihenfolge wieder her. Drücke Eingabe oder Leertaste.
a11y-notifications = Benachrichtigungen
notification-center-title = Benachrichtigungen
notification-center-empty = Keine neuen Benachrichtigungen
notification-center-cleared = Benachrichtigungen gelöscht
notification-center-clear-all = Alle löschen
notification-center-close = Benachrichtigungen schließen
notification-center-back-to-list = Zurück zu den Benachrichtigungen
history-severity-info = Information
history-severity-warning = Warnung
history-severity-error = Fehler
history-notification-description-sourced = { $severity }. Von { $source }. { $time }
history-notification-description-sourced-body = { $severity }. Von { $source }. { $time }. { $body }
host-action-source-identity = Identität
host-action-source-devices = Geräte
host-action-source-workspace = Arbeitsbereich
host-action-source-session = Dieser Computer
notification-center-group-description = { $count } Benachrichtigungen. Eingabe zum { $expanded ->
        [yes] Einklappen
       *[no] Ausklappen
    }, Entf, um alle zu verwerfen.
notification-center-dismiss-all = Alle von { $app } verwerfen
notification-reply = Antworten
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
notification-more-from = +{ $count } weitere von { $app }
notification-stack-a11y = Benachrichtigungen
notification-group-system = System
notification-center-undo = Rückgängig
notification-not-now = Später
notification-app-failed-title = Eine App konnte nicht geöffnet werden
notification-service-unavailable = Das ist auf diesem Desktop noch nicht verfügbar

# Deine Geräte: ein neues von einem deiner anderen koppeln und deine
# Identitäten verwalten, wie die Einstellungen die Sitzung darum bitten.
search-action-identity-resource = Deine Identitäten
search-action-identity-list = Deine Identitäten anzeigen
search-action-identity-list-subtitle = Die Identitäten auf diesem Gerät zeigen und mit welcher sich neue Apps anmelden
search-action-identity-activate = Identität für neue Apps verwenden
search-action-identity-activate-subtitle = Apps, die du ab jetzt öffnest, mit einer deiner Identitäten anmelden
search-action-identity-remove = Identität von diesem Gerät entfernen
search-action-identity-remove-subtitle = Eine Identität, die du hier nicht mehr nutzt, samt ihrem Schlüssel löschen
search-action-identity-protect = Schlüssel einer Identität schützen
search-action-identity-protect-subtitle = Den Schlüssel einer Identität aus einer einfachen Datei in geschützten Speicher verschieben
search-action-identity-devices-list = Vertrauenswürdige Geräte anzeigen
search-action-identity-devices-list-subtitle = Die Geräte zeigen, denen die Identität dieses Arbeitsbereichs vertraut, und welche widerrufen sind
search-action-identity-device-revoke = Gerät einer Identität widerrufen
search-action-identity-device-revoke-subtitle = Einem verlorenen, ausgemusterten oder kompromittierten Gerät dauerhaft nicht mehr vertrauen
search-action-identity-home-peer-remove = Heimserver entfernen
search-action-identity-home-peer-remove-subtitle = Einen gekoppelten Heimserver widerrufen, damit er deine Arbeitsbereiche nicht mehr bereitstellt
search-action-identity-home-peer-retry = Entfernen von Heimservern abschließen
search-action-identity-home-peer-retry-subtitle = Entfernten Heimservern, die vorher nicht erreichbar waren, das Ende mitteilen
search-action-identity-device-pair-resource = Geräte in der Nähe koppeln
search-action-identity-device-pair-discover = Gerät in der Nähe zum Koppeln suchen
search-action-identity-device-pair-discover-subtitle = In diesem Netzwerk nach einem neuen Gerät oder Heimserver suchen, das beitreten möchte
search-action-identity-device-pair-approve = Gerät in der Nähe bestätigen
search-action-identity-device-pair-approve-subtitle = Das gefundene Gerät beitreten lassen, sobald beide Bildschirme denselben Code zeigen
search-action-identity-device-pair-reject = Gerät in der Nähe ablehnen
search-action-identity-device-pair-reject-subtitle = Ein gefundenes Gerät vergessen und seine Bestätigung abbrechen
search-action-identity-device-pair-asking = Geräte, die als du beitreten möchten
search-action-identity-device-pair-asking-subtitle = Neue Geräte, die deine anderen Geräte gefunden haben und hier auf deine Bestätigung warten
search-action-identity-device-pair-answer = Einem Gerät antworten, das als du beitreten möchte
search-action-identity-device-pair-answer-subtitle = Ein Gerät, das ein anderes deiner Geräte gefunden hat, bestätigen oder ablehnen, wie in seiner Mitteilung

# Ein neues Gerät, das ein anderes deiner Geräte gefunden hat: die Mitteilung,
# die hier die Person fragt.
pairing-asks-title = Ein Gerät möchte du sein
pairing-asks-body = { $device }{ $from }, { $when }. Wenn du bestätigst, handelt es als du. Bestätige nur, wenn es { $code } zeigt.
pairing-asks-via = {" "}(gefunden von deinem Gerät { $device })
pairing-approve = { $code } bestätigen
pairing-decline = Ablehnen

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
workspace-settings-emoji-too-large = Das Bild ist zu groß, um ein Emoji zu werden. Wähle eines unter 8 MB.
workspace-settings-emoji-unreadable = Das Bild konnte nicht gelesen werden.
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
workspace-settings-members-count = { $count ->
    [one] 1 Person teilt diesen Arbeitsbereich
   *[other] { $count } Personen teilen diesen Arbeitsbereich
}
workspace-settings-members-help = Wer entfernt wird, erhält keine Synchronisierung und Änderungen mehr; bereits heruntergeladene Kopien bleiben bei der Person.
workspace-settings-you-manage = Von dir verwaltet
workspace-settings-view-only = Nur ansehen
workspace-settings-members-empty-title = Sonst ist niemand in diesem Arbeitsbereich
workspace-settings-members-empty-detail = Erstelle im Tab „Allgemein“ eine signierte Einladung, um jemanden hinzuzufügen.
workspace-settings-people-header = Personen
workspace-settings-you = Du
workspace-settings-member-extra = { $count ->
    [one] 1 zusätzliche Berechtigung
   *[other] { $count } zusätzliche Berechtigungen
}
workspace-settings-remove = Entfernen
workspace-settings-owner-only = Nur der Eigentümer des Arbeitsbereichs kann Berechtigungen ändern.
workspace-settings-refused = Der Arbeitsbereich hat die Änderung abgelehnt
workspace-settings-refused-own-role = Niemand ändert die eigene Rolle.
workspace-settings-refused-own-membership = Niemand entfernt sich selbst aus dem Arbeitsbereich.
workspace-settings-agents-count = { $count ->
    [one] 1 Agent
   *[other] { $count } Agenten
}
workspace-settings-agents-help = Agenten arbeiten hier mit dir. Sie tun nur, was du ihnen erlaubst.
workspace-settings-add-agent = Neuer Agent
workspace-settings-create-agent = Erstellen
workspace-settings-cancel = Abbrechen
workspace-settings-bots-help = Bots führen hier kleine Programme von selbst aus: nach Zeitplan, wenn sich etwas ändert, oder als Brücke zu einem Dienst wie Discord.
workspace-settings-agents-empty-title = Noch keine Agenten
workspace-settings-agents-empty-detail = Ein Agent arbeitet hier mit dir, mit dem Zugriff, den du ihm erlaubst.
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
workspace-settings-retire-agent = Entfernen
workspace-settings-agent-name = Name
workspace-settings-agent-colour = Farbe
workspace-settings-agent-colour-hint = Seine Farbe kommt von seinem Namen, bis du eine wählst.
workspace-settings-agent-model-default-choice = Dein Standardmodell
workspace-settings-agent-name-number = Agent { $number }
workspace-settings-agent-yours = Deiner
workspace-settings-agent-keeper-you = Von dir gehalten
workspace-settings-agent-keeper = Gehalten von { $name }
workspace-settings-agent-model-line = Modell: { $model }
workspace-settings-agents-unavailable = Die Agenten des Workspace lassen sich auf diesem Gerät nicht lesen
workspace-settings-agent-name-needed = Benenne den Agenten zuerst
workspace-settings-review-access = Zugriff prüfen
workspace-settings-agent-access-back = Alle Agenten
workspace-settings-agent-access-note = Lege fest, was dieser Agent in jeder App tun darf. Senden, Veröffentlichen und endgültiges Löschen fragen dich trotzdem zuerst, außer du hast für diese Aktion „Immer erlauben“ gewählt.
workspace-settings-agent-access-read = Lesen
workspace-settings-agent-access-change = Änderungen
workspace-settings-agent-access-outside = Wirkungen nach außen
workspace-settings-agent-access-set-here = Hier gesetzt
workspace-settings-agent-access-reset = Standard verwenden
workspace-settings-agent-access-always = Immer erlaubt · { $count }
workspace-settings-agent-exception = { $action } in { $app }
workspace-settings-agent-access-stop = Wieder fragen
workspace-settings-agent-model = Modell
workspace-settings-agent-model-default = Der Standard: { $model }
workspace-settings-agent-model-none = Kein Modell in den Einstellungen gewählt
workspace-settings-agent-model-use = Verwenden
workspace-settings-agent-model-option = { $model } von { $provider }
workspace-settings-agent-model-in-use = In Verwendung
workspace-settings-agent-model-keeper = Nur { $name } wählt das Modell, mit dem es läuft
workspace-settings-agent-model-keeper-you = Du wählst das Modell, mit dem es läuft
workspace-settings-agent-spend-of = { $spent } von { $limit } heute
workspace-settings-agent-spend = { $spent } heute
workspace-settings-agent-limit = Tageslimit
workspace-settings-agent-limit-amount = { $limit } pro Tag
workspace-settings-agent-limit-none = Kein Limit
workspace-settings-agent-limit-today = { $spent } heute
workspace-settings-agent-limit-keeper-you = Nur du legst fest, was er pro Tag ausgeben darf. Dort hält er an und sagt es dir, wenn ein Lauf es erreicht.
workspace-settings-agent-limit-keeper = Nur { $name } legt fest, was er pro Tag ausgeben darf.
workspace-settings-agent-limit-placeholder = Dollar pro Tag
workspace-settings-agent-limit-set = Setzen
workspace-settings-agent-limit-not-amount = Ein Limit ist ein Dollarbetrag, etwa 5 oder 0.50
workspace-settings-agent-access-empty = Noch keine Apps in diesem Arbeitsbereich.
workspace-settings-answer-allow = Erlauben
workspace-settings-answer-ask = Erst fragen
workspace-settings-answer-deny = Verbieten
workspace-settings-revoke = Entziehen
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
workspace-settings-name-label = Name des Arbeitsbereichs
workspace-settings-rename-admin-only = Nur Administratoren des Arbeitsbereichs können ihn umbenennen.
workspace-settings-invite-not-allowed = Nur wer hier Personen einladen darf, kann einen Link erstellen.
workspace-settings-invite-create = Einladung erstellen
workspace-settings-devices-title = Deine Geräte
workspace-settings-devices-local = Fenster, Gruppen und Leisten des Arbeitsbereichs teilen alle, die darin sind. Wähle, welche deiner Geräte ihn zeigen und wo er auf diesem steht.
workspace-settings-device-here = { $device } (dieses Gerät)
workspace-settings-device-this = Dieses Gerät
workspace-settings-device-unnamed = Eines deiner Geräte
workspace-settings-device-shown = Angezeigt
workspace-settings-device-hidden = Ausgeblendet
workspace-settings-move-earlier = Nach oben
workspace-settings-move-later = Nach unten
workspace-settings-leave = Arbeitsbereich verlassen
workspace-settings-leave-description = Nimmt ihn mit den Fenstern und Gruppen, die dazugehören, von diesem Gerät. Für alle, die darin sind, bleibt er bestehen.
workspace-settings-leave-action = Verlassen
workspace-settings-leave-sole-member = Du bist das einzige Mitglied: Es gibt niemanden, dem er bleiben könnte.
workspace-settings-leave-last = Das ist dein einziger Arbeitsbereich hier.
workspace-settings-delete = Arbeitsbereich löschen
workspace-settings-delete-description = Alles darin geht verloren: seine Fenster, seine Gruppen, seine Geschichte. Er verschwindet von jedem Gerät, auf dem er ist.
workspace-settings-delete-action = Löschen
workspace-settings-delete-not-held = Dieses Gerät hat ihn nicht.
workspace-settings-delete-others = Es sind noch andere darin.
workspace-settings-delete-elsewhere = Er ist auch auf einem anderen deiner Geräte: lösche ihn dort.
workspace-settings-delete-not-owner = Er gehört jemand anderem.
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
a11y-shell-stop-following = { $name } nicht mehr folgen
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
people-panel-unpin = Zurück in die Leiste
a11y-people-unpin = Wer hier ist, zurück in die Leiste
people-panel-screen-private = Behält für sich, wo sie gerade sind
people-panel-nothing-open = Auf ihrem Bildschirm ist nichts offen
people-panel-pin = Auf dem Bildschirm halten
a11y-people-pin = Wer hier ist, auf dem Bildschirm halten
a11y-people-overlay = Wer hier ist
a11y-people-follow = { $name } folgen
a11y-people-stop-following = { $name } nicht mehr folgen
launchpad-subtitle = Wähle, wie du anfängst. Alle in diesem Arbeitsbereich sehen, was du hier machst.
launchpad-subtitle-private = Wähle, wie du anfängst. Nur du siehst, was du hier machst, bis du jemanden einlädst.
launchpad-subtitle-public = Wähle, wie du anfängst. Dieser Arbeitsbereich ist öffentlich: Alle, die beitreten, sehen, was du hier machst.

# Der Fensterwechsler (Alt+Tab).
window-switcher-title = Fenster wechseln
window-switcher-description = Fenster in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke Tab oder die Pfeiltasten zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
window-switcher-app-title = Fenster dieser App wechseln
window-switcher-app-description = Fenster dieser App in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke ` zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
window-switcher-untitled = Unbenanntes Fenster
window-switcher-caption-paged = { $position } von { $count }
window-switcher-hint-walk = Weiter
window-switcher-hint-go = Dorthin
window-switcher-hint-stay = Bleiben

# Workspace overview
overview-empty-window = Leeres Fenster
overview-untitled = Ohne Titel
overview-access-only-you = Nur du
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
browser-welcome-hint = Pfeiltasten bewegen, Eingabe öffnet

# The workspace overview every desktop shows
overview-title = Arbeitsbereichsübersicht
overview-strip-empty = Keine Fenster
overview-go-to = Wechseln zu
overview-no-workspace = Kein Arbeitsbereich ausgewählt
overview-you-are-here = Du bist hier
overview-shared-out = Von hier aus geteilt
overview-more-shares = und { $count } weitere
overview-share-with-two = { $first } und { $second }
overview-share-with-many = { $count ->
        [one] { $first } und { $count } weitere Person
       *[other] { $first } und { $count } weitere Personen
    }
overview-share-can-edit = { $with } darf bearbeiten
overview-new-workspace = Neuer Arbeitsbereich
overview-join-workspace = Einem Arbeitsbereich beitreten…
overview-leave-workspace = Verlassen
overview-a11y-leave = { $name } verlassen
overview-two-facts = { $first }, { $second }
overview-tilo-more-workspaces = Weitere Workspaces
overview-tilo-hours = { $hours } Std.
overview-tilo-hosts = Hosts & Rechenleistung
overview-tilo-agents = Agenten
overview-tilo-account = Konto
overview-tilo-about = Über Tilo
overview-tilo-workspace-usage = { $host }, { $time } diesen Monat
overview-tilo-plan-used-of = { $used } von { $included } Std.
overview-tilo-plan-used = { $used }
overview-tilo-credit = { $amount } Guthaben
overview-tilo-no-card = Noch keine Karte: wird erst gebraucht, wenn etwas eine Linux-Maschine braucht
overview-tilo-no-plan = Kein Tarif gewählt
overview-tilo-host-device = Dieses Gerät
overview-tilo-host-self = Dein eigener Host
overview-tilo-host-provider = Hosting-Anbieter
overview-tilo-manage-billing = Abrechnung verwalten
overview-tilo-agent-everywhere = Alle deine Workspaces
overview-tilo-agent-everywhere-lower = alle deine Workspaces
overview-tilo-agent-browser-pages = Bedient Webseiten im Browser
overview-tilo-agent-working = { $runs ->
        [one] Arbeitet gerade
       *[other] { $runs } Läufe gerade
    }
overview-tilo-revoke = Widerrufen
overview-tilo-revoke-question = { $agent } verliert den Zugriff auf { $reach } und hört sofort auf
overview-tilo-revoke-access = Zugriff entziehen
overview-tilo-keep = Behalten
overview-windows-title = Fenster in diesem Arbeitsbereich
overview-windows-description = Jede Leiste dieses Arbeitsbereichs von oben nach unten, mit ihren Fenstern der Reihe nach. Die Pfeiltasten wechseln zwischen ihnen, Enter öffnet eines. F2 benennt die Leiste eines Fensters um, Alt mit Oben oder Unten verschiebt diese Leiste. Klicke oder tippe auf ein Fenster, um dorthin zu wechseln, klicke eines mit der mittleren Taste an, um es zu schließen, oder ziehe es auf einen anderen Arbeitsbereich oder eine andere Leiste, um es dorthin zu verschieben.
overview-strips-title = Leisten in diesem Arbeitsbereich
overview-strip-rename = Umbenennen
overview-strip-move-up = Nach oben
overview-strip-move-down = Nach unten
browser-new-workspace-name = Arbeitsbereich { $number }
workspace-notice-joined = { $name } ist dazugekommen
workspace-notice-agent-finished = { $agent } ist fertig
workspace-notice-agent-card = Karte öffnen
workspace-notice-agent-finished-changes = { $agent } ist fertig: { $count ->
    [one] 1 Änderung
   *[other] { $count } Änderungen
}
workspace-notice-agent-finished-in = { $agent } ist fertig: { $count ->
    [one] 1 Änderung
   *[other] { $count } Änderungen
} in { $workspace }
workspace-notice-renamed = { $name } hat den Arbeitsbereich in { $workspace } umbenannt
# Was der Schreibtisch sagt, wenn etwas nicht geschehen ist, worum jemand
# gebeten hat: wonach gefragt wurde, warum, und die Taste, die es erneut versucht.
answer-app-not-opened = { $app } konnte nicht geöffnet werden
answer-app-stopped = { $app } funktionierte nicht mehr
answer-why-stopped = Es wurde unerwartet beendet.
answer-app-closed-for-memory = { $app } wurde geschlossen, um Arbeitsspeicher freizugeben
answer-app-closed-for-runaway = { $app } hat zu viel Arbeitsspeicher verbraucht
answer-why-runaway = Es wurde geschlossen, damit der Rest weiterläuft.
answer-try-again = Erneut versuchen
answer-reopen = Wieder öffnen
answer-why-unknown-app = Es gibt keine App, die das öffnet.
answer-why-not-opened = Es wurde beendet, bevor ein Fenster erschien.
answer-why-cannot-start = Apps können gerade nicht gestartet werden.
peer-window-elsewhere = Auf einem anderen Gerät geöffnet. Teile das Fenster dort, um es auf diesem zu sehen.
# Ein Kürzel des Desktops, für das eine Browserseite keine Oberfläche hat: sie
# sagt das, statt nichts zu tun.
answer-capture-not-in-browser = Bildschirmfotos und Aufnahmen gibt es in Tilo auf einem Computer
answer-why-capture-not-in-browser = Dafür braucht es den ganzen Bildschirm, den eine Browserseite nicht hat.
answer-remote-not-in-browser = Fernsteuerung gibt es in Tilo auf einem Computer
answer-why-remote-not-in-browser = Hier steht nichts unter der Kontrolle von jemand anderem.

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
notification-alarm-dismiss = Verwerfen
notification-alarm-snooze = In 5 Minuten erneut erinnern
notification-sender-claims = App sagt: { $name }
notification-open = Öffnen
notification-quiet-do-not-disturb = Nicht stören
notification-quiet-presenting = Nicht stören beim Präsentieren
notification-quiet-sharing = Nicht stören beim Teilen
notification-quiet-focus-until = Fokus bis { $time }

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
feedback-part-in = { $what } in { $app }
feedback-part-named = „{ $name }“ ({ $role ->
    [Button] Schaltfläche
    [ToggleButton] Schaltfläche
    [Link] Link
    [Tab] Tab
    [MenuItem] Menüeintrag
    [Checkbox] Kästchen
    [Switch] Schalter
    [TextInput] Feld
    [MultilineTextInput] Feld
    [Heading] Überschrift
   *[other] { $role }
})
feedback-part-role = { $role ->
    [ListItem] Ein Listeneintrag
    [Row] Eine Zeile
    [Cell] Eine Zelle
    [Button] Eine Schaltfläche
    [ToggleButton] Eine Schaltfläche
    [Link] Ein Link
    [Image] Ein Bild
    [TextInput] Ein Feld
    [MultilineTextInput] Ein Feld
    [StaticText] Ein Text
    [Paragraph] Ein Absatz
    [Heading] Eine Überschrift
    [List] Eine Liste
    [Group] Eine Gruppe
    [Window] Ein Fenster
    [Toolbar] Eine Werkzeugleiste
    [Menu] Ein Menü
    [MenuItem] Ein Menüeintrag
    [Tab] Ein Tab
   *[other] Ein Teil
}

# Der Ansicht einer Person folgen
follow-label = Du folgst { $person }
follow-stopped = Folgen beendet
follow-stop = Beenden

people-follows = Folgt { $person }
people-follows-you = Folgt dir
people-follows-where = { $place }, folgt { $person }
people-follows-you-where = { $place }, folgt dir

# The way out of a window that filled the display, said over it once.
fullscreen-way-out = { $key } beendet das Vollbild.

## The desktop's published actions
desktop-actions-chrome = Desktop
desktop-actions-desk = Fenster
desktop-actions-launchpad = Startfeld
desk-focus-window = { $window } fokussieren
desk-close-window = { $window } schließen
desk-look-window = { $window } als Bild ansehen
desk-read-window = Lesen, was { $window } zeigt
session-host-new-workspace = Neuer Arbeitsbereich
session-host-new-workspace-description = Auf diesem Rechner anlegen
session-host-branch-workspace = Arbeitsbereich abzweigen
session-host-branch-workspace-description = Eine Kopie dieses Bereichs, die ihren eigenen Weg geht
desktop-branch-workspace-name = { $name } Abzweig
desktop-cannot-branch-workspace = Dieser Arbeitsbereich enthält, was du nicht lesen kannst, darum kann er hier nicht abgezweigt werden
session-branching-workspace = { $name } wird abgezweigt
session-branching-workspace-body = { $branch } öffnet sich, sobald seine Einträge, Dateien und Umgebungen kopiert sind.
session-branched-workspace = { $branch } ist bereit
session-branched-workspace-body = Er enthält, was { $name } enthält, mit dir als einzigem Mitglied. Sein Chat ist geblieben.
session-branch-failed = { $branch } ist nicht vollständig
session-host-join-workspace = Arbeitsbereich beitreten
session-host-join-workspace-description = Aus seinem Verzeichnis
session-host-leave-workspace = Arbeitsbereich verlassen
session-host-leave-workspace-description = Von diesem Rechner entfernen
session-host-workspace-settings = Arbeitsbereich-Einstellungen
session-host-workspace-settings-description = Sein Name und wer darin ist
session-host-lock = Sperren
session-host-log-out = Abmelden
session-host-restart = Neu starten
session-host-shut-down = Ausschalten
session-host-report-problem = Ein Problem melden
session-host-report-problem-description = Schick Tilo, was nicht funktioniert
session-feedback-destination = Tilo-Feedback
session-left-workspace = Du hast { $name } verlassen
session-left-workspace-body = Seine Fenster sind von diesem Rechner verschwunden. Du kannst ihm über das Verzeichnis wieder beitreten.
session-deleted-workspace = { $name } ist gelöscht
session-deleted-workspace-body = Er ist von diesem Rechner und aus deiner Liste der Arbeitsbereiche verschwunden.
session-owner-deleted-workspace = { $name } wurde von seinem Besitzer gelöscht
session-owner-deleted-workspace-body = Er ist von diesem Rechner verschwunden, und nichts darin lässt sich mehr öffnen.
session-joined-workspace = Du bist { $name } beigetreten
session-joined-workspace-body = Es ist jetzt geöffnet, mit allem, was seine Leute darin halten.
session-join-failed-title = Arbeitsbereich konnte nicht beigetreten werden
desktop-no-workspace = Es gibt keinen Arbeitsbereich namens { $name }
desktop-joined-workspace-unknown = Dieser Arbeitsbereich liegt in einem Arbeitsbereich, dem dieser Rechner nicht beitreten kann
desk-first-workspace = Das ist der erste Arbeitsbereich
desk-last-workspace = Das ist der letzte Arbeitsbereich
desk-first-strip = Das ist die erste Leiste
desk-last-strip = Das ist die letzte Leiste
desk-window-edge = Nichts geändert. Das Fenster kann nicht weiter in diese Richtung.
desktop-no-strip = Hier gibt es keine Leiste namens { $name }
desktop-strip-not-here = Diese Leiste ist nicht mehr in diesem Arbeitsbereich
desktop-workspace-gone = Diesen Arbeitsbereich gibt es hier nicht mehr
desktop-no-workspace-to-leave = Es gibt keinen Arbeitsbereich zu verlassen
desktop-no-workspace-to-delete = Es gibt keinen Arbeitsbereich zu löschen
desktop-only-workspace = Das ist der einzige Arbeitsbereich hier
desktop-workspace-stays = Dieser Arbeitsbereich bleibt: Jemand anderes ist darin, oder ein anderes deiner Geräte hält ihn.
desktop-cannot-delete-workspace = Dieser Desktop kann keine Arbeitsbereiche löschen
desktop-window-not-here = Dieses Fenster ist nicht mehr auf diesem Desktop
desktop-nothing-to-move-to = Es gibt kein Ziel für dieses Fenster
desktop-move-no-record = Ein Fenster zu verschieben braucht keinen Eintrag
desktop-actions-search = Suche
session-input = Eingabegeräte
session-toggle-touchpad = Touchpad ein- oder ausschalten
session-param-touchpad-on = Ob das Touchpad an ist; weglassen zum Umschalten
session-next-input-source = In der nächsten Tastatureingabequelle tippen
session-screen = Bildschirm
session-lock = Bildschirm sperren
session-log-out = Abmelden
session-magnifier = Lupe
session-magnifier-toggle = Lupe ein- oder ausschalten
session-magnifier-zoom-in = Stärker vergrößern
session-magnifier-zoom-out = Weniger vergrößern
session-displays = Bildschirme
session-displays-next = Nächste Bildschirmanordnung
session-displays-set = Bildschirme anordnen
session-clipboard = Zwischenablage
session-clipboard-paste-files = Dateien hier einfügen
session-param-paste-destination = Der Ordner, in den die kopierten Dateien eingefügt werden

session-window-share = Geteilte Fenster
session-window-share-share = Ein Fenster mit dem Raum teilen
session-window-share-param-window = Das Fenster, nach der Kennung, die seine App erfassen darf
session-window-share-param-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-param-app = Die App, zu der das Fenster gehört
session-window-share-param-workspace = Der Arbeitsbereich, in dem es geteilt wird
session-window-share-stop = Das Teilen des Fensters beenden
session-window-share-allow-input = Den Raum das Fenster bedienen lassen
session-window-share-param-on = Ob sie es bedienen dürfen; weglassen zum Umschalten
session-window-share-watch = Ein Fenster zeigen, das ein anderes Mitglied teilt
session-window-share-unwatch = Es nicht mehr zeigen
session-window-share-workspace-left = Diese Sitzung ist nicht mehr im Arbeitsbereich

## The on-screen readout for the hardware keys.
desktop-actions-osd = Bildschirmanzeige
desktop-action-show-osd = Eine Anzeige zeigen
desktop-action-param-osd-kind = Worum es geht: volume, brightness, keyboard-backlight, microphone, airplane-mode, touchpad, play, pause, next-track, previous-track, stop, no-backlight, night-light, do-not-disturb, magnifier, captured-keys oder pointer-released
desktop-action-param-osd-level = Die Stufe, von 0 bis 1
desktop-action-param-osd-on = Ob es an ist
osd-play = Wiedergabe
osd-pause = Pause
osd-play-named = Spielt { $song }
osd-pause-named = Pausiert { $song }
osd-next-track = Nächster Titel
osd-previous-track = Vorheriger Titel
osd-stop = Stopp
osd-muted = Stummgeschaltet
osd-percent = { $percent } %
osd-off = Aus
osd-volume = Lautstärke { $percent } %
osd-brightness = Helligkeit { $percent } %
osd-no-backlight = Kein regelbarer Bildschirm
osd-microphone-muted = Mikrofon stummgeschaltet
osd-microphone-on = Mikrofon an
osd-keyboard-backlight-off = Tastaturbeleuchtung aus
osd-keyboard-backlight = Tastaturbeleuchtung { $percent } %
osd-airplane-mode-on = Flugmodus an
osd-airplane-mode-off = Flugmodus aus
osd-touchpad-on = Touchpad an
osd-touchpad-off = Touchpad aus
osd-volume-name = Lautstärke
osd-brightness-name = Helligkeit
osd-keyboard-backlight-name = Tastaturbeleuchtung
osd-readout = Anzeige
osd-reading = { $device }, { $value }
osd-night-light-on = Nachtlicht an
osd-night-light-off = Nachtlicht aus
osd-do-not-disturb-on = Nicht stören an
osd-do-not-disturb-off = Nicht stören aus
osd-magnifier-on = Lupe an
osd-magnifier-off = Lupe aus
osd-keys-captured = Tasten übernommen
osd-input-source = Eingabequelle
osd-pointer-released = Zeiger freigegeben
desktop-action-param-osd-device = Das geänderte Gerät, wie eine Person es kennt
desktop-action-param-osd-device-glyph = Das Symbol des Geräts: speaker, headphones, display oder keyboard
desktop-action-param-osd-chord = Die Tastenkombination, die es ausgelöst hat
desktop-action-param-osd-display = Der Anschluss des Bildschirms, auf dem es erscheint
desktop-display-name = Bildschirm { $number }
# What a person can do to one window, from its own menu.
window-menu-reveal = Zu diesem Fenster gehen
window-menu-rename = Dieses Fenster benennen…
window-tab-name = Fenstername
window-menu-full-screen = Vollbild
window-menu-exit-full-screen = Vollbild beenden
window-menu-maximize = Den Streifen füllen
window-menu-restore = Den Streifen zurückgeben
window-menu-float = Dieses Fenster schweben lassen
window-menu-snap-back = Zurück in den Streifen
window-menu-move-to-strip = In eine Leiste verschieben
window-menu-move-to-workspace = In einen Arbeitsbereich verschieben
window-menu-move-to-display = Auf einen Bildschirm verschieben
window-menu-width = Breite
window-menu-width-half = Halb so breit wie die Leiste
window-menu-width-narrow = Schmal
window-menu-width-normal = Normal
window-menu-width-wide = Breit
window-menu-width-equal = Alle Fenster gleich
window-menu-wider = Breiter
window-menu-narrower = Schmaler
window-menu-widest = Breitest
window-menu-narrowest = Schmalst
window-menu-new-tab = Neuer Tab dieser App
window-menu-not-yours-to-open = Nur zusehen
window-menu-no-app = Keine App hier
window-menu-split-right = Nach rechts teilen
window-menu-split-one-tab = Nichts zu teilen: Dieses Fenster hat nur einen Tab
window-menu-new-group = In ein neues Fenster verschieben
window-menu-close = Schließen
window-menu-close-window = Dieses Fenster schließen
window-menu-stop-desktop-control = Die Steuerung des Desktops beenden
window-menu-someone-elses = { $name }s Fenster
# The desk's widgets: what each is called, and what its controls do.
desk-widget-music = Musik
desk-widget-team = Menschen hier
desk-widget-calendar = Kalender
desk-widget-system-monitor = Systemmonitor
desk-widget-pin = Dieses Widget immer in diesem Arbeitsbereich anzeigen
desk-widget-unpin = Dieses Widget nicht mehr immer anzeigen
desk-widget-open = { $widget } öffnen
desk-widget-collapse = { $widget } einklappen
desk-widget-remove = { $widget } entfernen
desk-widget-removed = { $widget } vom Desktop entfernt
desk-widget-undo = Rückgängig
desk-team-title = In diesem Arbeitsbereich
desk-team-more = +{ $count } weitere
desk-monitor-title = Dieses Gerät
desk-monitor-cpu = Prozessor
desk-monitor-memory = Arbeitsspeicher
desk-monitor-temperature = Temperatur
desk-monitor-cpu-short = CPU
desk-monitor-memory-short = RAM
desk-widget-resize = { $widget } vergrößern

hung-app-title = Antwortet nicht
hung-app-card = { $window } antwortet nicht
hung-app-wait = Warten
hung-app-force-quit = Erzwungen beenden
desk-wait-window = Weiter auf { $window } warten
desk-force-quit-window = { $window } erzwungen beenden
activities-layer = Aktivitäten
activity-collapse = Weglegen
activity-close = Schließen

profile-agent-card = Karte öffnen
profile-agent-stop = Anhalten
profile-agent-undo-run = Lauf rückgängig machen
profile-agent-take-over = Übernehmen
profile-agent-revoke = Zugriff entziehen
profile-agent-now = Gerade
task-programs-resource = Aufgabenprogramme
task-programs-run = Ein Programm ausführen
task-programs-run-detail = Führt ein tilolang-Programm als Schritt des Laufs der Aufgabe aus, als der Agent, der an der Aufgabe arbeitet, und antwortet, was es gelesen oder geändert hat.
