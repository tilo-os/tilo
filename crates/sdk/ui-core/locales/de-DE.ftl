counter = Wert: { $count }
tab-modified = Nicht gespeicherte Änderungen
tab-loading = Wird geladen
tab-playing = Gibt Ton wieder
tab-muted = Stummgeschaltet
tab-close-named = { $title } schließen
tab-mute-named = { $title } stummschalten
tab-search-in = In { $title } suchen
tab-held-by = Fenster von { $name }
tab-opened-by = { $agent } hat dieses Fenster geöffnet
tab-opened-by-run = { $run }
standalone-search-title = Suchen

history-mode-title = Verlauf
history-mode-select-hint = Diese Änderung im Verlauf anzeigen
history-mode-entry-ids = Änderung
history-mode-preview-resource = Dokument
host-search = Suchen…
host-search-placeholder = Suchen
dropdown-search-placeholder = Auswahl durchsuchen
dropdown-search-description = Zum Filtern tippen. Mit den Pfeiltasten auswählen und mit Eingabe bestätigen.
host-undo = Widerrufen
host-undo-named = { $change } widerrufen
host-redo = Wiederholen
host-redo-named = { $change } wiederholen
host-zoom-in = Vergrößern
host-zoom-out = Verkleinern
host-zoom-reset = Originalgröße
host-history-show = Verlauf einblenden
host-history-hide = Verlauf ausblenden
host-history-recent = { $change } von { $author }, { $when }
host-history-unavailable = Es gibt kein Dokument, dessen Verlauf angezeigt werden kann.
host-menu-file = Ablage
host-menu-edit = Bearbeiten
host-menu-view = Darstellung
host-menu-history = Verlauf
host-menu-window = Fenster
host-menu-help = Hilfe
host-menu-about = Über { $app }
host-menu-services = Dienste
host-menu-hide = { $app } ausblenden
host-menu-hide-others = Andere ausblenden
host-menu-show-all = Alle einblenden
host-menu-quit = { $app } beenden
host-menu-close = Fenster schließen
host-menu-minimize = Im Dock ablegen
host-menu-zoom = Zoomen
host-menu-front = Alle nach vorne bringen
host-window-minimize = Minimieren
host-window-maximize = Maximieren
host-window-restore = Wiederherstellen
host-window-snap = Zurück in den Streifen
host-window-collapse = Einklappen
host-window-expand = Ausklappen
host-window-close = Schließen
window-toolbar-back = Zurück
window-toolbar-forward = Vorwärts
window-toolbar-new-tab = Neuer Tab
window-toolbar-go = Öffnen
window-toolbar-more = Weitere Aktionen
window-toolbar-views = Ansicht
window-toolbar-suggestions = Vorschläge
window-toolbar-edited = Bearbeitet
window-toolbar-view-only = Nur ansehen
segmented-choose = Auswählen
window-toolbar-show-sidebar = Seitenleiste einblenden
window-toolbar-hide-sidebar = Seitenleiste ausblenden
window-toolbar-show-inspector = Inspektor einblenden
window-toolbar-hide-inspector = Inspektor ausblenden
window-share-to-channel = In Kanal teilen…
window-stop-sharing-in = Teilen in { $place } beenden
history-loading = Verlauf wird geladen…
lazy-loading = Wird geladen…
history-genesis = Erstellt
history-moved = Zwischen Arbeitsbereichen verschoben
history-review-title = { $resource } wiederherstellen
history-review-confirm = Version wiederherstellen
history-review-refresh = Prüfung aktualisieren
history-review-cancel = Abbrechen
history-review-body = Version: { $version }

    Die Wiederherstellung bringt das Dokument als eine neue Änderung genau auf diese Version zurück. Spätere Änderungen bleiben im Verlauf, und Rückgängig macht die Wiederherstellung rückgängig.
history-review-selected-version = Ausgewählte Version
history-review-not-content = Diese Aktivität enthält keinen wiederherstellbaren Dokumentinhalt.
history-review-no-clock = Die Uhr dieses Computers ist nicht verfügbar, daher kann die Wiederherstellung keine Berechtigungen prüfen.
history-restore-unsupported = Diese App kann diese Version nicht sicher wiederherstellen.
history-preview-description = { $resource } in dieser Version

# The History host's own controls and status, beside the live app.
history-mode-unavailable = Verlauf nicht verfügbar
history-mode-content-unavailable = Früherer Inhalt nicht verfügbar
history-mode-back-to-activity = Zurück zur Aktivität
history-mode-back-to-live = Zurück zur aktuellen Version
history-mode-earlier = Frühere Änderungen anzeigen
history-mode-before = Vorher
history-mode-after = Nachher
history-mode-showing-before = Frühere Version: Vorher
history-mode-showing-after = Frühere Version: Nachher
history-mode-restore = Wiederherstellen…
history-mode-live-updates = Der aktuelle Inhalt wurde geändert
history-mode-page = Seite
history-mode-previous-page = Vorherige Seite
history-mode-next-page = Nächste Seite
history-mode-pending = „{ $action }“ wartet, bis du den Verlauf verlässt
history-mode-pending-unnamed = Eine Aktion wartet, bis du den Verlauf verlässt
history-mode-pending-held = „{ $action }“ wurde angehalten, während der Verlauf offen war
history-mode-pending-held-unnamed = Eine Aktion wurde angehalten, während der Verlauf offen war
history-mode-continue = Fortfahren
history-mode-confirm = Zulassen
history-mode-cancel = Abbrechen
history-mode-type-to-confirm = Gib „{ $phrase }“ ein, um zu bestätigen
history-mode-owner-gone = Dieses Dokument ist hier nicht mehr geöffnet
history-mode-custody-lost = Der Verlauf konnte nicht zurückholen, was diese Aktion gebraucht hätte
history-mode-control-owner-closed = Die App, die danach gefragt hat, ist geschlossen
history-mode-loading-media = Frühere Medien werden geladen
history-media-too-many = Diese Version hat zu viele Bilder zum Anzeigen
history-media-too-large = Die Bilder dieser Version sind zu groß zum Anzeigen
history-media-over-limit = Die Bilder dieser Version sind zu groß zum Anzeigen
history-media-hash-mismatch = Die Bilder dieser Version sind beschädigt und lassen sich nicht anzeigen

