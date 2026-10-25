counter = Wert: { $count }
tab-modified = Nicht gespeicherte Änderungen
tab-search-in = In { $title } suchen
tab-held-by = Fenster von { $name }
tab-controls-desktop = Steuert deinen Desktop

standalone-history-title = Verlauf
standalone-history-restore = Diese Version wiederherstellen…
standalone-history-restore-title = Diese Version wiederherstellen?
standalone-history-restore-body = Das Dokument kehrt zum Stand nach „{ $version }“ zurück. Dabei entsteht eine neue Änderung: Spätere Versionen bleiben im Verlauf, und Rückgängig macht die Wiederherstellung rückgängig.
standalone-history-restore-confirm = Version wiederherstellen
standalone-history-restore-cancel = Abbrechen
standalone-history-restore-action = Version wiederherstellen
standalone-history-restore-done = Version wiederhergestellt. Rückgängig macht es rückgängig.
standalone-history-restore-stale = Das Dokument hat sich während der Prüfung geändert. Wähle die Version erneut aus, um sie wiederherzustellen.
standalone-history-restore-pending = Schließe die aktuelle Bearbeitung ab und stelle die Version dann wieder her.
standalone-history-restore-denied = Nur Bearbeitende können eine Version wiederherstellen.
standalone-history-restore-unavailable = Diese Version kann hier nicht wiederhergestellt werden.
standalone-history-preview-resource = Dokument
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
host-history-compare = Auswahl vergleichen
host-history-recent = { $author } · { $change } — { $when }
host-history-unavailable = Es gibt kein Dokument, dessen Verlauf angezeigt werden kann.
host-history-compare-unavailable = Wähle zwei oder mehr Änderungen aus, um sie zu vergleichen.
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
host-window-close = Schließen
window-toolbar-back = Zurück
window-toolbar-forward = Vorwärts
window-toolbar-go = Öffnen
window-toolbar-more = Weitere Aktionen
window-toolbar-views = Ansicht
window-toolbar-suggestions = Vorschläge
window-toolbar-edited = Bearbeitet
segmented-choose = Auswählen
window-toolbar-show-sidebar = Seitenleiste einblenden
window-toolbar-hide-sidebar = Seitenleiste ausblenden
window-toolbar-show-inspector = Inspektor einblenden
window-toolbar-hide-inspector = Inspektor ausblenden
history-genesis = Erstellt
history-edited-field = { $more ->
    [0] { $path } bearbeitet
   *[other] { $path } und { $more } weitere bearbeitet
}
history-removed-field = { $more ->
    [0] { $path } entfernt
   *[other] { $path } und { $more } weitere entfernt
}
history-edited-fields = { $count ->
    [one] 1 Feld bearbeitet
   *[other] { $count } Felder bearbeitet
}
history-removed-fields = { $count ->
    [one] 1 Feld entfernt
   *[other] { $count } Felder entfernt
}
history-review-title = { $resource } wiederherstellen
history-review-confirm = Version wiederherstellen
history-review-refresh = Prüfung aktualisieren
history-review-cancel = Abbrechen
history-review-body = Version: { $version }

    Die Wiederherstellung bringt das Dokument als eine neue Änderung genau auf diese Version zurück. Spätere Änderungen bleiben im Verlauf, und Rückgängig macht die Wiederherstellung rückgängig.
history-review-selected-version = Ausgewählte Version
history-review-not-content = Diese Aktivität enthält keinen wiederherstellbaren Dokumentinhalt.
history-review-no-clock = Die aktuelle Uhrzeit ist nicht verfügbar. Die Wiederherstellung kann den Ablauf von Berechtigungen nicht prüfen.
history-restore-unsupported = Diese Version kann mit dem Inhaltsformat dieser App nicht sicher wiederhergestellt werden.
history-preview-description = { $resource } in dieser Version

