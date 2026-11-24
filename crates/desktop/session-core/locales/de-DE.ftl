# Was der Schreibtisch nennt, was niemand benannt hat
shell-unnamed-workspace = Arbeitsbereich
shell-unnamed-strip = Leiste { $number }

# Obere Leiste
continuity-dismiss = Ausblenden
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
# Jemand wollte ein geteiltes Fenster sehen, und nichts davon ist bisher hinausgegangen: Er wartet
# und sieht erst zu oder steuert, wenn etwas hinausgegangen ist.
topbar-sharing-agent-waiting = Agent wartet: { $name }
topbar-sharing-agent-waiting-control = Agent wartet, darf steuern: { $name }
topbar-sharing-person-waiting = Wartet: { $name }
topbar-sharing-person-waiting-control = Wartet, darf steuern: { $name }
topbar-sharing-session-waiting = Wartet: { $name }
topbar-sharing-session-waiting-control = Wartet, darf steuern: { $name }
topbar-sharing-count-waiting = { $waiting } { $waiting ->
        [one] wartet
       *[other] warten
    }
topbar-sharing-count-waiting-control = { $waiting } { $waiting ->
        [one] wartet
       *[other] warten
    }, { $controllers } { $controllers ->
        [one] darf
       *[other] dürfen
    } steuern
topbar-sharing-count-mixed = { $viewers } { $viewers ->
        [one] sieht
       *[other] sehen
    } zu, { $waiting } { $waiting ->
        [one] wartet
       *[other] warten
    }
topbar-sharing-count-mixed-controlling = { $viewers } { $viewers ->
        [one] sieht
       *[other] sehen
    } zu, { $controllers } { $controllers ->
        [one] steuert
       *[other] steuern
    }, { $waiting } { $waiting ->
        [one] wartet
       *[other] warten
    }
topbar-sharing-unknown-window = ein Fenster
# Wer ein geteiltes Fenster sieht, wenn nichts ihn benennt, nach „Sieht zu:“ oder „Steuert:“.
topbar-sharing-unnamed-viewer = ein anderes Gerät
topbar-sharing-window-in-workspace = { $window } in { $workspace }
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
topbar-show-people = Alle hier
topbar-viewer-can-control = kann { $targets } steuern
topbar-viewer-view-only = sieht nur { $targets }
topbar-viewer-waiting = wartet auf { $targets }
topbar-viewer-waiting-control = wartet auf { $targets }, Steuern erlaubt
# Wer ein geteiltes Fenster sieht, wie ein Screenreader es sagt: was der Arbeitsbereich über ihn
# sagt, der Name aus der oberen Leiste, wenn nur der bekannt ist, und ohne Namen eben das.
topbar-viewer-entry = { $kind ->
        [agent] { $name } (Agent)
        [person] { $name } (Person)
        [unnamed] ein Gerät ohne Namen
       *[named] { $name }
    }: { $access }
topbar-people-visible = Bildschirm sichtbar für { $viewers }. Zeigen, wer hier ist
topbar-people-waiting = Noch nichts gesendet an { $viewers }. Zeigen, wer hier ist
topbar-people-visible-and-waiting = Bildschirm sichtbar für { $viewers }. Noch nichts gesendet an { $waiting }. Zeigen, wer hier ist
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
topbar-workspace-menu = Arbeitsbereich wechseln, jetzt in { $workspace }
topbar-profile = { $name } (du): dein Profil
topbar-face-agent = { $name }, Agent
topbar-faces-more = { $count } weitere
workspace-menu-new-strip = Neue Leiste
workspace-menu-settings = Arbeitsbereichseinstellungen
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
activity-view-tasks = Aufgaben
activity-title-whats-new = Neues
activity-title-explore = Arbeitsbereich-Aktivität
activity-title-replay = Arbeitsbereich wiedergeben
activity-source-workspace = Arbeitsbereich
activity-scope-this-workspace = Dieser Arbeitsbereich
activity-scope-all-workspaces = Alle Arbeitsbereiche
activity-period-all-time = Gesamter Zeitraum
activity-period-today = Heute
activity-period-week = 7 Tage
activity-period-month = 30 Tage

activity-detail-restore = Wiederherstellen
activity-history-undo = Diese Änderung rückgängig machen
activity-history-restore = Diese Version wiederherstellen
activity-pane-details = Details
activity-history-gone = Diese Änderungen sind nicht mehr im Verlauf.
activity-served-personal-only = Das kann nur die Person an diesem Gerät.
activity-served-read-only = Der Verlauf lässt sich von hier aus nicht ändern.
activity-served-unavailable = Der Verlauf dieses Arbeitsbereichs wird hier nicht aufbewahrt.
activity-lens-everything = Alles
activity-lens-agent-tasks = Agent-Aufgaben  { $count }
activity-lens-unclassified = Nicht zugeordnet  { $count }
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
activity-no-resource = Keine Datei und kein Eintrag
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
activity-agent-task-people = Zugewiesen von { $issuer }, ausgeführt von { $agent }
activity-agent-fallback = Agent
activity-unknown-member = Unbekanntes Mitglied
activity-undo-task-needs-open = Öffne alle betroffenen Dokumente, um diese Aufgabe gemeinsam rückgängig zu machen.
activity-undo-run-needs-open = Öffne jedes Dokument, das dieser Lauf geändert hat, um ihn gemeinsam rückgängig zu machen.
activity-undo-run-in-progress = Das Rückgängigmachen dieses Laufs läuft noch.
activity-undo-run-nothing-to-undo = Dieser Lauf hat nichts geändert, was sich rückgängig machen lässt.
activity-undo-task-partial = Aufgabe teilweise rückgängig gemacht; spätere Bearbeitungen anderer wurden behalten.
activity-undo-task-partial-records = Aufgabe rückgängig gemacht, außer den unten genannten Einträgen.
activity-undo-task-record-left = { $record } in { $app } wurde nicht rückgängig gemacht.
activity-detail-undo-step = Diesen Schritt rückgängig
activity-undo-task-done = Ganze Aufgabe als neuer Verlaufseintrag rückgängig gemacht.
activity-undo-task-refused = Die Aufgabe konnte nicht rückgängig gemacht werden. { $detail }
activity-undo-task-unavailable = Aufgaben können hier nicht rückgängig gemacht werden.
activity-removed-workspace = ein entfernter Arbeitsbereich
activity-receipt-completed = Abgeschlossen
activity-receipt-failed = Fehlgeschlagen
activity-receipt-cancelled = Abgebrochen
activity-receipt-ended = { $status }: { $what }
activity-replay-layout-not-recorded = Fenstergrößen und ausgewählte Tabs wurden nicht aufgezeichnet
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
activity-event-membership = Mitglieder des Arbeitsbereichs geändert
activity-event-member-added = { $name } als { $role ->
        [viewer] Betrachter
        [commenter] Kommentierende Person
        [editor] Bearbeiter
        [moderator] Moderator
       *[owner] Eigentümer
    } hinzugefügt
