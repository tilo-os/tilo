# Kalender — deutsche Übersetzung (de-DE).

## Verlauf: was jede Änderung getan hat, nach Feld und nach Aktion

calendar-new-event = Ein Ereignis hinzugefügt
calendar-new-task = Eine Aufgabe hinzugefügt
calendar-new-project = Das Projekt { $name } hinzugefügt
calendar-delete-event = Ein Ereignis gelöscht
calendar-reschedule = Ein Ereignis verschoben
calendar-set-start = Den Beginn eines Ereignisses verschoben
calendar-move-to-calendar = Ein Ereignis in einen anderen Kalender verschoben
calendar-invite = { $email } eingeladen
calendar-delete-task = Eine Aufgabe gelöscht
calendar-delete-project = Ein Projekt gelöscht
calendar-import-items = Kalendereinträge importiert
calendar-adopt-remote-event = Ein Ereignis von seinem Server übernommen
calendar-adopt-remote-task = Eine Aufgabe von ihrem Server übernommen
calendar-drop-remote-item = Entfernt, was ein Server nicht mehr hält
calendar-server-applied = Ein Kalenderserver hat ein Ereignis übernommen
calendar-server-removed = Ein Kalenderserver hat ein Ereignis gelöscht
calendar-pulled = Einen Kalender abgerufen
calendar-event-title = Ein Ereignis in { $new } umbenannt
calendar-event-start = Den Beginn eines Ereignisses verschoben
calendar-event-end = Das Ende eines Ereignisses verschoben
calendar-event-all-day = { $new ->
    [true] Ein Ereignis ganztägig gemacht
   *[false] Einem Ereignis seine Zeiten gegeben
}
calendar-event-location = Geändert, wo ein Ereignis stattfindet
calendar-event-color = Die Farbe eines Ereignisses geändert
calendar-event-recurrence = Geändert, wie ein Ereignis sich wiederholt
calendar-event-recurrence-interval = Geändert, wie oft ein Ereignis sich wiederholt
calendar-event-recurrence-until = Geändert, wann ein Ereignis aufhört, sich zu wiederholen
calendar-event-recurrence-count = Geändert, wie viele Male ein Ereignis sich wiederholt
calendar-event-recurrence-exdates = Ein Datum eines Ereignisses übersprungen oder wiederhergestellt
calendar-event-occurrence-overrides = Ein Datum eines Ereignisses geändert
calendar-event-timezone = Die Zeitzone eines Ereignisses geändert
calendar-event-organizer = Geändert, wer ein Ereignis organisiert
calendar-event-attendees = Geändert, wer zu einem Ereignis eingeladen ist
calendar-event-reminder-min = Die Erinnerung eines Ereignisses geändert
calendar-event-project = Ein Ereignis unter ein anderes Projekt eingeordnet
calendar-event-calendar-id = Ein Ereignis in einen anderen Kalender verschoben
calendar-attendee-name = Den Namen einer eingeladenen Person geändert
calendar-attendee-email = Die Adresse einer eingeladenen Person geändert
calendar-attendee-status = Eine Antwort auf eine Einladung festgehalten
calendar-occurrence-override-cancelled = { $new ->
    [true] Ein Datum eines Ereignisses abgesagt
   *[false] Ein Datum eines Ereignisses wiederhergestellt
}
calendar-occurrence-override-start = Ein Datum eines Ereignisses verschoben
calendar-occurrence-override-end = Das Ende eines Datums eines Ereignisses verschoben
calendar-occurrence-override-title = Ein Datum eines Ereignisses umbenannt
calendar-occurrence-override-location = Geändert, wo ein Datum eines Ereignisses stattfindet
calendar-task-title = Eine Aufgabe in { $new } umbenannt
calendar-task-done = { $new ->
    [true] Eine Aufgabe abgeschlossen
   *[false] Eine Aufgabe wieder geöffnet
}
calendar-task-due = Geändert, wann eine Aufgabe fällig ist
calendar-task-priority = Die Priorität einer Aufgabe geändert
calendar-task-project = Eine Aufgabe unter ein anderes Projekt eingeordnet
calendar-project-name = Ein Projekt in { $new } umbenannt
calendar-project-color = Die Farbe eines Projekts geändert
calendar-project-order = Ein Projekt verschoben
calendar-calendar-projects = Die Projekte geändert
calendar-calendar-events = Die Ereignisse geändert
calendar-calendar-tasks = Die Aufgaben geändert
calendar-calendar-notes = Notizen bearbeitet
calendar-calendar-uploads = Festgehalten, wie ein Upload zu einem Kalenderserver lief
calendar-calendar-deletes = Festgehalten, wie ein Löschen auf einem Kalenderserver lief