# The History host's own controls and status, beside the live app.
history-mode-title = Verlauf
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
history-mode-confirm = Bestätigen
history-mode-cancel = Abbrechen
history-mode-type-to-confirm = Gib „{ $phrase }“ ein, um zu bestätigen
history-mode-owner-gone = Der Besitzer des Verlaufs ist nicht mehr verfügbar
history-mode-custody-lost = Die ursprüngliche Komponente konnte während des Verlaufs nicht wiederhergestellt werden
history-mode-control-owner-closed = Der ursprünglich registrierte Besitzer des Steuerelements ist geschlossen
history-mode-loading-media = Frühere Medien werden geladen
history-media-too-many = Die früheren Medien überschreiten die Vorschaugrenze von 4096 Dateien
history-media-too-large = Die früheren Medien sind zu groß
history-media-over-limit = Die früheren Medien überschreiten die Vorschaugrenze von 128 MiB
history-media-hash-mismatch = Die früheren Medien stimmen nicht mit ihrem gespeicherten Inhalts-Hash überein

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
history-work-agent-run = Agentenlauf
history-work-shortcut = Kurzbefehl
history-selection-clear = Aufheben
history-sync-finish-edit = Schließe die aktuelle Bearbeitung ab, bevor entfernte Änderungen empfangen werden
history-sync-queue-full = Die Warteschlange für entfernte Änderungen ist voll. Nach der aktuellen Bearbeitung ist ein neuer Stand für die Synchronisierung nötig.
history-sync-owner-changed = Verbindung oder Besitzer des Dokuments hat sich geändert, während entfernte Änderungen ausstanden. Verbinde dich neu und synchronisiere, bevor du fortfährst.
history-sync-offline = Offline. Änderungen werden synchronisiert, sobald du wieder verbunden bist.
arriving-loading = Wird geladen
arriving-from = Von { $source }
arriving-received-of = { $received } von { $total }
arriving-received = { $received } empfangen
arriving-failed = Konnte nicht geladen werden
arriving-retry = Erneut versuchen
content-unavailable = Keines der verbundenen Geräte hat es
room-sync-connecting = Verbinde mit { $workspace }…
room-sync-connecting-unnamed = Verbinde…
room-sync-host-needs-update = Der Host dieses Arbeitsbereichs muss aktualisieren
room-sync-host-needs-update-body = Bis dahin wird hier nichts synchronisiert. Deine Änderungen bleiben auf diesem Gerät und erreichen ihn danach.
room-sync-device-needs-update = Tilo muss aktualisieren, um diesen Arbeitsbereich zu synchronisieren
room-sync-device-needs-update-body = Die Aktualisierung kommt. Bis dahin bleiben deine Änderungen auf diesem Gerät.
room-sync-shell-gone = Dieses Fenster hat die Verbindung zu Tilo verloren, daher kann der Inhalt dieses Raums es nicht erreichen. Öffne es erneut.
history-local-action-not-saved = Diese Änderung konnte nicht in deinem Verlauf gespeichert werden.
history-local-action-retry = Erneut versuchen
history-local-action-unrecorded = Deine Änderungen in diesem Fenster sind gespeichert, aber dein Verlauf zeichnet sie nicht auf.

history-sync-refused = Synchronisierung angehalten: Dieser Arbeitsbereich hat eine Änderung nicht angenommen. Deine Änderungen bleiben auf diesem Gerät.
history-sync-remote-waiting = Entfernte Änderungen werden übernommen, sobald die aktuelle Bearbeitung abgeschlossen ist.
history-sync-owner-closed = Der Besitzer des Dokuments wurde geschlossen oder hat gewechselt. Verbinde dich neu, bevor du den Nachfolger synchronisierst.

# File history in the History host.
history-files-select-file = Wähle eine Datei mit versioniertem Verlauf
history-files-choose-parent = Wähle für „Vorher“ eine tatsächliche Vorgängerversion
history-files-no-earlier = Keine frühere Version
history-files-checking-compensation = Datei: { $file }

    Eine neue Prüfung zum Rückgängigmachen läuft