activity-event-member-joined = { $name } über eine Einladung hinzugefügt
activity-event-member-removed = { $name } aus dem Arbeitsbereich entfernt
activity-event-member-role = { $name } zu { $role ->
        [viewer] Betrachter
        [commenter] Kommentierender Person
        [editor] Bearbeiter
        [moderator] Moderator
       *[owner] Eigentümer
    } gemacht
activity-event-members-added = { $names } zum Arbeitsbereich hinzugefügt
activity-event-member-unknown = ein Mitglied
activity-event-device-added = Gerät „{ $name }“ hinzugefügt
activity-event-device-added-unnamed = Ein Gerät hinzugefügt
activity-event-devices-added = { $count } { $count ->
        [one] Gerät
       *[other] Geräte
    } hinzugefügt
activity-fold-added = Hinzugefügt:
activity-fold-added-devices = Geräte hinzugefügt:
activity-fold-opened = Geöffnet:
activity-fold-device = „{ $name }“
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
    } für { $scope } an { $who }
activity-capability-unknown = eine Berechtigung
activity-capability-this-workspace = diesen Arbeitsbereich
activity-capability-one-shared-file = eine geteilte Datei
activity-capability-network = Netzwerk { $host }
activity-capability-agent = Agent { $agent }
activity-capability-bot = Bot { $bot }
activity-capability-through-app = { $who } in { $app }

# Schreibtischflächen
desktop-system-status = Systemstatus
desktop-app-unavailable = Anwendung nicht verfügbar
desktop-app-identity-unavailable = Anwendungsidentität nicht verfügbar
desktop-app-integration-unavailable = { $app } lässt sich auf diesem Rechner nicht öffnen
desktop-app-no-adapter = Das Fenster dieser App lässt sich hier nicht zeigen
desktop-search-copied = Kopiert
desktop-search-searching = Suche läuft…

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
a11y-notifications-clear-description = Löscht alle Benachrichtigungen. Acht Sekunden lang kannst du das rückgängig machen.
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
host-action-source-identity = Identität
host-action-source-devices = Geräte
host-action-source-workspace = Arbeitsbereich
action-source-desktop = Dein Desktop
host-action-source-session = Dieser Computer
notification-center-group-description = { $count } Benachrichtigungen. Eingabe zum { $expanded ->
        [yes] Einklappen
       *[no] Ausklappen
    }, Entf, um alle zu verwerfen.
notification-center-dismiss-all = Alle von { $app } verwerfen
notification-reply = Antworten
notification-center-dismiss = Verwerfen
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