## History groups and catch-up briefs
history-group-changes = { $count ->
    [one] 1 Änderung
   *[other] { $count } Änderungen
}
history-group-edits = { $count ->
    [one] 1 Bearbeitung
   *[other] { $count } Bearbeitungen
}
history-group-comments = { $count ->
    [one] 1 Kommentar
   *[other] { $count } Kommentare
}
history-group-access-changes = { $count ->
    [one] 1 Berechtigungsänderung
   *[other] { $count } Berechtigungsänderungen
}
history-group-deletions = { $count ->
    [one] 1 Löschung
   *[other] { $count } Löschungen
}
history-group-over = { $count } in { $span }
history-group-chars = +{ $inserted } −{ $deleted } Zeichen
history-group-from = ab „{ $summary }“
history-group-named-by = benannt von { $name }
history-span-minutes = { $count } Min.
history-span-hours = { $count } Std.
history-span-days = { $count ->
    [one] 1 Tag
   *[other] { $count } Tagen
}
history-authors-two = { $first } und { $second }
history-authors-more = { $first }, { $second } + { $rest }
history-outcome-access = Arbeitsbereichszugriff geändert
history-outcome-discussion = Diskussion fortgesetzt
history-outcome-discussed = { $resource } besprochen
history-outcome-organized-content = Geteilte Inhalte aufgeräumt
history-outcome-organized = { $resource } aufgeräumt
history-outcome-updated-content = Geteilte Inhalte aktualisiert
history-outcome-worked-on = An { $resource } gearbeitet
history-outcome-workspace = Arbeitsbereich aktualisiert
history-chip-all = Alle
history-chip-edits = Bearbeitungen
history-chip-edits-short = Bearb.
history-chip-comments = Kommentare
history-chip-comments-short = Komm.
history-chip-access = Zugriff
history-chip-access-short = Zugriff
history-chip-deletions = Löschungen
history-chip-deletions-short = Lösch.
history-op-splice = Text und Struktur
history-op-edit-meta = Metadaten
history-op-move = Verschieben
history-op-comment = Kommentare
history-op-tombstone = Löschen
history-op-restore = Wiederherstellen
history-op-govern = Zugriff
history-op-compaction = Prüfpunkte
history-op-redact = Schwärzung
history-selection-count = { $count ->
    [one] 1 Änderung ausgewählt
   *[other] { $count } Änderungen ausgewählt
}
history-selection-name = Benennen
history-band-name = Benennen
history-band-rename = Umbenennen
history-band-undo = Rückgängig
history-band-restore = Wiederherstellen
history-entry-revert = Zurücknehmen
history-entry-copy = Kopieren
history-band-open = noch offen
history-band-shown = { $shown } von { $total } angezeigt
history-redacted = Entfernt von { $who } { $when }: { $reason }
history-empty = Noch keine Aktivität
history-empty-filtered = Keine Aktivität passt zu diesen Filtern
history-empty-hidden = { $count ->
    [one] Eine Änderung ist geladen, keine wird angezeigt
   *[other] { $count } Änderungen sind geladen, keine wird angezeigt
}
history-empty-hidden-by = { $count ->
    [one] Eine Änderung ist ausgeblendet durch { $filters }
   *[other] { $count } Änderungen sind ausgeblendet durch { $filters }
}
history-filter-dates = einen Zeitraum
history-filters-clear = Filter zurücksetzen
history-entries = Verlaufseinträge
history-author-anyone = Alle
history-work-task = Aufgabe
history-work-agent-run = Lauf
history-work-shortcut = Kurzbefehl
history-selection-clear = Aufheben
history-sync-finish-edit = Schließe deine Bearbeitung ab, bevor Änderungen anderer ankommen
history-sync-queue-full = Zu viele Änderungen anderer warten. Sie kommen an, sobald deine Bearbeitung abgeschlossen ist.
history-sync-owner-changed = Die Verbindung hat gewechselt, während Änderungen anderer ausstanden. Verbinde dich neu, um sie zu empfangen.
history-sync-offline = Offline. Änderungen werden synchronisiert, sobald du wieder verbunden bist.
history-sync-reconnecting = Verbinde. Änderungen werden synchronisiert, sobald die Verbindung steht.
history-sync-join-refused = Dieser Arbeitsbereich hat die App nicht beitreten lassen: { $reason }. Deine Änderungen bleiben auf diesem Gerät.
arriving-loading = Wird geladen
arriving-from = Von { $source }
arriving-received-of = { $received } von { $total }
arriving-received = { $received } empfangen
arriving-failed = Konnte nicht geladen werden
arriving-retry = Erneut versuchen
content-unavailable = Keines der verbundenen Geräte hat es
workspace-sync-connecting = Verbinde mit { $workspace }…
workspace-sync-connecting-unnamed = Verbinde…
workspace-sync-unreachable = { $workspace } ist gerade nicht erreichbar
workspace-sync-unreachable-unnamed = Der Host dieses Arbeitsbereichs ist gerade nicht erreichbar
workspace-sync-unreachable-body = Das hier ist, was dieses Gerät hat. Deine Änderungen bleiben hier und werden synchronisiert, sobald der Host wieder da ist.
workspace-sync-host-needs-update = Der Host dieses Arbeitsbereichs muss aktualisieren
workspace-sync-host-needs-update-body = Bis dahin wird hier nichts synchronisiert. Deine Änderungen bleiben auf diesem Gerät und erreichen ihn danach.
workspace-sync-device-needs-update = Tilo muss aktualisieren, um diesen Arbeitsbereich zu synchronisieren
workspace-sync-device-needs-update-body = Die Aktualisierung kommt. Bis dahin bleiben deine Änderungen auf diesem Gerät.
workspace-sync-shell-gone = Dieses Fenster hat die Verbindung zu Tilo verloren, daher kann der Inhalt dieses Arbeitsbereichs es nicht erreichen. Öffne es erneut.
workspace-sync-not-yet-synced = Noch nicht synchronisiert. Deine Änderungen bleiben auf diesem Gerät, bis der Host von { $workspace } sie hat.
workspace-sync-not-yet-synced-unnamed = Noch nicht synchronisiert. Deine Änderungen bleiben auf diesem Gerät, bis der Host des Arbeitsbereichs sie hat.
workspace-sync-cannot-keep = Synchronisieren gerade nicht möglich. Auf diesem Gerät ist kein Platz, um deine Änderungen an { $workspace } aufzubewahren.
workspace-sync-cannot-keep-unnamed = Synchronisieren gerade nicht möglich. Auf diesem Gerät ist kein Platz, um deine Änderungen aufzubewahren.
history-local-action-not-saved = Diese Änderung konnte nicht in deinem Verlauf gespeichert werden.
history-local-action-retry = Erneut versuchen
history-local-action-unrecorded = Deine Änderungen in diesem Fenster sind gespeichert, aber dein Verlauf zeichnet sie nicht auf.

history-sync-refused = Dieser Arbeitsbereich hat eine Änderung nicht angenommen. Alles andere wird weiter synchronisiert; die Änderung bleibt auf diesem Gerät.
history-sync-unreadable = Die geteilte Fassung dieses Eintrags konnte hier nicht gelesen werden. Deine eigenen Änderungen bleiben auf diesem Gerät und werden weiter gespeichert.
history-sync-remote-waiting = Änderungen anderer kommen an, sobald deine Bearbeitung abgeschlossen ist.
history-sync-owner-closed = Die Verbindung des Dokuments wurde geschlossen. Verbinde dich neu, um Änderungen anderer zu empfangen.

# File history in the History host.
history-files-select-file = Wähle eine Datei mit versioniertem Verlauf
history-files-choose-parent = Wähle, mit welcher früheren Version verglichen wird
history-files-no-earlier = Keine frühere Version
history-files-checking-compensation = Datei: { $file }

    Die Datei wird erneut geprüft
history-files-refresh-activity = Aktivität aktualisieren
history-files-check-restore = Ergebnis der Wiederherstellung prüfen
history-files-retry-restore = Dieselbe Wiederherstellung erneut versuchen
history-files-redo-restore = Dateiwiederherstellung wiederholen…
history-files-undo-restore = Dateiwiederherstellung rückgängig machen…
history-files-current-unavailable = Die aktuelle Version lässt sich nicht lesen
history-files-current-revision = Aktuelle Version: { $revision }
history-files-live-updates = Die Datei hat sich seither geändert. Die gewählte Version bleibt
history-files-before-parent = Mit Version { $revision } vergleichen
history-files-showing-parent = Vergleich mit Version { $revision }
history-files-created = Erstellt
history-files-edited = Bearbeitet
history-files-renamed = Umbenannt
history-files-moved = Verschoben
history-files-deleted = Gelöscht
history-files-restored = Wiederhergestellt
history-files-conflict-resolved = Konflikt gelöst
history-files-unknown-author = Unbekannte Person
history-files-author = Person { $identity }
history-files-loading = Dateiverlauf wird geladen
history-files-loading-version = Die Version wird geladen
history-files-checking-permissions = Berechtigungen werden geprüft…
history-files-earlier-pending = Eine frühere Wiederherstellung ist noch nicht abgeschlossen. Prüfe ihr Ergebnis
history-files-submitting = Wird wiederhergestellt…
history-files-version-count = { $count ->
        [one] { $count } Version
       *[other] { $count } Versionen
    }