## Die Aufgabenansicht

calendar-tasks-title = Aufgaben
calendar-view-month = Monat
calendar-view-week = Woche
calendar-view-day = Tag
calendar-view-agenda = Agenda
calendar-tasks-new = Neue Aufgabe
calendar-tasks-entry = Eine Aufgabe hinzufügen
calendar-tasks-entry-name = Titel der neuen Aufgabe
calendar-tasks-empty-inbox = Der Posteingang ist leer
calendar-tasks-empty-today = Heute ist nichts fällig
calendar-tasks-empty-upcoming = Nichts steht an
calendar-tasks-empty-completed = Keine abgeschlossenen Aufgaben
calendar-tasks-empty-project = Keine Aufgaben in { $list }
calendar-tasks-empty-completed-hint = Es wurde noch nichts abgeschlossen.
calendar-tasks-empty-hint = Tippe oben eine Aufgabe ein und drücke die Eingabetaste.
calendar-tasks-priority-none = Keine Priorität
calendar-tasks-priority-low = Niedrige Priorität
calendar-tasks-priority-medium = Mittlere Priorität
calendar-tasks-priority-high = Hohe Priorität
calendar-tasks-added = { $title } hinzugefügt
calendar-change-refused = Der Kalender hat das nicht übernommen: { $reason }
calendar-tasks-list = Aufgabenliste
calendar-tasks-add-to = Eine Aufgabe zu { $list } hinzufügen
calendar-tasks-row-due = { $title }, fällig { $due }
calendar-tasks-row-priority = { $title }, { $priority }
calendar-tasks-row-subtask = { $title }, Unteraufgabe
calendar-tasks-empty-description = { $title }. { $message }
calendar-view-name = Ansicht { $view }
calendar-tasks-inbox = Posteingang
calendar-tasks-today = Heute
calendar-tasks-upcoming = Demnächst
calendar-tasks-completed = Abgeschlossen

## Die Seitenleiste und wie die App sich für assistive Technik nennt

calendar-navigation = Kalendernavigation
calendar-month = Monat
calendar-projects = Projekte
calendar-sync = Abgleich
calendar-open-task = Aufgabe öffnen
calendar-close-list = Liste schließen
calendar-list-empty = Keine Aufgaben
calendar-project = Projekt

## Was das Fenster über einen Abgleich, einen Import, eine Bearbeitung oder
## einen geänderten Kalender sagt, jeweils in der Schwere des Ortes, der es meldet.

calendar-sync-running = Kalender werden abgeglichen …
calendar-sync-running-short = Abgleich läuft …
calendar-sync-synced = { $calendars } Kalender und { $events } Ereignisse abgeglichen.
calendar-sync-offline = Offline. Änderungen werden abgeglichen, sobald du wieder verbunden bist.
calendar-sync-conflict = Eine Änderung wurde auch auf dem Server bearbeitet. Deine Fassung bleibt erhalten und wird später abgeglichen.
calendar-sync-failed = Abgleich fehlgeschlagen
calendar-sync-failed-hint = Abgleich fehlgeschlagen. Prüfe das Konto in den Einstellungen und gleiche dann erneut ab.
calendar-sync-error = Abgleich fehlgeschlagen: { $reason }. Prüfe das Konto in den Einstellungen und gleiche dann erneut ab.
calendar-sync-unreachable = Der Kalenderserver war nicht erreichbar
calendar-sync-unavailable = Abgleich nicht verfügbar: { $reason }
calendar-sync-no-accounts = Noch keine Kalenderkonten. Füge eines in den Einstellungen hinzu.
calendar-account-gone = Dieses Kalenderkonto ist nicht mehr eingerichtet
calendar-invite-address-required = Gib eine vollständige E-Mail-Adresse ein, etwa name@example.com
calendar-invite-already = { $email } ist bereits eingeladen
calendar-invite-added = { $email } hinzugefügt. Sende die Einladung in Mail.
calendar-invite-prepared = Einladung für Mail vorbereitet. Prüfe sie dort, bevor du sie sendest.
calendar-export-empty = Noch nichts zu exportieren
calendar-exported = { $events } Ereignisse und { $tasks } Aufgaben exportiert
calendar-export-cancelled = Export abgebrochen
calendar-move-event-failed = Das Ereignis konnte nicht verschoben werden: { $reason }
calendar-showing-all = Alle Kalender werden angezeigt
calendar-showing-calendar = { $calendar } wird angezeigt
calendar-calendar-hidden = { $calendar } ist ausgeblendet. Schalte ihn in der Seitenleiste ein, um seine Ereignisse zu sehen.
calendar-visible = Der Kalender ist jetzt sichtbar
calendar-hidden = Der Kalender ist jetzt ausgeblendet