# Workspace settings
workspace-settings-title = Arbeitsbereichseinstellungen
workspace-settings-close = Schließen
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
workspace-settings-role-admin = Admin
workspace-settings-role-guest = Gast
workspace-settings-role-owner-preview = Kann Personen, Berechtigungen und Einstellungen des Arbeitsbereichs verwalten.
workspace-settings-role-moderator-preview = Kann geteilte Arbeit bearbeiten, Nachrichten anderer entfernen, Kanäle sperren und Mitglieder entfernen oder sperren.
workspace-settings-role-editor-preview = Kann geteilte Arbeit ansehen, kommentieren und bearbeiten.
workspace-settings-role-commenter-preview = Kann geteilte Arbeit ansehen und kommentieren, aber nicht bearbeiten.
workspace-settings-role-viewer-preview = Kann geteilte Arbeit ansehen, aber weder kommentieren noch bearbeiten.
workspace-settings-role-administrator = Administrator
workspace-settings-role-administrator-preview = Kann hier alles: geteilte Arbeit bearbeiten, Leute einladen und entfernen, weitere Administratoren ernennen und Namen und Einstellungen des Arbeitsbereichs ändern.
workspace-settings-scope-workspace = dieser Arbeitsbereich
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
workspace-settings-allows-edit = { $what } bearbeiten
workspace-settings-allows-view = { $what } ansehen
workspace-settings-allows-action = „{ $action }“ in { $app } verwenden
workspace-settings-granted-by-person = erteilt von { $id }
workspace-settings-granted-by-you = von dir erteilt
workspace-settings-granted-by-agent = erteilt von Agent { $id }
workspace-settings-granted-by-bot = erteilt von Bot { $id }
workspace-settings-permission-detail = { $allows }, { $grantor }
workspace-settings-section-apps = Apps
workspace-settings-section-people = Personen
workspace-settings-section-agents = Agenten
workspace-settings-section-roles = Rollen
workspace-settings-subject-role = alle mit der Rolle { $role }
workspace-settings-grant-network = Netzwerkzugriff
workspace-settings-grant-read-files = Dateien des Arbeitsbereichs lesen
workspace-settings-grant-edit-files = Dateien des Arbeitsbereichs bearbeiten
workspace-settings-members-count = { $count ->
    [one] 1 Person teilt diesen Arbeitsbereich
   *[other] { $count } Personen teilen diesen Arbeitsbereich
}
workspace-settings-members-help = Wer entfernt wird, erhält keine Änderungen und Aktualisierungen mehr; bereits heruntergeladene Kopien bleiben auf den Geräten der Person.
workspace-settings-you-manage = Von dir verwaltet
workspace-settings-view-only = Nur ansehen
workspace-settings-members-empty-title = Sonst ist niemand in diesem Arbeitsbereich
workspace-settings-members-empty-detail = Erstelle im Tab „Allgemein“ eine signierte Einladung, um jemanden hinzuzufügen.
workspace-settings-people-header = Personen
workspace-settings-kept-header = Wo er liegt
workspace-settings-kept-reads = Liest den Arbeitsbereich
workspace-settings-kept-reads-cloud = Liest den Arbeitsbereich, und der Cloud-Anbieter auch
workspace-settings-kept-stores = Hält eine verschlüsselte Kopie und liest nichts davon
workspace-settings-kept-unnamed = Ein Server
workspace-settings-delegated-header = An Server gegebene Arbeit
workspace-settings-delegated-detail = { $work }, übernommen von { $server }
workspace-settings-delegated-detail-agent = { $work }, übernommen von { $server } als { $agent }
workspace-settings-delegated-former-member = Ein ehemaliges Mitglied
workspace-settings-delegated-revoke-sheet-title = Diese Arbeit zurücknehmen?
workspace-settings-delegated-revoke-sheet-description = Der Server übernimmt deine Arbeit sofort nicht mehr.
workspace-settings-delegated-keep = Behalten
workspace-settings-delegated-revoke = Zurücknehmen
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
workspace-settings-agents-empty-title = Noch keine Agenten
workspace-settings-agents-empty-detail = Ein Agent arbeitet hier mit dir, mit dem Zugriff, den du ihm erlaubst.
workspace-settings-agents-header = Agenten
workspace-settings-retire-agent = Entfernen
workspace-settings-agent-name = Name
workspace-settings-agent-model-default-choice = Dein Standardmodell
workspace-settings-agent-name-number = Agent { $number }
workspace-settings-agent-added-by-you = Von dir hinzugefügt
workspace-settings-agent-added-by = Hinzugefügt von { $name }
workspace-settings-agent-model-line = Modell: { $model }
workspace-settings-agents-unavailable = Die Agenten des Workspace lassen sich auf diesem Gerät nicht lesen
workspace-settings-agent-name-needed = Benenne den Agenten zuerst
workspace-settings-review-access = Zugriff prüfen
workspace-settings-agent-access-back = Alle Agenten
workspace-settings-agent-access-note = In Apps sind Wirkungen nach außen auf „Erst fragen“ oder „Verbieten“ begrenzt; in deinem eigenen Konto darfst du sie erlauben.
workspace-settings-agent-access-read = Lesen
workspace-settings-agent-access-change = Änderungen
workspace-settings-agent-access-outside = Wirkungen nach außen
workspace-settings-agent-access-workspaces = Deine Arbeitsbereiche
workspace-settings-agent-access-workspaces-effects = Deine Arbeitsbereiche erstellen, verzweigen, betreten und löschen.
workspace-settings-agent-access-members = Mitglieder und Einstellungen
workspace-settings-agent-access-members-effects = Leute einladen und entfernen, ihre Rollen und Sperren, sowie die Einstellungen des Arbeitsbereichs.
workspace-settings-agent-access-set-here = Hier gesetzt
workspace-settings-agent-access-reset = Standard verwenden
workspace-settings-agent-access-always = { $count } immer erlaubt
workspace-settings-agent-exception = { $action } in { $app }
workspace-settings-agent-access-stop = Wieder fragen
workspace-settings-agent-model = Modell
workspace-settings-agent-model-default = Der Standard: { $model }
workspace-settings-agent-model-none = Kein Modell in den Einstellungen gewählt
workspace-settings-agent-model-use = Verwenden
workspace-settings-agent-model-option = { $model } von { $provider }
workspace-settings-agent-model-in-use = In Verwendung
workspace-settings-agent-model-view-only = Nur Mitglieder, die die Agenten des Arbeitsbereichs ändern dürfen, wählen sein Modell
workspace-settings-agent-spend-of = { $spent } von { $limit } heute
workspace-settings-agent-spend = { $spent } heute
workspace-settings-agent-limit = Tageslimit
workspace-settings-agent-limit-amount = { $limit } pro Tag
workspace-settings-agent-limit-none = Kein Limit
workspace-settings-agent-limit-today = { $spent } heute
workspace-settings-agent-limit-note = Dort hält er an und sagt dir Bescheid, wenn ein Lauf das Limit erreicht.
workspace-settings-agent-limit-view-only = Nur Mitglieder, die die Agenten des Arbeitsbereichs ändern dürfen, legen fest, was er pro Tag ausgeben darf.
workspace-settings-agent-limit-placeholder = Dollar pro Tag
workspace-settings-agent-limit-set = Setzen
workspace-settings-agent-limit-not-amount = Ein Limit ist ein Dollarbetrag, etwa 5 oder 0.50
workspace-settings-answer-allow = Erlauben
workspace-settings-answer-ask = Erst fragen
workspace-settings-answer-deny = Verbieten
workspace-settings-revoke = Zurücknehmen
workspace-settings-unavailable-app = Nicht verfügbare App
workspace-settings-apps-empty-title = Keine Dateiverknüpfungen installiert
workspace-settings-apps-empty-detail = Noch keine App gibt einen unterstützten Dateityp an.
workspace-settings-apps-intro-title = Standard-Apps
workspace-settings-apps-intro-detail = Jede Art von Datei öffnet in der App des Arbeitsbereichs, solange du keine eigene wählst.
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
workspace-settings-permissions-intro-title = Berechtigungen
workspace-settings-permissions-intro-detail = Jede Berechtigung sagt, wer was tun darf, und wo.
workspace-settings-active-grants = { $count ->
    [one] 1 aktive Freigabe
   *[other] { $count } aktive Freigaben
}
workspace-settings-permissions-empty-title = Keine Berechtigungen erteilt
workspace-settings-permissions-empty-detail = Apps erhalten nichts, bis es hier erteilt oder in einer Abfrage erlaubt wird.
workspace-settings-who = Wer
workspace-settings-who-agent = Wer: ein Agent, nur mit Zugriff auf dieses Fenster
workspace-settings-nobody-to-grant = Noch niemand, dem du etwas erteilen kannst. Lade jemanden ein oder öffne hier eine App.
workspace-settings-what-it-allows = Was es erlaubt
workspace-settings-grant-header = Berechtigung erteilen
workspace-settings-expiry-caption = Gültigkeit
workspace-settings-grant = Erteilen
workspace-settings-remove-sheet-title = Diese Person entfernen?
workspace-settings-remove-sheet-description = Die Person erhält keine Änderungen und Aktualisierungen mehr. Heruntergeladene Kopien bleiben auf ihren Geräten.
workspace-settings-revoke-sheet-title = Diese Berechtigung zurücknehmen?
workspace-settings-revoke-sheet-description = Sie gilt sofort nicht mehr. Prüfe zuerst ihre Aktivität, wenn du unsicher bist.
workspace-settings-keep-access = Zugriff behalten
workspace-settings-review-activity = Zuerst Aktivität prüfen
workspace-settings-remove-person = Person entfernen
workspace-settings-keep-permission = Berechtigung behalten
workspace-settings-revoke-permission = Berechtigung zurücknehmen
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
workspace-settings-servers-title = Server
workspace-settings-servers-description = Eine Änderung auf einem erreicht die anderen, und Menschen können den Arbeitsbereich von jedem aus öffnen.
workspace-settings-servers-origin-label = Adresse eines anderen Servers
workspace-settings-servers-add = Hinzufügen
workspace-settings-servers-remove = Entfernen
workspace-settings-servers-here = Dieser Server
workspace-settings-servers-connected = Hierüber bist du verbunden
workspace-settings-servers-unnamed = Ein weiterer Server
workspace-settings-leave = Arbeitsbereich verlassen
workspace-settings-leave-description = Nimmt ihn mit den Fenstern und Gruppen, die dazugehören, von diesem Gerät. Für alle, die darin sind, bleibt er bestehen.
workspace-settings-leave-membership = Du bist kein Mitglied mehr: Er verschwindet aus deinen Arbeitsbereichen und bleibt für alle anderen darin bestehen.
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