history-files-binding-changed = Dieses Fenster zeigt jetzt eine andere Datei. Öffne den Verlauf erneut
history-files-unavailable-host = Diese Datei hat hier keinen Verlauf
history-files-historical-version = Frühere Version
history-files-foreign-response = Diese Verlaufsantwort gehört zu einer anderen Datei. Versuche es erneut
history-files-restore-explainer = Wiederherstellen erzeugt eine neue Version; die früheren bleiben im Verlauf
history-files-foreign-review = Diese Prüfung gehört zu einer anderen Datei. Versuche es erneut
history-files-invalid-result = Ob die Wiederherstellung lief, ist unklar. Prüfe den Verlauf der Datei
history-files-not-visible = Diese Datei ist nicht mehr sichtbar
history-files-error-stale = Der Verlauf der Datei hat sich geändert. Aktualisiere und versuche diese Version dann erneut
history-files-error-missing-version = Diese Version ist nicht mehr verfügbar
history-files-error-invalid = Der Verlauf dieser Datei ließ sich nicht lesen
history-files-error-unavailable = Der Dateiverlauf ist vorübergehend nicht verfügbar
history-files-error-too-large = Diese Version ist zu groß zum Anzeigen
history-files-error-no-content = Diese Version hat keinen Inhalt zum Anzeigen
history-files-outcome-applied = Version { $revision } wiederhergestellt. Die Datei zeigt ihren aktuellen Inhalt
history-files-outcome-unchanged = Die Datei entspricht bereits dieser Version. Es wurde nichts wiederhergestellt
history-files-outcome-pending = Die Wiederherstellung ist noch nicht abgeschlossen. Prüfe ihr Ergebnis oder versuche es erneut
history-files-outcome-review = Die Datei hat sich geändert. Aktualisiere die Prüfung; die gewählte alte Version bleibt ausgewählt
history-files-outcome-read-only = Diese Datei ist schreibgeschützt und lässt sich nicht wiederherstellen
history-files-outcome-pending-writes = Speichere deine Änderungen, bevor du wiederherstellst. Nichts Getipptes geht verloren
history-files-outcome-unsupported-owner = Diese App kann den Inhalt dieser Datei nicht wiederherstellen
history-files-outcome-unavailable = Die Wiederherstellung ist vielleicht nicht erfolgt. Prüfe den Verlauf der Datei, bevor du es erneut versuchst
history-files-outcome-invalid = Diese Wiederherstellung lief nicht. Versuche es erneut

close-title = Dokument schließen
close-unsaved = Änderungen vor dem Schließen speichern?
close-save-failed = Änderungen konnten nicht gespeichert werden
close-saving = Warten, bis das Speichern abgeschlossen ist…
close-save-cancelled = Speichern wurde abgebrochen. Das Dokument ist weiterhin geöffnet.
close-save = Speichern
close-discard = Änderungen verwerfen
close-retry = Erneut versuchen
close-keep-editing = Weiter bearbeiten
close-broker-disconnected = Die Verbindung zum Speicherdienst wurde getrennt. Das Dokument ist weiterhin geöffnet.

block-image-untitled = Bild
# The title a row shows for an item nobody has named yet.
list-row-untitled = Ohne Titel
list-row-check = { $title } als erledigt markieren
block-image-missing = Noch nicht auf diesem Gerät
notes-image-edit = Beschreibung bearbeiten
notes-image-done = Fertig
notes-image-source = Bildbeschreibung und Ziel
table-edit-source = Quelltext
table-edit-visual = Tabelle
table-add-row = Zeile hinzufügen
table-add-column = Spalte hinzufügen

block-editor-empty = Etwas schreiben…
block-editor-change-type = Blocktyp ändern
block-editor-copy-code-hint = Code kopieren

block-editor-code-copied = Kopiert
widget-breadcrumb = Pfad

widget-filepicker-cancel = Abbrechen
widget-filepicker-open = Öffnen
widget-filepicker-save = Speichern
widget-filepicker-choose-folder = Ordner auswählen
widget-filepicker-replace = Ersetzen
widget-filepicker-replace-title = „{ $name }“ ersetzen?
widget-filepicker-replace-body = Hier existiert bereits eine Datei namens „{ $name }“. Beim Ersetzen wird ihr Inhalt überschrieben.
widget-filepicker-replace-question = Hier existiert bereits eine Datei namens „{ $name }“. Ersetzen?
widget-filepicker-replace-detail = Beim Ersetzen wird ihr Inhalt überschrieben.
widget-filepicker-up = Nach oben
widget-filepicker-places = Orte
widget-filepicker-files = Dateien
widget-filepicker-folders = Ordner
widget-filepicker-file-type = Dateityp
widget-filepicker-all-files = Alle Dateien
widget-filepicker-name-label = Name
widget-filepicker-name-accessible = Dateiname
widget-filepicker-name-placeholder = Dateiname …
widget-filepicker-name-invalid = Das ist kein Dateiname.
widget-filepicker-name-no-folder = Es gibt keinen Ordner „{ $folder }“.
widget-filepicker-name-not-allowed = Diese App kann nicht in „{ $folder }“ speichern.
widget-filetable-column-name = Name
widget-filetable-column-size = Größe
widget-filetable-column-modified = Geändert
widget-filetable-column-kind = Art
widget-filepicker-empty = Dieser Ordner ist leer
widget-filepicker-place-home = Persönlicher Ordner
widget-filepicker-place-documents = Dokumente
widget-filepicker-place-downloads = Downloads
widget-filepicker-place-pictures = Bilder
widget-filepicker-place-music = Musik
widget-filepicker-place-videos = Videos
widget-filepicker-place-recent = Zuletzt
widget-filepicker-search = Suchen
widget-filepicker-search-results = Suchergebnisse
widget-filepicker-location = Ort
widget-filepicker-recent-empty = Hier wurde noch nichts ausgewählt
widget-filepicker-no-matches = Keine Treffer
widget-filepicker-wants-open-file = { $app } möchte eine Datei öffnen
widget-filepicker-wants-open-files = { $app } möchte Dateien öffnen
widget-filepicker-wants-save-file = { $app } möchte eine Datei speichern
widget-filepicker-wants-choose-folder = { $app } möchte einen Ordner verwenden
widget-filepicker-title-open-file = Datei öffnen
widget-filepicker-title-open-files = Dateien öffnen
widget-filepicker-title-save-file = Datei speichern
widget-filepicker-title-choose-folder = Ordner auswählen

widget-block-indent = Block mit Unterelementen einrücken
widget-block-text = Blocktext
widget-block-outdent = Block mit Unterelementen ausrücken
tree-rename-field = Umbenennen

# The show/hide control inside every password field.
text-input-show-password = Passwort anzeigen
text-input-hide-password = Passwort verbergen

# The password prompt every secret in Tilo is asked by.
secret-prompt-continue = Weiter
secret-prompt-cancel = Abbrechen
secret-prompt-username = Benutzername
secret-prompt-password = Passwort
secret-prompt-pin = PIN
secret-prompt-puk = PUK
secret-prompt-new-pin = Neue PIN
secret-prompt-confirm-pin = Neue PIN bestätigen
secret-prompt-caps-lock = Feststelltaste ist aktiv
secret-prompt-pin-length = { $min } bis { $max } Ziffern
secret-prompt-pin-length-exact = { $count } Ziffern
secret-prompt-attempts-left = { $count ->
    [one] Noch ein Versuch
   *[other] Noch { $count } Versuche
}
secret-prompt-pins-differ = Die neuen PINs stimmen nicht überein
secret-prompt-keypad-delete = Löschen
secret-prompt-keypad-enter = Fertig

# Authentication surfaces share the system theme and catalog.

# Die Namen, unter denen die beiden Auswahlen der Zeile „Jetzt läuft“ gelesen werden.
now-playing-choose-player = Player wählen
now-playing-choose-output = Wiedergabeort wählen

