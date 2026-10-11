counter = Wert: { $count }
btn_1 = Klick Mich
tab-modified = Nicht gespeicherte Änderungen

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
host-window-close = Schließen
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
history-mode-pending = Ausstehend #{ $id } von { $owner }: { $action } mit Argumenten { $arguments }
history-mode-continue = Fortfahren
history-mode-confirm = Bestätigen
history-mode-cancel = Abbrechen
history-mode-confirmation-unavailable = Bestätigung nicht verfügbar
history-mode-type-to-confirm = Gib { $phrase } ein, um zu bestätigen
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
history-op-splice = Text / Struktur
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
history-selection-clear = Aufheben
history-sync-finish-edit = Schließe die aktuelle Bearbeitung ab, bevor entfernte Änderungen empfangen werden
history-sync-queue-full = Die Warteschlange für entfernte Änderungen ist voll. Nach der aktuellen Bearbeitung ist ein neuer Stand für die Synchronisierung nötig.
history-sync-owner-changed = Verbindung oder Besitzer des Dokuments hat sich geändert, während entfernte Änderungen ausstanden. Verbinde dich neu und synchronisiere, bevor du fortfährst.
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
history-files-outcome-applied-legacy = Version { $revision } wiederhergestellt (Vorgang { $intent }). Der aktuelle zusammengeführte Dateiinhalt wird getrennt angezeigt. Vorheriger Verweis: älterer Inhaltsvertreter; frühere Urheberschaft unbekannt.
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

notes-image-edit = Bild bearbeiten
notes-image-done = Fertig
notes-image-source = Bildbeschreibung und Ziel
table-edit-source = Quelltext
table-edit-visual = Tabelle
table-add-row = Zeile hinzufügen
table-add-column = Spalte hinzufügen

block-editor-drag-hint = Klicken für Blocktyp · Ziehen zum Verschieben · Alt+↑/↓
block-editor-find-placeholder = In Notiz suchen
block-editor-find-no-results = Keine Ergebnisse
block-editor-find-previous = Vorheriger Treffer
block-editor-find-next = Nächster Treffer
block-editor-find-close = Suche schließen
block-editor-change-type = Blocktyp ändern
block-editor-copy-code-hint = Code kopieren

block-editor-code-exit-hint = Ctrl+Enter: Code verlassen
block-editor-code-exit-compact = Ctrl+Enter ↵
block-editor-code-copied = Kopiert
block-editor-code-keyboard-hint = Ctrl+Enter: darunter schreiben · Ctrl+Shift+Enter: darüber schreiben · Tab / Shift+Tab: einrücken / ausrücken · Ctrl+Shift+C: Code kopieren
widget-breadcrumb = Pfad
widget-block-indent = Block mit Unterelementen einrücken
widget-block-text = Blocktext
widget-block-outdent = Block mit Unterelementen ausrücken
widget-maximized-restore = Wiederherstellen
tree-rename-field = Umbenennen
tree-move-up = Nach oben verschieben
tree-move-down = Nach unten verschieben
tree-move-indent = In das Element darüber verschieben
tree-move-outdent = Aus dem übergeordneten Element verschieben

# The show/hide control inside every password field.
text-input-show-password = Passwort anzeigen
text-input-hide-password = Passwort verbergen

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

# Relative Zeitangaben in Verlaufs- und Aktivitätszeilen.
relative-time-just-now = gerade eben
relative-time-minutes = { $count ->
        [one] vor { $count } Min.
       *[other] vor { $count } Min.
    }
relative-time-hours = { $count ->
        [one] vor { $count } Std.
       *[other] vor { $count } Std.
    }
relative-time-days = { $count ->
        [one] vor { $count } Tag
       *[other] vor { $count } Tagen
    }

# Grobe relative Zeitangaben („vor 3 Tagen“) in Listen und Karten.
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