# Workspace settings: document access and roles
workspace-settings-access-all-apps = Alle Apps im Workspace
workspace-settings-access-app-default = { $app }: Standardzugriff
workspace-settings-create-role = Rolle erstellen
workspace-settings-custom-roles = Benutzerdefinierte Rollen
workspace-settings-delete-role = Diese Rolle löschen?
workspace-settings-delete-role-confirm = Rolle löschen
workspace-settings-delete-role-detail = Ihre Berechtigungen und Mitgliederzuweisungen werden entfernt.
workspace-settings-document-access = Datenzugriff
workspace-settings-document-apply = Zugriffsregel anwenden
workspace-settings-document-empty = Es gibt keine gemeinsamen Dokumente zu verwalten.
workspace-settings-document-everyone = Alle
workspace-settings-document-label = Gilt für
workspace-settings-keep-role = Rolle behalten
workspace-settings-rename-role = Umbenennen
workspace-settings-role-name = Rollenname
workspace-settings-save-role = Rolle speichern

# People popover
a11y-shell-people = Personen
a11y-shell-people-description = Wer hier ist, wo und woran gerade gearbeitet wird
a11y-shell-people-here-now = Jetzt anwesend
a11y-shell-people-agent = Agent
a11y-shell-people-now = Jetzt: { $activity }
a11y-shell-people-recent-toggle = Was { $name } zuletzt gemacht hat
a11y-shell-people-recent-toggle-description = Ihre letzten Änderungen ein- oder ausblenden
a11y-shell-sharing-change = Ändern, was du teilst
a11y-shell-follow = { $name } folgen
a11y-shell-follow-description = Die Live-Ansicht dieser Person im Arbeitsbereich spiegeln
a11y-shell-stop-following = { $name } nicht mehr folgen
a11y-shell-invite = Personen einladen
a11y-shell-invite-description = Eine authentifizierte Einladung in den Arbeitsbereich erstellen
a11y-shell-join = Arbeitsbereich beitreten

# Empty-workspace launchpad
launchpad-title = Etwas anfangen
launchpad-description = Wähle eine Unterhaltung, Notizen, Aufgaben, einen Anruf, eine Einladung oder die Aktivität im Arbeitsbereich.
launchpad-open-app = App öffnen
launchpad-discuss = Besprechen
launchpad-notes = Notizen
launchpad-tasks = Aufgaben
launchpad-agent = Agent verbinden
launchpad-meet = Anruf starten
launchpad-invite = Personen einladen
launchpad-activity = Auf dem Laufenden bleiben
launchpad-discuss-detail = Die gemeinsame Unterhaltung öffnen
launchpad-notes-detail = Gemeinsam Ideen aufschreiben
launchpad-tasks-detail = Aufschreiben, was zu tun ist
launchpad-agent-detail = Lass einen Agenten hier arbeiten
launchpad-meet-detail = Mit diesem Arbeitsbereich im Chat sprechen
launchpad-invite-detail = Jemanden in diesen Arbeitsbereich holen
launchpad-activity-detail = Sehen, was sich geändert hat und warum
launchpad-discuss-detail-private = Die Unterhaltung dieses Arbeitsbereichs öffnen
launchpad-notes-detail-private = Ideen aufschreiben
launchpad-meet-detail-private = Jemanden im Chat anrufen
launchpad-invite-detail-private = Gemeinsam mit jemandem daran arbeiten

# Empty-workspace launchpad: the keyboard hint under the cards
launchpad-keyboard-hint = Pfeiltasten bewegen, Eingabe öffnet, Super+N zeigt alle Apps

# Moved from the shell catalogue with the code that uses them
people-panel-agent-tag = Agent
people-panel-viewing = Sieht sich { $title } an
people-panel-in-app = In { $app }
# Jemand ohne Namen: die eine Angabe, die sie von jeder anderen namenlosen Person unterscheidet.
people-panel-joined = Beigetreten um { $time }
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
people-panel-invite-withheld = Du kannst niemanden in diesen Arbeitsbereich einladen. Wer dazukommt, entscheidet seine Inhaberin oder sein Inhaber.
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
launchpad-subtitle = Alle in diesem Arbeitsbereich sehen, was du hier machst.
launchpad-subtitle-private = Nur du siehst, was du hier machst, bis du jemanden einlädst.
launchpad-subtitle-public = Dieser Arbeitsbereich ist öffentlich: Alle, die beitreten, sehen, was du hier machst.

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
overview-access-lead = Geteilt mit
overview-access-one = Geteilt mit { $name }
overview-access-two = Geteilt mit { $name } und { $other }
overview-access-many = Geteilt mit { $name } und { $count } weiteren
overview-shared-in-app = { $scope } in { $app }
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
browser-welcome-subtitle = Ein Arbeitsbereich für dich, dein Team und deine KI-Agenten, laufend gespeichert und Ende-zu-Ende-verschlüsselt.
browser-welcome-hint = Pfeiltasten bewegen, Eingabe öffnet