# Die eingebettete Berechtigungsabfrage einer isolierten App.
permission-prompt-allow = Erlauben
permission-prompt-session = Diese Sitzung
permission-prompt-always = Immer erlauben
permission-prompt-deny = Ablehnen
permission-prompt-requested = { $capability }, angefragt von { $app }

# Grobe relative Zeitangaben („vor 3 Tagen“) in Verlaufszeilen, Listen und Karten.
relative-ago-just-now = gerade eben
relative-ago-minutes = vor { $count } Min.
relative-ago-hours = vor { $count } Std.
relative-ago-yesterday = gestern
relative-ago-days = { $count ->
        [one] vor { $count } Tag
       *[other] vor { $count } Tagen
    }
relative-ago-weeks = { $count ->
        [one] vor { $count } Woche
       *[other] vor { $count } Wochen
    }
relative-ago-months = { $count ->
        [one] vor { $count } Monat
       *[other] vor { $count } Monaten
    }
relative-ago-years = { $count ->
        [one] vor { $count } Jahr
       *[other] vor { $count } Jahren
    }
relative-ahead-moment = gleich
relative-ahead-minutes = in { $count } Min.
relative-ahead-hours = in { $count } Std.
relative-ahead-tomorrow = morgen
relative-ahead-days = { $count ->
        [one] in { $count } Tag
       *[other] in { $count } Tagen
    }
relative-ahead-weeks = { $count ->
        [one] in { $count } Woche
       *[other] in { $count } Wochen
    }
relative-ahead-months = { $count ->
        [one] in { $count } Monat
       *[other] in { $count } Monaten
    }
relative-ahead-years = { $count ->
        [one] in { $count } Jahr
       *[other] in { $count } Jahren
    }

# Dates, times and numbers (ui::format). Months are 1 = January … 12 =
# December; weekdays are 1 = Monday … 7 = Sunday.
format-month-short = { $month ->
        [1] Jan.
        [2] Feb.
        [3] März
        [4] Apr.
        [5] Mai
        [6] Juni
        [7] Juli
        [8] Aug.
        [9] Sept.
        [10] Okt.
        [11] Nov.
       *[12] Dez.
    }
format-month-name = { $month ->
        [1] Januar
        [2] Februar
        [3] März
        [4] April
        [5] Mai
        [6] Juni
        [7] Juli
        [8] August
        [9] September
        [10] Oktober
        [11] November
       *[12] Dezember
    }
format-weekday-short = { $weekday ->
        [1] Mo.
        [2] Di.
        [3] Mi.
        [4] Do.
        [5] Fr.
        [6] Sa.
       *[7] So.
    }
format-weekday-name = { $weekday ->
        [1] Montag
        [2] Dienstag
        [3] Mittwoch
        [4] Donnerstag
        [5] Freitag
        [6] Samstag
       *[7] Sonntag
    }
# A day this year in a list: "Jun 30".
format-month-day = { $day }. { $month }
# A day this year and a time of day: "30. Juni 14:05".
format-month-day-time = { $day }. { $month } { $time }
# A day in another year in a list: "Jun 30, 2025".
format-month-day-year = { $day }. { $month } { $year }
# The "Long" date style: "30 Jun 2026".
format-date-long = { $day }. { $month } { $year }
# The default date style, the language's own convention: "Jun 30, 2026" (de "30.06.2026").
# $day and $month2 / $day2 are the zero-padded numbers, $month the short month name.
format-date-language = { $day2 }.{ $month2 }.{ $year }
# A date and a time of day, the date numeric ("2026-06-30 13:42") or worded ("30 Jun 2026, 13:42").
format-date-time-numeric = { $date } { $time }
format-date-time-worded = { $date }, { $time }
format-date-time-language = { $date }, { $time }
# The top of a day: "Tuesday, June 30" and "Tuesday, June 30, 2026".
format-day-heading = { $weekday }, { $day }. { $month }
format-day-heading-year = { $weekday }, { $day }. { $month } { $year }
# A day column or compact header: "Tue, Jun 30".
format-day-heading-short = { $weekday }, { $day }. { $month }
# A span of days within one month: "Jun 23 – 29".
format-day-range-in-month = { $start }.–{ $end }. { $month }
# Any span between two dates or times: "Jun 29 – Jul 5".
format-range = { $start } – { $end }
# Two already-formatted values in a list: "verschlüsselt, 134 GB frei".
format-list-join = { $head }, { $tail }
# The top of a month grid: "June 2026".
format-month-heading = { $month } { $year }
# The clock the language reads by default: 12 or 24 (here 24-hour).
format-clock-hours = 24
# The halves of a 12-hour clock: "2:05 PM".
format-time-am = AM
format-time-pm = PM
# How long something takes or took: "1 hr 5 min".
format-duration-days = { $count ->
        [one] { $count } Tag
       *[other] { $count } Tage
    }
format-duration-hours = { $count } Std.
format-duration-minutes = { $count } Min.
format-duration-seconds = { $count } Sek.
# How many things a folder, a selection or a result holds: "12 items".
format-items = { $count ->
        [one] { $formatted } Objekt
       *[other] { $formatted } Objekte
    }
format-percent = { $value } %
# Abbreviated large counts: "12.3K", "4.1M", "1.1B".
format-compact-thousands = { $value } Tsd.
format-compact-millions = { $value } Mio.
format-compact-billions = { $value } Mrd.
# Narrow relative times for tight list columns: "now", "3m", "2h", "4d", "2w", "3mo", "1y".
relative-compact-now = jetzt
relative-compact-minutes = { $count } Min.
relative-compact-hours = { $count } Std.
relative-compact-days = { $count } T.
relative-compact-weeks = { $count } Wo.
relative-compact-months = { $count } Mon.
relative-compact-years = { $count } J.

# Ein Software-Teilnehmer in Verlauf, Präsenz und Aktivität.
history-run-step-separator = {", "}
participant-agent = { $name } · Agent
participant-shortcut = { $name } · Kurzbefehl
participant-app = { $name } · App
participant-bot = { $name } · Bot
# Zwei oder mehr Personen in einem Arbeitsbereich tragen denselben Namen: jede
# wird nummeriert, für alle im Arbeitsbereich gleich.
participant-numbered = { $name } ({ $number })
interaction-agent = Agent
interaction-collaborator = Mitwirkende Person
# Was jemand gerade tut: der Name, dann die Handlung im Präsens.
interaction-editing-field = bearbeitet { $field }
interaction-added-text = Text hinzugefügt
interaction-working = arbeitet
interaction-action-doing = { $name }: { $doing }
# Dasselbe mit der Phase, wo sie nur ein Mensch klären kann.
interaction-action-status = { $name }: { $doing }. { $phase }
interaction-inspecting = Prüft
interaction-applying = Wendet an
interaction-waiting = Wartet
interaction-committed = Übernommen
interaction-failed = Fehlgeschlagen
interaction-cancelled = Abgebrochen
interaction-progress = { $label } - { $done }/{ $total }

# Was ein Arbeitsschritt ist, wie eine Person ihn neben dem Gesicht der
# handelnden Person, in der Personenliste und auf der Karte am Ort der
# Handlung liest: die Art Arbeit, die die Handlung selbst angibt, an dem
# Datensatz oder Eintrag, an dem sie geschieht; der Eintrag wird mit seinem
# Titel benannt, wo der Schritt einen nennt.
step-creating = Legt in { $record } an
# Ein Anlegen, dessen Angabe die Sammlung nennt, in die es schreibt, sagt, was
# entsteht: mit dem Substantiv, das der Katalog der App dafür schreibt, samt
# Artikel ("Legt eine Aufgabe in Aufgaben an"). "Erstellt" läse sich als
# Vergangenheit, deshalb sagt das Verb, was gerade geschieht.
step-creating-one = Legt { $noun } in { $record } an
step-writing = Schreibt in { $record }
step-editing = Bearbeitet { $record }
step-deleting = Löscht { $record }
step-moving = Verschiebt { $record }
step-reading = Liest { $record }
step-running = Führt { $command } aus
step-running-in = Führt in { $record } aus
step-working = Arbeitet in { $record }
step-working-alone = Arbeitet