history-files-refresh-activity = Aktivität aktualisieren
history-files-check-restore = Ergebnis der Wiederherstellung prüfen
history-files-retry-restore = Dieselbe Wiederherstellung erneut versuchen
history-files-redo-restore = Dateiwiederherstellung wiederholen…
history-files-undo-restore = Dateiwiederherstellung rückgängig machen…
history-files-current-unavailable = Aktuelle Dateiversion nicht verfügbar
history-files-current-revision = Aktuelle Dateiversion: { $revision }
history-files-live-updates = Die aktuelle Datei wurde geändert; die ausgewählte unveränderliche Version bleibt angeheftet
history-files-before-parent = Vorgänger { $revision }
history-files-showing-parent = Vorgänger: { $revision }
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
history-files-loading-version = Unveränderliche Version wird geladen
history-files-checking-permissions = Aktuelle Dateiberechtigungen und Stand werden geprüft
history-files-earlier-pending = Eine frühere Wiederherstellung steht noch aus; prüfe ihr genaues Ergebnis
history-files-submitting = Geprüfte Wiederherstellung wird übermittelt
history-files-version-count = { $count ->
        [one] { $count } unveränderliche Version
       *[other] { $count } unveränderliche Versionen
    }
history-files-binding-changed = Die Dateizuordnung hat sich geändert; öffne den Verlauf für die aktuelle Datei erneut
history-files-unavailable-host = Der Dateiverlauf ist auf diesem Host nicht verfügbar
history-files-historical-version = Frühere Version
history-files-foreign-response = Die Verlaufsantwort gehört zu einer anderen Datei
history-files-restore-explainer = Die Wiederherstellung erzeugt eine neue Dateiversion und behält den bisherigen Verlauf
history-files-foreign-review = Die Prüfung gehört zu einer anderen Datei oder Einbindung
history-files-invalid-result = Ungültiges Ergebnis vom Backend; die genaue Anfrage bleibt offen
history-files-not-visible = Diese Datei ist nicht mehr sichtbar
history-files-error-stale = Der Dateiverlauf hat sich geändert; aktualisiere die Aktivität und versuche diese Version erneut
history-files-error-missing-version = Diese unveränderliche Version ist nicht verfügbar
history-files-error-invalid = Ungültige Verlaufsantwort
history-files-error-unavailable = Der Dateiverlauf ist vorübergehend nicht verfügbar
history-files-error-too-large = Die frühere Vorschau überschreitet die unterstützte Größe
history-files-error-no-content = Diese Version hat keinen gespeicherten Inhaltsverweis
history-files-outcome-applied = Version { $revision } wiederhergestellt (Vorgang { $intent }). Der aktuelle zusammengeführte Dateiinhalt wird getrennt angezeigt. Vorheriger Verweis: beobachtete Version.
history-files-outcome-unchanged = Der ausgewählte Inhalt stimmt bereits überein; keine neue Version und kein Rückgängig-Eintrag
history-files-outcome-pending = Die Wiederherstellung steht aus. Prüfe das Ergebnis oder versuche denselben Vorgang erneut
history-files-outcome-review = Die aktuelle Datei hat sich geändert. Aktualisiere die Prüfung; die ausgewählte alte Version bleibt angeheftet
history-files-outcome-read-only = Diese Datei ist schreibgeschützt; Wiederherstellen und Rückgängigmachen sind nicht verfügbar
history-files-outcome-pending-writes = Ausstehende Dateiänderungen müssen abgeschlossen sein, bevor wiederhergestellt wird; gepufferter Inhalt bleibt erhalten
history-files-outcome-unsupported-owner = Nicht unterstützter Inhaltsbesitzer: { $mapping }
history-files-outcome-unavailable = Die Bestätigung der Wiederherstellung ist nicht verfügbar. Behalte diesen Vorgang und prüfe sein Ergebnis
history-files-outcome-invalid = Ungültige geprüfte Wiederherstellungsanfrage; es gibt keinen älteren Ausweichweg

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
widget-maximized-restore = Wiederherstellen
widget-maximized-fullscreen = Vollbild
tree-rename-field = Umbenennen
tree-move-up = Nach oben verschieben
tree-move-down = Nach unten verschieben
tree-move-indent = In das Element darüber verschieben
tree-move-outdent = Aus dem übergeordneten Element verschieben

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
lock-welcome-back = Willkommen zurück