# The workspace overview every desktop shows
overview-title = Arbeitsbereichsübersicht
overview-strip-empty = Keine Fenster
overview-workspaces = Arbeitsbereiche
overview-no-workspace = Kein Arbeitsbereich ausgewählt
overview-you-are-here = Du bist hier
overview-shared-out = Von hier aus geteilt
overview-more-shares = und { $count } weitere
overview-share-writable = darf bearbeiten
overview-more-people = +{ $count }
overview-new-workspace = Neuer Arbeitsbereich
overview-join-workspace = Einem Arbeitsbereich beitreten…
overview-leave-workspace = Verlassen
overview-a11y-leave = { $name } verlassen
overview-two-facts = { $first }, { $second }
overview-tilo-more-workspaces = Weitere Workspaces
overview-tilo-hours = { $hours } Std.
overview-tilo-hosts = Hosts & Rechenleistung
overview-tilo-agents = Agenten
overview-tilo-workspace-usage = { $server }, { $time } diesen Monat
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
overview-tilo-agent-working = { $sessions ->
        [one] Arbeitet gerade
       *[other] { $sessions } Sitzungen gerade
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
answer-app-from-before-update = { $app } läuft noch von vor dem Update
answer-why-from-before-update = Starte es neu, um es wieder zu sehen. Ungespeicherte Änderungen darin gehen verloren.
answer-why-restarting = Ungespeicherte Änderungen darin gehen verloren.
answer-restart = Neu starten
answer-apps-from-before-update = { $app } laufen noch von vor dem Update
answer-why-apps-from-before-update = Starte sie neu, um sie wieder zu sehen. Dort geöffnete Apps werden geschlossen, und ungespeicherte Änderungen darin gehen verloren.
answer-why-apps-restarting = Ungespeicherte Änderungen darin gehen verloren.
answer-restarting = Wird neu gestartet…
answer-app-closed-after-update = { $app } wurde nach dem Update geschlossen
answer-why-closed-after-update = Es lief noch von vor dem Update. Ungespeicherte Änderungen darin sind verloren.
answer-why-unknown-app = Es gibt keine App, die das öffnet.
answer-why-not-opened = Es wurde beendet, bevor ein Fenster erschien.
answer-why-no-sandbox = Dieser Computer erlaubt die Sandbox nicht, in der Apps laufen. Schalte Benutzer-Namespaces ein und versuche es dann erneut.
answer-why-not-built = Beim Öffnen ist ein Problem aufgetreten. Versuche es erneut. Wenn es sich weiterhin nicht öffnet, schließe dieses Fenster und öffne { $app } erneut.
answer-why-cannot-start = Apps können gerade nicht gestartet werden.
# Ein Kürzel des Desktops, für das eine Browserseite keine Oberfläche hat: sie
# sagt das, statt nichts zu tun.
answer-capture-not-in-browser = Bildschirmfotos und Aufnahmen gibt es in Tilo auf einem Computer
answer-why-capture-not-in-browser = Dafür braucht es den ganzen Bildschirm, den eine Browserseite nicht hat.
answer-remote-not-in-browser = Fernsteuerung gibt es in Tilo auf einem Computer
answer-why-remote-not-in-browser = Hier steht nichts unter der Kontrolle von jemand anderem.

# Das Menü hinter dem Tilo-Zeichen der oberen Leiste und das Info-Blatt, das es öffnet
tilo-about-title = Über diesen Computer
tilo-about-title-browser = Über Tilo
tilo-about-lead = Das ist Tilo, ein Betriebssystem.
tilo-about-body-browser = Du benutzt Tilo in deinem Browser. Bald läuft es auch direkt auf deinem eigenen Computer.
tilo-about-version = Tilo { $version }
tilo-about-link-get = Tilo holen
tilo-about-link-home = tilo.so
tilo-about-link-updates = Updates
tilo-about-link-details = Details
tilo-about-link-notify = Benachrichtigt werden
tilo-about-link-source = Quellcode
tilo-about-link-community = Community

notification-preview-hidden = Benachrichtigungsvorschau verborgen

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
activity-morning-unavailable = Diese Quelle ist hier nicht verfügbar
activity-morning-source-unnamed = Gewählte Quelle
activity-morning-source = { $source }, letzte Aktivität { $time }

activity-morning-open = Morgenquelle öffnen

activity-morning-payload-unavailable = Noch keine Zusammenfassung. Aktualisiere die Quelle, um es erneut zu versuchen.

activity-morning-refresh = Quelle aktualisieren
activity-morning-draft = Geplante Aktualisierung entwerfen…
activity-morning-command-unavailable = Quellenaktion nicht verfügbar

activity-morning-next = Nächste ausgewählte Quelle

# Jemand anderes hat ein Fenster außer Sicht geöffnet: der Hinweis am Rand
desktop-window-opened-elsewhere = { $person } hat { $app } geöffnet
# Strip switcher: the strip ribbon grown into labels, one per strip of the workspace.
strip-switcher-a11y = Leisten
strip-switcher-new-strip = Neue Leiste
strip-switcher-rename-named = { $name } umbenennen
strip-switcher-name-placeholder = Name der Leiste
strip-switcher-refused = Die Leiste konnte nicht geändert werden
strip-switcher-remove-named = { $name } entfernen
strip-switcher-rename = Umbenennen
strip-switcher-remove = Entfernen

profile-action-settings = Einstellungen
profile-action-devices-access = Geräte und Zugriff

# Der Ansicht einer Person folgen
follow-label = Du folgst { $person }
follow-stopped = Folgen beendet
follow-stop = Beenden

people-follows = Folgt { $person }
people-follows-you = Folgt dir
people-follows-where = { $place }, folgt { $person }
people-follows-you-where = { $place }, folgt dir

## The desktop's published actions
desktop-actions-chrome = Desktop
desktop-actions-shell = Fenster
desktop-actions-launchpad = Startfeld
shell-focus-window = { $window } fokussieren
shell-close-window = { $window } schließen
shell-look-window = { $window } als Bild ansehen
shell-look-window-action = Das Fenster als Bild ansehen
shell-look-window-detail = Sieht ein Fenster, das dieser Desktop nicht selbst führt, so an, wie es gerade aussieht: antwortet mit dem Bild, aufbewahrt im Arbeitsbereich, in dem das Fenster steht.
shell-read-window = Lesen, was { $window } zeigt
shell-read-window-action = Wie das Fenster sich selbst nennt
shell-read-window-detail = Was ein Fenster, das dieser Desktop nicht selbst führt, über sich sagt: sein Titel und die App, zu der es gehört.
session-host-new-workspace = Neuer Arbeitsbereich
session-host-new-workspace-description = Auf diesem Rechner anlegen
session-host-branch-workspace = Arbeitsbereich abzweigen
session-host-branch-workspace-description = Eine Kopie dieses Bereichs, die ihren eigenen Weg geht
desktop-branch-workspace-name = { $name } Abzweig
desktop-cannot-branch-workspace = Dieser Arbeitsbereich enthält, was du nicht lesen kannst, darum kann er hier nicht abgezweigt werden
session-branching-workspace = { $name } wird abgezweigt
session-branching-workspace-body = { $branch } öffnet sich, sobald seine Inhalte und Dateien kopiert sind.
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
session-join-failed-title = Arbeitsbereich konnte nicht beigetreten werden
session-awaiting-admission = Dieses Gerät ist noch nicht in { $name }
session-awaiting-admission-body = { $host } stellt { $name } bereit und muss dieses Gerät zuerst aufnehmen. Seine Apps warten hier, bis dieses Gerät { $host } einmal erreicht.
desktop-no-workspace = Es gibt keinen Arbeitsbereich namens { $name }
desktop-joined-workspace-unknown = Dieser Arbeitsbereich liegt in einem Arbeitsbereich, dem dieser Rechner nicht beitreten kann
desktop-workspace-not-held = Dieser Rechner ist noch nicht in diesem Arbeitsbereich. Tritt ihm zuerst mit einer Einladung bei
shell-first-workspace = Das ist der erste Arbeitsbereich
shell-last-workspace = Das ist der letzte Arbeitsbereich
shell-first-strip = Das ist die erste Leiste
shell-last-strip = Das ist die letzte Leiste
shell-window-edge = Nichts geändert. Das Fenster kann nicht weiter in diese Richtung.
shell-window-closed = { $window } geschlossen
shell-window-closed-undo = Rückgängig
desktop-no-strip = Hier gibt es keine Leiste namens { $name }
desktop-workspace-gone = Diesen Arbeitsbereich gibt es hier nicht mehr
desktop-verb-names-nothing = Das gibt es auf diesem Desktop nicht mehr
desktop-no-workspace-to-leave = Es gibt keinen Arbeitsbereich zu verlassen
desktop-no-workspace-to-delete = Es gibt keinen Arbeitsbereich zu löschen
desktop-only-workspace = Das ist der einzige Arbeitsbereich hier
desktop-workspace-stays = Dieser Arbeitsbereich bleibt: Jemand anderes ist darin, oder ein anderes deiner Geräte hält ihn.
desktop-cannot-delete-workspace = Dieser Desktop kann keine Arbeitsbereiche löschen
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

session-window-share = Geteilte Fenster
session-window-share-share = Ein Fenster mit dem Arbeitsbereich teilen
session-window-share-param-window = Das Fenster, nach der Kennung, die seine App erfassen darf; weglassen für das Fenster, das der Tab zeigt
session-window-share-param-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-param-app = Die App, zu der das Fenster gehört; weglassen für die App des Fensters, das der Tab zeigt
session-window-share-param-workspace = Der Arbeitsbereich, in dem es geteilt wird
session-window-share-param-share-workspace = Der Arbeitsbereich, in dem es geteilt wird; weglassen für den, in dem der Tab steht
session-window-share-stop = Das Teilen des Fensters beenden
session-window-share-allow-input = Den Arbeitsbereich das Fenster bedienen lassen
session-window-share-param-on = Ob sie es bedienen dürfen; weglassen zum Umschalten
session-window-share-watch = Ein Fenster zeigen, das ein anderes Mitglied teilt
session-window-share-unwatch = Es nicht mehr zeigen
session-window-share-workspace-left = Dieser Rechner ist nicht mehr im Arbeitsbereich
session-window-share-live-keys = Wartet darauf, dass die Schlüssel dieses Arbeitsbereichs dieses Gerät erreichen
session-window-share-live-keyless = Dieses Gerät hat keine Schlüssel dieses Arbeitsbereichs
session-window-share-live-unread = Dieses Gerät kann nicht lesen, wer in diesem Arbeitsbereich ist
session-window-share-live-no-carrier = Kein Gerät in diesem Arbeitsbereich überträgt seine Anrufe und geteilten Fenster
session-window-share-live-unreached = Wartet auf die Verbindung zu { $device }, dem Gerät, das die Anrufe und geteilten Fenster dieses Arbeitsbereichs überträgt
session-window-share-live-busy = { $device }, das Gerät, das die Anrufe und geteilten Fenster dieses Arbeitsbereichs überträgt, nimmt gerade so viele Geräte auf, wie es auf einmal kann; dieses Gerät fragt gleich noch einmal
session-window-share-live-refused = { $device } hat dieses Gerät nicht zu den Anrufen und geteilten Fenstern dieses Arbeitsbereichs zugelassen
session-window-share-live-absent = Dieser Rechner nimmt an den Anrufen und geteilten Fenstern dieses Arbeitsbereichs nicht teil
session-window-share-live-lent = Dieses Gerät kann in diesem Arbeitsbereich eigene Fenster teilen und nicht öffnen, was andere teilen
session-window-share-live-unheard-bound = { $count ->
        [one] Ein weiteres Gerät ist in den Anrufen und geteilten Fenstern dieses Arbeitsbereichs und wird hier nicht gehört: Dieses Gerät hört { $bound } andere gleichzeitig
       *[other] { $count } weitere Geräte sind in den Anrufen und geteilten Fenstern dieses Arbeitsbereichs und werden hier nicht gehört: Dieses Gerät hört { $bound } andere gleichzeitig
    }
session-window-share-first-page = Wartet auf die Seite des Fensters von dem Gerät, auf dem es läuft
session-window-share-first-picture = Wartet auf das erste Bild des Fensters
session-window-share-page-unsent = Wartet darauf, dass die Seite des Fensters an die Geräte geht, die es zeigen
session-window-share-page-withheld = Die App des Fensters wurde nach dessen Seite gefragt und hat sie nicht übergeben
session-window-share-page-overdue = Das Gerät, auf dem das Fenster läuft, wurde nach dessen Seite gefragt und hat sie nicht gesendet
session-window-share-page-too-large = Die Seite des Fensters ist zu groß, um sie an die Geräte zu senden, die es zeigen
session-window-share-page-unoffered = Wartet darauf, dass die App des Fensters dessen Seite anbietet: Das Fenster wird als seine Seite geteilt, nie als Bilder davon

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
window-menu-move-left = Nach links verschieben
window-menu-move-right = Nach rechts verschieben
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
# Ein Fenster mit seinem Arbeitsbereich zu teilen zeigt sein Livebild in seinem Tab auf den anderen
# Geräten des Arbeitsbereichs. „In Kanal teilen…“ aus dem Toolkit stellt das Fenster dagegen in
# einen Kanal: Die beiden Zeilen stehen beieinander, also nennt jede, wohin das Fenster geht.
window-menu-share-with-workspace = Mit dem Arbeitsbereich teilen
window-menu-stop-sharing-with-workspace = Teilen mit dem Arbeitsbereich beenden
window-menu-stop-sharing-watched = Teilen mit dem Arbeitsbereich beenden ({ $viewers } { $viewers ->
        [one] sieht zu
       *[other] sehen zu
    })
window-menu-stop-sharing-waiting = Teilen mit dem Arbeitsbereich beenden ({ $viewers } { $viewers ->
        [one] wartet
       *[other] warten
    })
window-menu-someone-elses = { $name }s Fenster
window-menu-someone-elses-unnamed = Fenster von jemand anderem
# Ein Fenster, das ein anderes Gerät teilt, wie der Tab es sagt, der es zeigt: wie der Tab heißt,
# wessen Fenster es ist, ob dieses Gerät hineinschreiben darf und wie es um seine Bilder steht.
peer-window-name = { $window } auf { $device }
peer-window-of = Fenster von { $name }
peer-window-can-type = Eingabe erlaubt
peer-window-view-only = Nur ansehen
peer-window-first-picture = Wartet auf das erste Bild
peer-window-picture-lost = Ein Teil des Bildes ging verloren, und das nächste ganze ist noch nicht angekommen
peer-window-live = Live
peer-window-ended = Beendet: { $why }
# Warum es endete, als Rest des Satzes nach „Beendet:“.
peer-window-ended-tab-gone = das Fenster wird hier nicht mehr gezeigt
peer-window-ended-left-workspace = dieses Gerät hat den Arbeitsbereich verlassen
peer-window-ended-sender-stopped = der Absender teilt es nicht mehr
peer-window-ended-restarted = sein Bild hat die Größe geändert
peer-window-ended-let-go = dieses Gerät hat das Fenster losgelassen
# Derselbe Tab, solange nichts vom Fenster darin gezeigt wird: wessen App das Fenster gehört und
# auf welchem Gerät es läuft, und darunter, warum es nicht hier ist. Ein Gerät, das nichts benennt,
# ist ein anderes Gerät des Arbeitsbereichs, nie sein Schlüssel.
peer-window-elsewhere = { $app } läuft auf { $device }
peer-window-elsewhere-unnamed = { $app } läuft auf einem anderen Gerät dieses Arbeitsbereichs
peer-window-not-shared = Es wird gerade nicht mit diesem Arbeitsbereich geteilt
peer-window-awaited = Wartet darauf, dass { $device } es sendet
peer-window-awaited-unnamed = Wartet darauf, dass sein Gerät es sendet
peer-window-host-away = { $device } ist gerade nicht mit diesem Arbeitsbereich verbunden
peer-window-host-away-unnamed = Sein Gerät ist gerade nicht mit diesem Arbeitsbereich verbunden
peer-window-host-left = { $device } gehört nicht mehr zu diesem Arbeitsbereich
peer-window-host-left-unnamed = Sein Gerät gehört nicht mehr zu diesem Arbeitsbereich
# Warum der Tab eine davon nicht ausführt.
peer-window-refused-ended = Dieses Fenster wird nicht mehr geteilt
peer-window-refused-view-only = Dieses Fenster ist nur zum Ansehen geteilt
peer-window-refused-number = „{ $argument }“ ist keine Zahl
peer-window-refused-whole = „{ $argument }“ ist keine ganze Zahl
peer-window-refused-flag = „{ $argument }“ ist weder wahr noch falsch
peer-window-refused-button = Das ist keine Zeigertaste
peer-window-refused-key = Das ist keine Taste
peer-window-refused-text = Es gibt keinen Text einzugeben
# The shell's widgets: what each is called, and what its controls do.
shell-widget-music = Musik
shell-widget-team = Menschen hier
shell-widget-calendar = Kalender
shell-widget-system-monitor = Systemmonitor
shell-widget-pin = Dieses Widget immer in diesem Arbeitsbereich anzeigen
shell-widget-unpin = Dieses Widget nicht mehr immer anzeigen
shell-widget-open = { $widget } öffnen
shell-widget-collapse = { $widget } einklappen
shell-widget-remove = { $widget } entfernen
shell-widget-removed = { $widget } vom Desktop entfernt
shell-widget-undo = Rückgängig
shell-team-title = In diesem Arbeitsbereich
shell-team-more = +{ $count } weitere
shell-monitor-title = Dieses Gerät
shell-monitor-cpu = Prozessor
shell-monitor-memory = Arbeitsspeicher
shell-monitor-temperature = Temperatur
shell-monitor-cpu-short = CPU
shell-monitor-memory-short = RAM

hung-app-title = Antwortet nicht
hung-app-card = { $window } antwortet nicht
hung-app-wait = Warten
hung-app-force-quit = Erzwungen beenden
shell-wait-window = Weiter auf { $window } warten
shell-force-quit-window = { $window } erzwungen beenden
activities-layer = Aktivitäten
activity-collapse = Weglegen
activity-close = Schließen

profile-agent-card = Arbeit ansehen
profile-agent-stop = Anhalten
profile-agent-undo-run = Lauf rückgängig machen
profile-agent-take-over = Übernehmen
profile-agent-revoke = Zugriff entziehen
profile-agent-now = Gerade
agent-tools-resource = Agentenwerkzeuge
agent-tools-list = Werkzeuge auflisten
agent-tools-list-detail = Listet die Werkzeuge, mit denen ein Agent in seiner Sitzung arbeitet.
agent-tools-call = Ein Werkzeug aufrufen
agent-tools-call-detail = Führt eines der Werkzeuge des Agenten als Schritt seiner Sitzung aus und antwortet, was es gesagt hat.

## What a grant covers when it covers the whole tree
workspace-settings-scope-everything = Alles

## Actions

so-tilo-chrome-about = Über Tilo
so-tilo-chrome-activity = Verlauf des Arbeitsbereichs
so-tilo-chrome-branch-workspace = Den Arbeitsbereich abzweigen
so-tilo-chrome-close-activity = Den Verlauf schließen
so-tilo-chrome-close-window = Ein Fenster schließen
so-tilo-chrome-delete-workspace = Einen Arbeitsbereich löschen
so-tilo-chrome-equalize = Gleiche Breiten
so-tilo-chrome-fixed-shortcut = Ein festes Tastenkürzel ausführen
so-tilo-chrome-float = Das Fenster schweben lassen
so-tilo-chrome-fullscreen = Vollbild
so-tilo-chrome-history = Fensterverlauf
so-tilo-chrome-join-workspace = Einem Arbeitsbereich beitreten
so-tilo-chrome-leave-workspace = Einen Arbeitsbereich verlassen
so-tilo-chrome-maximize = Ein Fenster maximieren
so-tilo-chrome-delete-strip = Eine Leiste löschen
so-tilo-chrome-move-to-new-strip = Ein Fenster in eine eigene Leiste verschieben
so-tilo-chrome-move-strip = Eine Leiste verschieben
so-tilo-chrome-move-tab = Einen Tab verschieben
so-tilo-chrome-new-group = Neue Gruppe
so-tilo-chrome-new-strip = Neue Leiste
so-tilo-chrome-new-workspace = Neuer Arbeitsbereich
so-tilo-chrome-notifications = Benachrichtigungszentrum
so-tilo-chrome-open-app = Eine App öffnen
so-tilo-chrome-overview = Übersicht
so-tilo-chrome-pan = Eine Leiste schieben
so-tilo-chrome-quick-settings = Schnelleinstellungen
so-tilo-chrome-rename = Umbenennen
so-tilo-chrome-reopen-closed = Das geschlossene Fenster wieder öffnen
so-tilo-chrome-reveal-top-bar = Die obere Leiste zeigen
so-tilo-chrome-scroll = Ein Fenster scrollen
so-tilo-chrome-set-width = Die Breite setzen
so-tilo-chrome-share-window = Ein Fenster teilen
so-tilo-chrome-shortcut = Ein Tastenkürzel ausführen
so-tilo-chrome-open-settings = Einstellungen öffnen
so-tilo-chrome-show-app = Eine App zeigen
so-tilo-chrome-show-person = Eine Person zeigen
so-tilo-chrome-show-strip = Eine Leiste zeigen
so-tilo-chrome-show-strips = Die Leisten zeigen
so-tilo-chrome-split-right = Nach rechts teilen
so-tilo-chrome-step-strip = Durch die Leisten wechseln
so-tilo-chrome-stop-desktop-control = Die Fernsteuerung beenden
so-tilo-chrome-switch-workspace = Arbeitsbereich wechseln
so-tilo-chrome-toggle-people = Wer hier ist
so-tilo-chrome-workspace-menu = Arbeitsbereich-Menü
so-tilo-chrome-workspace-settings = Einstellungen des Arbeitsbereichs
so-tilo-layout-choose-search-row = Ein Suchergebnis wählen
so-tilo-layout-close-tab = Ein Fenster schließen
so-tilo-layout-focus = Ein Fenster fokussieren
so-tilo-layout-focus-display = Eine Anzeige fokussieren
so-tilo-layout-force-quit-app = Die App sofort beenden
so-tilo-layout-launchpad = Eine Karte wählen
so-tilo-layout-move-to-display = Auf eine andere Anzeige verschieben
so-tilo-layout-search = Den Desktop durchsuchen
so-tilo-layout-step-focus = Durch die Fenster wechseln
so-tilo-layout-wait-window = Weiter warten

# Parameterbeschreibungen der deklarierten Vokabulare, als `<Label>-<Argument>` gelesen.

desktop-action-show-osd-kind = Worum es geht: volume, brightness, keyboard-backlight, microphone, airplane-mode, touchpad, play, pause, next-track, previous-track, stop, no-backlight, night-light, do-not-disturb, magnifier, captured-keys oder pointer-released
desktop-action-show-osd-level = Die Stufe, von 0 bis 1
desktop-action-show-osd-on = Ob es an ist
desktop-action-show-osd-device = Das geänderte Gerät, wie eine Person es kennt
desktop-action-show-osd-device-glyph = Das Symbol des Geräts: speaker, headphones, display oder keyboard
desktop-action-show-osd-chord = Die Tastenkombination, die es ausgelöst hat
desktop-action-show-osd-display = Der Anschluss des Bildschirms, auf dem es erscheint
session-window-share-share-window = Das Fenster, nach der Kennung, die seine App erfassen darf; weglassen für das Fenster, das der Tab zeigt
session-window-share-share-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-share-app_id = Die App, zu der das Fenster gehört; weglassen für die App des Fensters, das der Tab zeigt
session-window-share-share-workspace = Der Arbeitsbereich, in dem es geteilt wird; weglassen für den, in dem der Tab steht
session-window-share-stop-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-allow-input-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-allow-input-on = Ob sie es bedienen dürfen; weglassen zum Umschalten
session-window-share-watch-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-watch-workspace = Der Arbeitsbereich, aus dem es gezeigt wird
session-window-share-unwatch-tab = Der Tab, in dem das Fenster auf diesem Gerät gezeigt wird
session-window-share-unwatch-workspace = Der Arbeitsbereich, aus dem es gezeigt wird
programs-resource = Deine Programme
programs-run-line = Diese Zeile ausführen
programs-run-line-detail = Führt eine tilolang-Zeile als dich aus und antwortet, was sie gelesen oder geändert hat.