# Was ein Lauf gerade tut, wenn es kein Schritt in einem Datensatz ist: auf
# seine Person warten, die Apps oder eine davon ansehen, nach Wörtern suchen.
step-waiting = Wartet
step-looking-at = Sieht { $what } an
step-looking-at-apps = Sieht die Apps an
step-searching = Sucht nach { $what }
# Eine mitwirkende Person, deren Schreibmarke ein Textfeld zeichnet: wo sie
# ist, für eine Vorlesehilfe gesagt. Bei einer Auswahl steht das stattdessen.
presence-writing-here = { $name } schreibt hier
presence-selecting-here = { $name } markiert hier
participant-unknown-agent = Unbekannter Agent


# Befehlspalette: Tastenhinweise, Ergebniszahl und leere Zustände.
palette-name = Befehlspalette
palette-argument-name = Aktionsbestätigung
palette-results = Ergebnisse
palette-placeholder = Suchen…
palette-empty = Nichts anzuzeigen
palette-no-results = Keine Ergebnisse für „{ $query }“
palette-no-results-hint = Schreibweise prüfen oder ein anderes Wort versuchen
palette-searching = Suche läuft…

# Kalender-Abschnitte in der Arbeitsbereich-Aktivität.
day-yesterday = Gestern
day-today = Heute
day-tomorrow = Morgen
day-overdue = Überfällig
day-past = { $overdue } seit { $when }
day-at-time = { $day } { $time }
history-day-this-week = Diese Woche
history-day-earlier = Früher

save-status-stalled = Noch nicht gespeichert. Neuer Versuch…
save-status-not-saved = Nicht gespeichert
save-status-not-saved-named = „{ $document }“ wurde nicht gespeichert
save-status-not-saved-notice = Deine Änderungen sind noch in { $app }.
save-status-saved = Gespeichert
save-status-failed = Speichern fehlgeschlagen
save-status-failed-detail = Deine letzten Änderungen sind noch nicht gespeichert.
save-status-retry = Erneut versuchen

# Suchleiste: das Band oben in einem Dokument, das darin sucht.
find-bar-name = Suchen
find-bar-placeholder = Suchen
find-bar-replace-placeholder = Ersetzen durch
find-bar-match-case = Groß-/Kleinschreibung
find-bar-whole-word = Ganzes Wort
find-bar-regex = Regulärer Ausdruck
find-bar-text = Text
find-bar-hex = Hex
find-bar-previous = Vorheriger Treffer
find-bar-next = Nächster Treffer
find-bar-close = Suche schließen
find-bar-replace = Ersetzen
find-bar-replace-all = Alle ersetzen
find-bar-position = { $current } von { $total }
find-bar-no-matches = Keine Treffer
widget-mention-agent = Agent
widget-mention-shortcut = Kurzbefehl
widget-mention-app = App
widget-mention-bot = Bot
widget-mention-list = Personen zum Erwähnen
rich-text-reference-unreadable = Nicht mit dir geteilt
rich-text-edit-not-kept = Deine letzte Änderung wurde nicht übernommen
rich-text-too-long = Der eingefügte Text ist zu lang (höchstens { $limit }). Füge ihn in kleineren Stücken ein.
rich-text-changed-while-typing = Dieser Absatz hat sich beim Tippen geändert. Deine Änderung wurde nicht übernommen.
widget-block-duplicate = Block mit Unterelementen duplizieren

widget-block-duplicate-selection = Ausgewählte oder fokussierte Blöcke duplizieren

widget-block-move-selection-up = Ausgewählte Blöcke nach oben verschieben
widget-block-move-selection-down = Ausgewählte Blöcke nach unten verschieben

block-callout-note = Hinweis
block-callout-tip = Tipp
block-callout-important = Wichtig
block-callout-warning = Warnung
block-callout-caution = Vorsicht

slash-menu-name = Blocktypen
slash-menu-filter = Blöcke filtern…
slash-paragraph = Absatz
slash-heading-1 = Überschrift 1
slash-heading-2 = Überschrift 2
slash-heading-3 = Überschrift 3
slash-bullet = Aufzählung
slash-numbered = Nummerierte Liste
slash-checklist = Checkliste
slash-page = Unternotiz
slash-link-page = Notiz verlinken
slash-embed-page = Notiz einbetten
slash-image = Bild
slash-quote = Zitat
slash-code = Codeblock
slash-table = Tabelle
slash-tex = TeX-Formel
slash-typst = Typst-Formel
slash-divider = Trennlinie
link-popover-name = Notizen verlinken
link-popover-filter = Notiz finden…
filter-menu-no-matches = Keine Treffer

widget-sidebar-resize = Breite der Seitenleiste ändern
widget-inspector-resize = Breite des Inspektors ändern
widget-inspector = Inspektor
widget-sheet-height = Höhe
widget-sheet-peek = Abgesenkt
widget-sheet-half = Halbe Höhe
widget-sheet-full = Volle Höhe

dictation-record-once = Einmal aufnehmen
dictation-finish = Fertig
dictation-cancel = Abbrechen

dictation-copy-transcript = Transkript kopieren
dictation-discard-transcript = Transkript verwerfen

dropdown-no-matches = Keine passende Auswahl
host-native-history-unobserved = Rückgängig ist für diese Seite noch nicht bereit.
host-native-history-unconfirmed = Die Seite hat ihr Rückgängig nicht bestätigt. Prüfe die Seite und versuche es dann erneut.
host-native-page-history-scope = Rückgängig und Wiederholen wirken auf diese Seite. Deine letzte Änderung kann in einem anderen Feld liegen.

# Shared native screen topology canvas
display-arrangement-name = Bildschirmanordnung
display-arrangement-description = Drück auf einen Bildschirm, um ihn auszuwählen, und zieh ihn neben einen anderen, um ihn dort zu platzieren. Per Tastatur wählt die Bildschirmauswahl einen aus, und die Pfeiltasten verschieben ihn.
display-arrangement-empty = Keine Bildschirme zum Anordnen
display-arrangement-item = Bildschirm { $number }: { $name }
display-arrangement-item-primary = Bildschirm { $number }: { $name }, Hauptbildschirm
display-arrangement-item-position = { $width } × { $height } bei { $x }, { $y }

device-display-session-unavailable = Die Tilo-Sitzung ist nicht verfügbar.
device-display-request-pending = Eine Anfrage zur Bildschirmposition steht bereits aus.
device-display-session-disconnected = Die Verbindung zur Tilo-Sitzung wurde getrennt.

app-activate-window = Dieses Fenster anzeigen
app-activate-window-description = Das Fenster dieser App in den Vordergrund holen.

terminal-service-control-unavailable = Der Terminaldienst antwortet nicht. Versuche es erneut.
terminal-service-protocol-unavailable = Dieses Fenster braucht ein neueres Tilo auf diesem Computer. Aktualisiere und versuche es dann erneut.
terminal-service-invalid-session = Das Terminal ist nicht gestartet. Versuche es erneut.
terminal-service-missing-lease = Das Terminal ist nicht gestartet. Versuche es erneut.
terminal-service-missing-terminal = Das Terminal ist nicht gestartet. Versuche es erneut.
terminal-service-invalid-lease = Die Verbindung des Terminals konnte nicht geprüft werden. Versuche es erneut.