# Ein Software-Teilnehmer in Verlauf, Präsenz und Aktivität.
history-run-step-separator = {", "}
participant-agent = { $name } · Agent
participant-shortcut = { $name } · Kurzbefehl
participant-app = { $name } · App
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
history-action-param-resource = Das Dokument, dessen Verlauf verwendet wird, wie der Verlauf es aufführt. Standardmäßig das geöffnete Dokument.
history-action-param-cursor = Wo die vorherige Seite endete.
history-action-param-entry = Eine Änderung, wie der Verlauf sie aufführt.
history-action-param-entries = Änderungen, wie der Verlauf sie aufführt.
history-action-param-session = Eine mit „Version ansehen“ geöffnete Vorschau.
history-action-param-heads = Die Live-Heads, die die Vorschau gemeldet hat. Die Wiederherstellung wird abgelehnt, wenn sich das Dokument seitdem geändert hat.
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
palette-hint-navigate = Navigieren
palette-hint-select = Auswählen
palette-hint-open = Öffnen
palette-hint-run = Ausführen
palette-hint-close = Schließen
palette-result-count = { $count ->
    [one] 1 Ergebnis
   *[other] { $count } Ergebnisse
}
palette-empty = Nichts anzuzeigen
palette-no-results = Keine Ergebnisse für „{ $query }“
palette-no-results-hint = Weniger Wörter versuchen oder den Bereich erweitern

# Kalender-Abschnitte in der Arbeitsbereich-Aktivität.
history-day-today = Heute
history-day-yesterday = Gestern
history-day-this-week = Diese Woche
history-day-earlier = Früher

save-status-saving = Wird gespeichert…
save-status-saved = Gespeichert
save-status-failed = Speichern fehlgeschlagen
save-status-retry = Erneut versuchen
save-status-dismiss = Schließen

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
widget-block-duplicate = Block mit Unterelementen duplizieren

widget-block-duplicate-selection = Ausgewählte oder fokussierte Blöcke duplizieren

widget-block-move-selection-up = Ausgewählte Blöcke nach oben verschieben
widget-block-move-selection-down = Ausgewählte Blöcke nach unten verschieben

block-editor-replace-placeholder = Ersetzen durch
block-editor-replace = Ersetzen

block-editor-replace-all = Alle ersetzen
block-editor-replace-all-help = Alle ersetzen — Umschalt+Enter im Ersetzungsfeld

widget-sidebar-resize = Breite der Seitenleiste ändern
widget-inspector-resize = Breite des Inspektors ändern

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
display-arrangement-description = Zieh einen Bildschirm neben einen anderen, um ihn dort zu platzieren. Das Positionsmenü jedes Bildschirms erledigt dasselbe per Tastatur.
display-arrangement-empty = Keine Bildschirme zum Anordnen
display-arrangement-item = Bildschirm { $number }: { $name }
display-arrangement-item-primary = Bildschirm { $number }: { $name }, primär
display-arrangement-item-position = { $width } × { $height } bei { $x }, { $y }

device-display-session-unavailable = Die Tilo-Sitzung ist nicht verfügbar.
device-display-request-pending = Eine Anfrage zur Bildschirmposition steht bereits aus.
device-display-session-disconnected = Die Verbindung zur Tilo-Sitzung wurde getrennt.

app-activate-window = Dieses Fenster anzeigen
app-activate-window-description = Die Plattform bitten, das vorhandene Fenster anzuzeigen, ohne den Dokumentbesitzer zu wechseln.

# Shown instead of an app's view while its record does not read as the app expects.
framework-unreadable = Dies kann nicht geöffnet werden
display-arrangement-offline = Offline
display-arrangement-excluded = Nicht im Layout
display-arrangement-offline-excluded = Offline · Nicht im Layout
display-arrangement-item-status = { $position }; { $status }
display-arrangement-caption-status = { $status } · { $name }

# Suche: Einstiegsgruppen in der leeren Anwendungsliste
search-apps-group-start-here = Zum Einstieg
search-apps-group-everyday = Alltag
search-apps-group-work = Arbeit
search-apps-group-media = Medien
search-apps-group-system-tools = Systemwerkzeuge
search-apps-group-more-apps = Weitere Apps
search-apps-group-developer-tools = Entwicklerwerkzeuge