# Now playing: Quick Settings and the lock screen.
now-playing-title = Wird abgespielt
now-playing-play = Abspielen
now-playing-pause = Pausieren
now-playing-previous = Vorheriger Titel
now-playing-next = Nächster Titel
now-playing-seek = Wiedergabeposition
now-playing-seek-help = Mit Pfeil links und rechts um fünf Sekunden springen
now-playing-choose-player = Player wählen
now-playing-choose-output = Wiedergabeort wählen
now-playing-output-local = Auf diesem Computer
now-playing-output-device = Eines deiner Geräte
now-playing-output-speaker-group = Eine deiner Lautsprechergruppen
now-playing-route-gone = Dieser Wiedergabeort ist nicht mehr verfügbar

# Die eingebettete Berechtigungsabfrage einer isolierten App.
permission-prompt-allow = Erlauben
permission-prompt-session = Diese Sitzung
permission-prompt-always = Immer erlauben
permission-prompt-deny = Ablehnen
permission-prompt-requested = { $capability } – angefragt von { $app }

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
# Zwei oder mehr Personen im Raum tragen denselben Namen: jede wird
# nummeriert, für alle im Raum gleich.
participant-numbered = { $name } ({ $number })
# Ein Agent, ein Kurzbefehl oder eine App, die für eine Person handelt.
participant-acting-for = { $name }, für { $person }
interaction-agent = Agent
interaction-collaborator = Mitwirkende Person
interaction-action-status = { $action } ({ $phase }) - { $name }
interaction-inspecting = Prüft
interaction-applying = Wendet an
interaction-waiting = Wartet
interaction-committed = Übernommen
interaction-failed = Fehlgeschlagen
interaction-cancelled = Abgebrochen
interaction-progress = { $label } - { $done }/{ $total }
participant-unknown-agent = Unbekannter Agent

# Der Verlauf als veröffentlichte Aktionen, für Palette, Skripte, Agenten und Hilfstechnologien.
history-action-resource = Dokumentverlauf
history-action-open = Verlauf öffnen
history-action-list = Änderungen auflisten
history-action-view-at = Version ansehen
history-action-compare = Änderungen vergleichen
history-action-close = Vorschau schließen
history-action-restore = Version wiederherstellen
history-action-name = Änderungen benennen
history-action-param-resource = Das Dokument, dessen Verlauf verwendet wird, wie der Verlauf es aufführt. Standardmäßig das geöffnete Dokument.
history-action-param-cursor = Wo die vorherige Seite endete.
history-action-param-entry = Eine Änderung, wie der Verlauf sie aufführt.
history-action-param-entries = Änderungen, wie der Verlauf sie aufführt.
history-action-param-session = Eine mit „Version ansehen“ geöffnete Vorschau.
history-action-param-heads = Die Live-Heads, die die Vorschau gemeldet hat. Die Wiederherstellung wird abgelehnt, wenn sich das Dokument seitdem geändert hat.
history-action-param-first = Die erste Änderung der Reihe, wie der Verlauf sie aufführt.
history-action-param-last = Die letzte Änderung der Reihe, wie der Verlauf sie aufführt.
history-action-param-label = Der Name der Reihe. Leer lassen, um den Namen zu entfernen.
history-action-denied = Du hast für diesen Teil des Dokumentverlaufs keine Berechtigung.
history-action-stale-document = Dieses Dokument ist nicht mehr geöffnet.
history-action-stale-cursor = Der Verlauf hat sich seit dieser Seite geändert. Liste ihn erneut von vorne auf.
history-action-unknown-entry = Diese Änderung ist nicht im Verlauf dieses Dokuments.
history-action-unknown-session = Diese Vorschau ist nicht geöffnet.
history-action-invalid = Diese Verlaufsanfrage ist ungültig.