# Shown instead of an app's view while its record does not read as the app expects.
framework-unreadable = Dies kann nicht geöffnet werden
framework-unreadable-hint = Diese Version der App kann nicht lesen, was hier gespeichert ist. Es wurde nichts verändert; ein Update der App kann es öffnen.
# The disclosure under an empty or error state that holds what went wrong technically.
empty-state-details = Details
file-open-drop-hint = Oder eine Datei hier ablegen
file-open-release = Zum Öffnen loslassen
# The page rail's title, above its thumbnails, unless the app names it.
page-sidebar-title = Seiten
widget-documents-in-workspace = In diesem Arbeitsbereich
# Shown instead of a window's document while it cannot be opened.
record-unavailable-title = Dieses Dokument kann nicht geöffnet werden
record-unavailable-hint = Es ist auf diesem Gerät gerade nicht verfügbar. Vielleicht ist es noch unterwegs, oder es wurde verschoben oder gelöscht.
display-arrangement-offline = Offline
display-arrangement-excluded = Nicht im Layout
display-arrangement-offline-excluded = Offline, nicht in der Anordnung
display-arrangement-item-status = { $position }; { $status }
display-arrangement-caption-status = { $status }: { $name }

# Suche: die Zeile, die den Rest einer gekürzten Gruppe zeigt
search-show-all = Alle { $count } zeigen
search-no-matches = Keine Treffer
# Was eine Gruppenüberschrift und ihr Chip über die Ergebnisse sagen
search-kind-actions = Aktionen
search-kind-apps = Apps
search-kind-notes = Notizen
search-kind-records = Datensätze
search-kind-files = Dateien
search-kind-folders = Ordner
search-kind-creators = Creator
search-kind-videos = Videos
search-kind-playlists = Wiedergabelisten
search-kind-tabs = Tabs
search-kind-window-groups = Fenstergruppen
window-root-name = Tilo-Anwendungsfenster
# Was das leere Feld der Palette sagt, worüber die Suche geht, nicht wie es heißt.
action-palette-query = Aktionen und Ergebnisse durchsuchen
search-field-filter = Filtern
search-field-placeholder = Suchen
# Wie das Feld selbst heißt, abseits des Platzhalters darin.
search-field-name = Suche
search-field-filter-name = Filter
presentation-window-title = Präsentationsfenster
approved-check-reports-disabled = Läuft

author-bot-chip = Bot
author-via-chip = über { $service }
author-from-service = Aus { $service }
author-from-service-as = Aus { $service } als { $handle }
author-bot-details = { $bot } (Bot, hinzugefügt von { $installer })
author-bot-details-as = { $name }, über { $bot } (Bot, hinzugefügt von { $installer })
author-bot-linked = Mit { $person } auf Tilo verknüpft
author-persona-suffix = { $name }, über { $service }

# Das Eingabefeld eines Kontextmenüs: Tippen schränkt seine Einträge ein, und
# seine Senden-Zeile übernimmt das Getippte in die Sendebox neben dem Menü.
menu-query-placeholder = Tippen zum Filtern
menu-query-name = Filtern oder senden
# The last row of every context menu, which opens the send-to box; once
# something is typed it names the sentence it carries.
menu-send = Senden an…
menu-ask-typed = Senden: { $text }

# Die Sendebox: an wen eine Nachricht geht, worum es geht und ihre Worte. Sie
# steht neben dem Menü, dessen Senden-Zeile sie geöffnet hat, und für eine
# Meldung allein.
send-to-name = Senden an
# The same box opened to the one place reports go.
send-to-report-name = Problem melden
send-to-new-agent = Neuer Agent
send-to-recipients-name = Jemanden hinzufügen
send-to-search-placeholder = Jemanden hinzufügen…
send-to-no-results = Nichts gefunden
send-to-add-subject = Betreff hinzufügen
send-to-message-name = Nachricht
send-to-message-placeholder = Nachricht schreiben
send-to-report-placeholder = Was ist schiefgelaufen?
send-to-send = Senden

widget-screenshot-saved = Gespeichert
widget-screenshot-copied = Kopiert
widget-screenshot-copied-saved = Kopiert und gespeichert
widget-screenshot-copy = Kopieren
widget-screenshot-edit = Bearbeiten
widget-screenshot-open = Öffnen
widget-screenshot-reveal = In Dateien zeigen
widget-screenshot-send = Senden…
widget-capture-title = Erfassen
widget-capture-dismiss = Schließen
widget-capture-mode-screenshot = Bildschirmfoto
widget-capture-mode-recording = Aufnahme
widget-capture-mode-pip = BiB
widget-capture-mode-pip-name = Bild im Bild
widget-capture-source-region = Auswahl
widget-capture-source-window = Fenster
widget-capture-source-display = Bildschirm
widget-capture-source-strip = Leiste
widget-capture-options = Optionen
widget-capture-delay = Verzögerung
widget-capture-delay-none = Ohne Verzögerung
widget-capture-delay-choice = { $seconds } Sekunden
widget-capture-pointer = Zeiger zeigen
widget-capture-system-audio = Systemton
widget-capture-microphone = Mikrofon
widget-capture-keep-recent = Letzte 30 s behalten
widget-capture-combine = Auswahlen kombinieren
widget-capture-layout-auto = Angeordnet
widget-capture-layout-grid = Raster
widget-capture-layout-stack = Gestapelt
widget-capture-take = Bildschirmfoto aufnehmen
widget-capture-record = Aufnahme starten
widget-capture-stop = Aufnahme beenden
widget-capture-create-pip = Bild im Bild erstellen
widget-capture-remove-selection = Diese Auswahl entfernen
widget-recording-saved = Aufnahme gespeichert
widget-recording-skipped = { $count ->
    [one] 1 Bild wurde übersprungen: die Festplatte war zu langsam
   *[other] { $count } Bilder wurden übersprungen: die Festplatte war zu langsam
}
widget-recording-incomplete = Aufnahme unvollständig: { $reason }
widget-recording-not-saved = Aufnahme nicht gespeichert: { $reason }
widget-recording-play = Abspielen
widget-replay-save = Letzte 30 s speichern
widget-replay-stop = Letzte 30 s nicht mehr behalten
widget-screenshot-send-workspace = In den Dateien dieses Arbeitsbereichs speichern
widget-screenshot-send-no-device = Kein anderes Gerät ist online
widget-screenshot-send-device = An { $device } senden
widget-markup-title = Markieren
widget-markup-close = Schließen
widget-markup-undo = Rückgängig
widget-markup-redo = Wiederholen
widget-markup-tools-pen = Stiftwerkzeuge
widget-markup-tools-highlighter = Markerwerkzeuge
widget-markup-tools-arrow = Pfeilwerkzeuge
widget-markup-tools-rectangle = Formwerkzeuge
widget-markup-tools-redact = Schwärzungswerkzeuge
widget-markup-tool-pen = Stift
widget-markup-tool-highlighter = Marker
widget-markup-tool-arrow = Pfeil
widget-markup-tool-rectangle = Rechteck
widget-markup-tool-redact = Schwärzen
widget-markup-fit = Bild einpassen
widget-markup-colors = Farben
widget-markup-color-red = Rot
widget-markup-color-yellow = Gelb
widget-markup-color-blue = Blau
widget-markup-color-green = Grün
widget-markup-color-white = Weiß
widget-markup-open = In App öffnen
widget-markup-copy = Kopieren
widget-markup-save = Kopie speichern