## Ziehen an einem Datum eines wiederholten Ereignisses
calendar-repeat-change = Wiederholtes Ereignis ändern
calendar-repeat-only-this = Nur dieses Ereignis
calendar-repeat-all-future = Alle künftigen Ereignisse
calendar-repeat-failed = Die Ereignisse konnten nicht verschoben werden: { $error }

## Eingeladene aus den Kontakten der Person
calendar-contact-attendees = Aus Kontakte einladen
calendar-contact-attendee = { $person } <{ $email }>
calendar-contact-add = Als Gast hinzufügen

## Das einzeilige Schnellanlegen: wonach es fragt und was es schreibt

calendar-quick-title = Ein Ereignis hinzufügen
calendar-quick-placeholder = Mittagessen mit Sam am Freitag um 13 Uhr
calendar-quick-field = Was hinzugefügt wird
calendar-quick-add = Hinzufügen
calendar-quick-cancel = Abbrechen

## Der Titel des Blatts, das einen Termin oder eine Aufgabe bearbeitet

calendar-event-detail-title = Termin
calendar-task-detail-title = Aufgabe

## Die Tasten, auf die dieses Fenster hört

calendar-help-title = Tastenkürzel
calendar-help-month = Monatsansicht
calendar-help-week = Wochenansicht
calendar-help-day = Tagesansicht
calendar-help-agenda = Agenda-Ansicht
calendar-help-today = Zu heute springen
calendar-help-previous = Vorheriger Zeitraum
calendar-help-next = Nächster Zeitraum
calendar-help-previous-day = Vorheriger Tag
calendar-help-next-day = Nächster Tag
calendar-help-previous-week = Vorherige Woche
calendar-help-next-week = Nächste Woche
calendar-help-new-event = Neues Ereignis, ausführlich
calendar-help-quick-add = Ein Ereignis in einer Zeile hinzufügen
calendar-help-delete = Das ausgewählte Element löschen
calendar-help-close = Den Editor oder die Karte schließen
calendar-help-shortcuts = Diese Liste der Tasten

## Wie das Fenster heißt und wonach seine Felder fragen

calendar-title = Kalender
calendar-a11y-syncing = Kalender werden abgeglichen
calendar-a11y-editor = Editor für das Ereignis. Änderungen werden von selbst übernommen.
calendar-quick-example = Mittagessen mit Sam Freitag um 13 Uhr
calendar-invite-placeholder = Per E-Mail einladen
calendar-location-placeholder = Ort hinzufügen

## Fenster- und Barrierefreiheits-Texte
calendar-a11y-task-due = Fällig am

## Die Antwort des Editors
calendar-done = Fertig

## Was ein Bedienelement vorliest
calendar-a11y-today = Zum heutigen Tag
calendar-a11y-previous = Vorheriger Zeitraum
calendar-a11y-next = Nächster Zeitraum
calendar-a11y-new-event = Neuer Termin
calendar-a11y-new-task = Neue Aufgabe
calendar-a11y-retry-sync = Kalender-Abgleich wiederholen
calendar-a11y-sync = Kalender abgleichen
calendar-a11y-export = Kalender exportieren
calendar-a11y-import = Kalender importieren
calendar-a11y-open-day = { $day } öffnen
calendar-a11y-discard-draft = Neuen Termin verwerfen
calendar-a11y-delete-task = Aufgabe löschen
calendar-a11y-all-day = Ganztägiger Termin
calendar-a11y-event-title = Titel des Termins bearbeiten
calendar-a11y-event-location = Ort des Termins bearbeiten
calendar-a11y-event-invite = Person per E-Mail einladen
calendar-a11y-start-date = Startdatum des Termins wählen
calendar-a11y-end-date = Enddatum des Termins wählen
calendar-a11y-start-time = Startzeit des Termins wählen
calendar-a11y-end-time = Endzeit des Termins wählen
calendar-a11y-choose-task-due = Fälligkeit der Aufgabe wählen
calendar-a11y-dismiss-picker = Aufgabenauswahl schließen
calendar-a11y-due-previous-month = Vorheriger Fälligkeitsmonat
calendar-a11y-due-next-month = Nächster Fälligkeitsmonat
calendar-a11y-clear-due = Fälligkeit der Aufgabe entfernen
calendar-a11y-set-due = Fälligkeit der Aufgabe auf { $day } setzen
calendar-a11y-move-event-to = Termin auf { $day } verschieben
calendar-a11y-show-tasks-in = Aufgaben in { $list } anzeigen
calendar-a11y-show-calendar = Kalender { $calendar } anzeigen
calendar-a11y-account-calendars = Kalender von { $account }
calendar-a11y-open-event = Termin { $title } öffnen
calendar-a11y-open-task = Aufgabe { $title } öffnen
calendar-a11y-toggle-task = { $done ->
    [true] Aufgabe { $title } wieder öffnen
   *[false] Aufgabe { $title } als erledigt markieren
}
calendar-a11y-delete-event = Termin löschen