# Befehlspalette: Tastenhinweise, Ergebniszahl und leere Zustände.
palette-name = Befehlspalette
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
history-day-this-week = Diese Woche
history-day-earlier = Früher

save-status-stalled = Noch nicht gespeichert. Neuer Versuch…
save-status-not-saved = Nicht gespeichert
save-status-not-saved-named = „{ $document }“ wurde nicht gespeichert
save-status-not-saved-notice = Deine Änderungen sind noch in { $app }.
save-status-saved = Gespeichert
save-status-failed = Speichern fehlgeschlagen
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
host-native-history-unobserved = Der Bearbeitungsverlauf des ursprünglichen Felds wurde noch nicht bestätigt.
host-native-history-unconfirmed = Die Verlaufsänderung dieser Seite wurde nicht bestätigt. Prüfe die Seite, bevor du es erneut versuchst.
host-native-page-history-scope = Rückgängig und Wiederholen gelten für den Bearbeitungsverlauf dieser Seite. Die letzte Änderung kann in einem anderen Feld liegen.

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
app-activate-window-description = Die Plattform bitten, das vorhandene Fenster anzuzeigen, ohne den Dokumentbesitzer zu wechseln.

terminal-service-control-unavailable = Die Shell-Verbindung ist nicht verfügbar.
terminal-service-protocol-unavailable = Aktualisiere den Shell-Dienst, bevor du in diesem Fenster eine Sitzung öffnest.
terminal-service-invalid-session = Der Shell-Dienst hat eine ungültige Sitzung zurückgegeben.
terminal-service-missing-lease = Die Shell-Sitzung hat keine Verbindung für ihre Laufzeit bereitgestellt.
terminal-service-missing-terminal = Die Shell-Sitzung hat kein Terminal bereitgestellt.
terminal-service-invalid-lease = Die Verbindung der Shell-Sitzung konnte nicht überprüft werden.

# Shown instead of an app's view while its record does not read as the app expects.
framework-unreadable = Dies kann nicht geöffnet werden
framework-unreadable-hint = Diese Version der App kann nicht lesen, was hier gespeichert ist. Es wurde nichts verändert; ein Update der App kann es öffnen.
# The disclosure under an empty or error state that holds what went wrong technically.
empty-state-details = Details
file-open-drop-hint = Oder eine Datei hier ablegen
file-open-release = Zum Öffnen loslassen
# Shown instead of a window's document while it cannot be opened.
record-unavailable-title = Dieses Dokument kann nicht geöffnet werden
record-unavailable-hint = Es ist auf diesem Gerät gerade nicht verfügbar. Vielleicht ist es noch unterwegs, oder es wurde verschoben oder gelöscht.
display-arrangement-offline = Offline
display-arrangement-excluded = Nicht im Layout
display-arrangement-offline-excluded = Offline, nicht in der Anordnung
display-arrangement-item-status = { $position }; { $status }
display-arrangement-caption-status = { $status } · { $name }

# Suche: die Zeile, die den Rest einer gekürzten Gruppe zeigt
search-show-all = Alle { $count } zeigen
search-no-matches = Keine Treffer
approved-check-reports-disabled = Phasenmeldungen nicht verfügbar

author-bot-chip = Bot
author-via-chip = über { $service }
author-bot-details = { $bot } (Bot, hinzugefügt von { $installer })
author-bot-details-as = { $name }, über { $bot } (Bot, hinzugefügt von { $installer })
author-bot-linked = Mit { $person } auf Tilo verknüpft
author-persona-suffix = { $name } · { $service }

# Context menu entry: tell an agent to do something with what the menu is about.
tell-agent = Einem Agenten sagen…

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
workspace-identity-required-title = Wähle, wer du hier bist
workspace-identity-required-body = Dieser Arbeitsbereich muss wissen, wer darin arbeitet, bevor { $app } öffnen kann.
workspace-identity-required-choose = Identität wählen

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
text-menu-bulleted-list = Aufzählung
text-menu-numbered-list = Nummerierte Liste

# A reference to something another app keeps, shown as a chip; $label is its name.
reference-open = { $label } öffnen

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