## Block editor
block-a11y-formula-source = Quelltext-Editor
block-a11y-formula-visual = Visueller Editor
block-a11y-copy-lang = { $lang }-Code kopieren
block-a11y-move-up = Block nach oben
block-a11y-move-down = Block nach unten
block-a11y-open-embed = Eingebettete Notiz { $title } öffnen
block-a11y-embed-description = Live-Vorschau einer verlinkten Notiz
block-a11y-table-column = Spalte { $column }
block-a11y-table-cell = Zelle { $row }, { $column }
block-a11y-table-header-row = Kopfzeile
block-a11y-table-row = Zeile { $row }
block-a11y-table = Tabelle
block-a11y-open-link = Verlinkte Notiz öffnen
block-a11y-open-external = Weblink öffnen
block-a11y-open-resource = { $label } öffnen
block-a11y-block = { $kind }-Block { $position }
block-a11y-level = Ebene { $level }
block-a11y-document = Ziehe den Griff eines Blocks, um ihn zu verschieben. Alt+Pfeil hoch und Alt+Pfeil runter verschieben den fokussierten Block. Tab rückt ihn unter den Block darüber ein, Umschalt+Tab wieder aus; Escape, dann Tab, verlässt den Editor.
block-formula-empty = Gib oben eine Formel ein, um sie zu sehen
block-formula-diagnostic = Vorschau: { $message }
block-formula-source = Quelltext
block-formula-visual = Visuell
block-kind-paragraph = Absatz
block-kind-quote = Zitat
block-kind-heading = Überschrift
block-kind-bullet = Aufzählungspunkt
block-kind-numbered = Nummerierter Eintrag
block-kind-checklist = Checklisten-Eintrag
block-kind-code = Codeblock
block-kind-table = Tabelle
block-kind-divider = Trennlinie
block-kind-callout = Hinweisbox
block-kind-named-callout = Hinweisbox { $name }
block-embed-missing-title = Notiz nicht gefunden
block-embed-missing = Die verlinkte Notiz wurde gelöscht.
block-kind-code-short = Code
block-table-add-cell = Zelle hinzufügen

window-search-name = In diesem Fenster suchen
window-search-placeholder = Suchen
window-search-app = In { $app } suchen
window-search-this-app = In dieser App suchen
window-search-everywhere = Überall suchen
window-search-close = Suche schließen
window-search-options = Suchoptionen
window-search-replace-options = Suchoptionen und Ersetzen
history-panel-title = Verlauf
history-panel-selected-changes = { $count } Änderungen
history-mode-restore-change = Nur diese Änderung wiederherstellen
history-panel-name = Diese Änderungen benennen
history-preview-after = Frühere Version, schreibgeschützt
history-preview-before = Vor dieser Änderung, schreibgeschützt
history-preview-now = Zurück zu jetzt

# A form's date field: what it reads as with no date, its calendar, the button that empties it,
# and the row that picks the time of day the date is due at.
date-field-add = Datum hinzufügen
date-field-calendar = Kalender
date-field-clear = Datum entfernen
date-field-named = { $label }: { $day }
time-field-name = Tageszeit
time-field-add = Zeit hinzufügen
time-field-remove = Zeit entfernen

person-field-none = Niemand
person-field-me = Ich

# Das Menü, das ein Rechtsklick oder langes Drücken auf bearbeitbarem Text öffnet.
text-menu-cut = Ausschneiden
text-menu-copy = Kopieren
text-menu-paste = Einfügen
text-menu-paste-plain = Ohne Formatierung einfügen
text-menu-select-all = Alles auswählen
text-menu-bold = Fett
text-menu-italic = Kursiv
text-menu-underline = Unterstrichen
text-menu-strikethrough = Durchgestrichen
text-menu-heading-1 = Überschrift 1
text-menu-heading-2 = Überschrift 2
text-menu-heading-3 = Überschrift 3
text-menu-body-text = Fließtext
text-menu-bulleted-list = Aufzählung
text-menu-numbered-list = Nummerierte Liste

# A reference to something another app keeps, shown as a chip; $label is its name.
reference-open = { $label } öffnen

# What someone else (an agent's run, mostly) changed that the person has not seen yet.
change-field = Geändert von { $who }
change-field-before = Geändert von { $who }. Vorher: { $before }
change-mark-who = { $who }
change-mark-created = Erstellt von { $who }
change-mark-changed = Geändert von { $who }
change-mark-removed = Entfernt von { $who }
change-mark-in-run = { $what } in „{ $run }“
change-window = { $count ->
    [one] Eine Änderung von { $who }
   *[other] { $count } Änderungen von { $who }
}
change-window-changed = Geändert von { $who }: { $count ->
    [one] { $count } { $one }
   *[other] { $count } { $noun }
}
change-window-new = Neu von { $who }: { $count ->
    [one] { $count } { $one }
   *[other] { $count } { $noun }
}
change-window-new-more = Neu von { $who }: mindestens { $count ->
    [one] { $count } { $one }
   *[other] { $count } { $noun }
}
change-mark-names-two = { $first } und { $second }
change-mark-names-three = { $first }, { $second } und { $third }
change-mark-names-more = { $first }, { $second } und { $count } weitere
change-mark-said = { $who } { $said }
change-mark-said-more = { $said } und { $count ->
    [one] eine weitere
   *[other] { $count } weitere
}
change-window-more = Mindestens { $count ->
    [one] eine Änderung
   *[other] { $count } Änderungen
} von { $who }
change-window-changed-more = Geändert von { $who }: mindestens { $count ->
    [one] { $count } { $one }
   *[other] { $count } { $noun }
}

# One step of an agent's task: the program it ran, what it answered, what it
# changed and what it proposes.
program-step-program = Programm, das der Agent ausgeführt hat
program-step-show-all = Alle { $count } Zeilen zeigen
program-step-undo = Rückgängig
program-step-approve = Genehmigen
program-step-decline = Ablehnen
program-step-waits = Wartet auf deine Bestätigung.
program-step-planned = Noch nichts geändert. Nach der Bestätigung ändert sich:

# A signal meter's name, by how strong the signal is (a Wi-Fi or mobile network).
signal-meter-unknown = Signalstärke unbekannt
signal-meter-weak = Schwaches Signal
signal-meter-fair = Mittleres Signal
signal-meter-good = Gutes Signal
signal-meter-strong = Starkes Signal

reaction-strip-add = Reaktion hinzufügen
reaction-strip-react = Reaktion { $emoji } hinzufügen
reaction-strip-unreact = Reaktion { $emoji } entfernen
reaction-strip-count = { $count ->
    [one] Eine Reaktion
   *[other] { $count } Reaktionen
}
reaction-strip-you = Du
reaction-strip-reacted-one = { $first } hat mit { $emoji } reagiert
reaction-strip-reacted-two = { $first } und { $second } haben mit { $emoji } reagiert
reaction-strip-reacted-three = { $first }, { $second } und { $third } haben mit { $emoji } reagiert
reaction-strip-reacted-more = { $first }, { $second } und { $others ->
    [one] eine weitere Person
   *[other] { $others } weitere
} haben mit { $emoji } reagiert

emoji-chooser-title = Emoji
emoji-chooser-search = Emoji suchen
emoji-chooser-groups = Emoji-Kategorien
emoji-chooser-workspace = Arbeitsbereich
emoji-chooser-recent = Zuletzt verwendet
emoji-chooser-results = Passende Emoji
emoji-chooser-no-results = Keine Emoji gefunden
emoji-chooser-arriving = Weitere Emoji kommen gleich
emoji-chooser-add = Emoji hinzufügen
emoji-bar = Schnelle Reaktionen
emoji-more = Weitere Emoji
emoji-chooser-add-failed = Das Bild konnte nicht hinzugefügt werden: { $reason }
emoji-chooser-tones = Hauttöne
emoji-chooser-tone-hint = Hautton wählen
emoji-chooser-toned = { $name }, { $tone }
emoji-chooser-tone-light = heller Hautton
emoji-chooser-tone-medium-light = mittelheller Hautton
emoji-chooser-tone-medium = mittlerer Hautton
emoji-chooser-tone-medium-dark = mitteldunkler Hautton
emoji-chooser-tone-dark = dunkler Hautton