## Wie die Auswahllisten des Editors heißen
calendar-editor-calendar = Kalender
calendar-editor-repeat = Wiederholung
calendar-editor-alert = Erinnerung
calendar-editor-time-zone = Zeitzone
calendar-editor-priority = Priorität

## Die Plätze der Fensterleiste
calendar-views = Ansicht
calendar-previous-month = Voriger Monat
calendar-next-month = Nächster Monat
calendar-sync-again = Erneut synchronisieren
calendar-open-settings = Einstellungen öffnen

## Der Hinweis im leeren Notizfeld eines Termins
calendar-notes-placeholder = Notizen hinzufügen

## Die leeren Felder eines neuen Termins
calendar-title-placeholder = Neuer Termin

## Actions

calendar-effects-sync = Kalender synchronisieren
calendar-effects-export-ics = ICS exportieren
calendar-effects-export-ics-content = ICS-Inhalt exportieren
calendar-effects-import-ics = ICS importieren
calendar-effects-contact-attendees = Aus Kontakten einladen
calendar-effects-prepare-in-mail = Einladung in Mail vorbereiten
calendar-effects-set-color = Kalenderfarbe wählen
calendar-effects-read-calendar = Kalender lesen
calendar-effects-set-visibility = Kalender ein- oder ausblenden
calendar-effects-show-view = Kalenderansicht anzeigen
calendar-effects-show-all-calendars = Alle Kalender anzeigen
calendar-effects-show-month = Monat anzeigen
calendar-effects-show-week = Woche anzeigen
calendar-effects-show-day = Tag anzeigen
calendar-effects-show-agenda = Agenda anzeigen
calendar-effects-show-tasks = Aufgaben anzeigen
calendar-effects-start-event = Neuer Termin
calendar-effects-start-task = Neue Aufgabe
calendar-effects-open = Öffnen
calendar-calendar = Kalender
calendar-calendar-new-event = Neuer Termin
calendar-calendar-new-task = Neue Aufgabe
calendar-calendar-new-project = Neues Aufgabenprojekt
calendar-calendar-delete-event = Termin löschen
calendar-calendar-reschedule = Termin verschieben
calendar-calendar-set-start = Beginn festlegen
calendar-calendar-move-to-calendar = In anderen Kalender verschieben
calendar-calendar-invite = Einladen
calendar-calendar-delete-task = Aufgabe löschen
calendar-calendar-delete-project = Projekt löschen
calendar-calendar-import-items = Kalendereinträge importieren
calendar-calendar-adopt-remote-event = Termin vom Server übernehmen
calendar-calendar-adopt-remote-task = Aufgabe vom Server übernehmen
calendar-calendar-drop-remote-item = Vom Server Gelöschtes entfernen
calendar-calendar-server-applied = Hochgeladene Änderungen erfassen
calendar-calendar-server-removed = Löschung auf dem Server erfassen
calendar-calendar-pulled = Abgerufene Änderungen erfassen

## Die Zeilenbeschriftungen der Termin- und Aufgabenformulare
calendar-field-starts = Beginn
calendar-field-ends = Ende
calendar-field-all-day = Ganztägig
calendar-field-location = Ort
calendar-field-calendar = Kalender
calendar-field-repeat = Wiederholung
calendar-field-alert = Erinnerung
calendar-field-time-zone = Zeitzone
calendar-field-colour = Farbe
calendar-field-guests = Gäste
calendar-field-due = Fälligkeit
calendar-field-priority = Priorität
calendar-field-project = Projekt
calendar-field-subtasks = Teilaufgaben
calendar-field-no-due-date = Fälligkeit hinzufügen