## The workspace's own emoji
workspace-emoji-choose-title = Emoji hinzufügen
workspace-emoji-choose-accept = Hinzufügen
workspace-emoji-image-files = Bilder
workspace-emoji-loading = die Bildwerkzeuge laden noch
workspace-emoji-not-a-picture = es ist kein PNG, JPEG oder WebP
workspace-emoji-too-detailed = es ist zu detailreich für ein Emoji
workspace-emoji-unavailable = die Emoji dieses Arbeitsbereichs sind nicht verfügbar
workspace-emoji-not-allowed = du darfst hier keine Emoji hinzufügen
# Der Gesprächs-Grundbaustein: Zustellvokabular und die Anhangsleiter.
conversation-delivery-queued = In der Warteschlange
conversation-delivery-sending = Wird gesendet
conversation-delivery-submitted = Übermittelt
conversation-delivery-sent = Gesendet
conversation-delivery-delivered = Zugestellt
conversation-delivery-read = Gelesen
conversation-delivery-failed = Nicht gesendet
conversation-delivery-uncertain = Status unklar
conversation-new-messages = Neue Nachrichten
conversation-day-new = { $day }: Neue Nachrichten
conversation-read-by = Bis hierher gelesen von { $names }
conversation-jump-unread = Zu den ersten ungelesenen
conversation-jump-present = Zum Neuesten
conversation-earlier-unavailable = Frühere Nachrichten sind auf diesem Gerät nicht verfügbar
conversation-attachment-open = Öffnen
conversation-attachment-open-full-size = In voller Größe öffnen
conversation-attachment-save-as = Speichern unter …
conversation-attachment-open-saved = Gespeicherte Kopie öffnen
conversation-attachment-no-access = Diese Datei ist nicht für dich freigegeben
conversation-attachment-unavailable = Diese Datei ist nicht auf diesem Gerät
conversation-attachment-uploading = Wird hochgeladen
conversation-attachment-cancel-upload = Cancel upload
conversation-attachment-saved = eine Kopie auf diesem Gerät
conversation-attachment-image = Image
conversation-attachment-video = Video
conversation-attachment-file = File
conversation-draft = Draft
# Jobs: what is running. A job's row says what it is doing and how far.
jobs-starting = Wird gestartet
jobs-phase-of = { $phase }: { $done } von { $total }
jobs-phase-seen = { $phase }: { $seen }
jobs-percent = { $percent } %
jobs-bar-of = { $done } von { $total }
jobs-failed = Nicht fertig geworden: { $reason }
jobs-stopped = Angehalten
jobs-stop = Anhalten
jobs-stopping = Wird angehalten
jobs-show-me = Zeigen
jobs-read = Gelesen
jobs-more = Noch { $count }
jobs-running-heading = Läuft
jobs-finished-heading = Beendet
jobs-none = Es läuft nichts

jobs-files-one = { $count } Datei
jobs-files = { $count } Dateien
jobs-items-one = { $count } Element
jobs-items = { $count } Elemente
jobs-steps-one = { $count } Schritt
jobs-steps = { $count } Schritte

# Needs you: what wants a person.
attention-blocking = Jetzt
attention-waiting = Wartet
attention-noted = Gut zu wissen
attention-none = Nichts braucht dich
attention-because = { $about }, { $because }
attention-defer = Für später beiseitelegen
attention-deferred = { $count } beiseitegelegt
attention-pill = { $count } warten
action-document-replaced = Diese Aktion wurde nicht ausgeführt, weil ihr Dokument erneut geöffnet oder ersetzt wurde. Versuche es im aktuellen Dokument erneut.

# One-shot action proposals: one model call's answer, before it is written.
proposal-model-on-device = { $model }, läuft auf diesem Gerät
proposal-model-hosted = { $model }, läuft auf einem anderen Rechner
proposal-context-selection = Die Auswahl
proposal-context-too-many = Das ist zu viel Kontext für einen Schritt. Wähle weniger.
# $name is the part, $limit how many kilobytes one part may carry.
proposal-context-too-long = { $name } ist länger als { $limit } KB. Wähle weniger.
proposal-diff-more =
    { $count ->
        [one] Eine weitere Zeile wird nicht gezeigt
       *[other] { $count } weitere Zeilen werden nicht gezeigt
    }
proposal-accept = Übernehmen
proposal-reject = Verwerfen
proposal-decisions = Diese Änderung übernehmen oder verwerfen

# Spoken when Ctrl+Super+Enter (or both volume keys held) turns the screen reader on or off.
screen-reader-turned-on = Bildschirmleser an
screen-reader-turned-off = Bildschirmleser aus

## Completion drawers
completion-in-conversation = In diesem Gespräch
completion-members = Mitglieder
completion-no-match = Keine Treffer für „{ $query }“
completion-results = { $count ->
    [one] 1 Treffer
   *[other] { $count } Treffer
}
emoji-picker-add-reaction = Reaktion hinzufügen
quote-dismiss = Nicht mehr antworten
quote-open = Zeigen, worauf dies antwortet

toast-reply-placeholder = Antworten…

## Window furniture and widget chrome, named where a screen reader reads it
floating-window-move-hint = Bewegliches Fenster. Alt halten und mit den Pfeiltasten verschieben; mit Umschalt in größeren Schritten.
widget-dialog-close = Schließen
widget-dialog-cancel = Abbrechen
widget-dismiss = Ausblenden
widget-hand-to-agent = An Agenten übergeben
widget-more = Mehr
widget-copy = Kopieren
widget-hosted-applications = Gehostete Anwendungen
widget-searching = Wird gesucht…
widget-search-results = Suchergebnisse
widget-loading = Wird geladen
widget-timeline-name = Position im Zeitstrahl
widget-timeline-hint = Chronologische Sammlung. Pfeiltasten bewegen, Bild auf und Bild ab bewegen einen Bildschirm, Pos1 und Ende springen zum Anfang oder Ende; Plus und Minus ändern die Dichte.
widget-timeline-jump = Zu { $marker } springen
widget-timeline-group = Chronologischer Index
widget-timeline-group-hint = Nach Datum geordnete Bildlaufleiste
widget-shortcut-change = Tastenkürzel ändern
widget-shortcut-unavailable = Dieses Tastenkürzel ist nicht verfügbar. Aktivieren, um ein anderes zu wählen.
widget-chart-series = { $name } Datenreihe
widget-step-of = Schritt { $step } von { $total }
widget-steps-count = { $count ->
    [one] { $count } Schritt
   *[other] { $count } Schritte
  }
widget-page-of = Seite { $page } von { $total }
widget-preview-named = Vorschau von { $label }
widget-type-to-confirm = Zum Bestätigen { $phrase } eingeben
widget-color-name = Farbe
widget-color-hex = Hex-Farbe
widget-crop-name = Seite zuschneiden
widget-crop-hint = Von { $left }% bis { $right }% in der Breite und { $top }% bis { $bottom }% in der Höhe
widget-document-switch-blocked = Dokumentwechsel blockiert
widget-qr-failed = QR-Code konnte nicht gezeichnet werden: { $error }
media-volume = Lautstärke
media-position = Position
media-more-controls = Weitere Steuerungen
media-play = Wiedergeben
media-pause = Pausieren
media-previous = Vorheriger Titel
media-next = Nächster Titel
media-mute = Stummschalten
media-unmute = Ton einschalten
media-like = Gefällt mir
media-unlike = Gefällt mir nicht mehr
media-lyrics = Songtext
media-shuffle = Zufällige Wiedergabe
media-repeat = Wiederholen
media-repeat-one = Titel wiederholen
media-repeat-all = Alle wiederholen
media-queue = Warteschlange
media-player = Videoplayer
media-no-video = Kein Video geladen
media-player-hint = Leertaste gibt wieder oder pausiert; Pfeil links und rechts springen; Pfeil hoch und runter ändern die Lautstärke; M schaltet stumm; F schaltet Vollbild
media-key-play-pause = Leertaste
media-position-value = { $position } von { $duration }
media-fullscreen-enter = Vollbild öffnen
media-fullscreen-exit = Vollbild verlassen
media-playback-settings = Wiedergabeeinstellungen
media-toggle-on = Ein
media-toggle-off = Aus
floating-window-title = Schwebendes Fenster
now-playing-seek = Wiedergabeposition
now-playing-seek-help = Mit Pfeil links und rechts um fünf Sekunden springen

## What the records are called, and the actions on them
settings-text-assistance = Rechtschreibung und Text
