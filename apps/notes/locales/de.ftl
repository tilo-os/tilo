notes-created-date = Erstellt { $date }
notes-edited-date = Bearbeitet { $date }
notes-dates = Erstellt { $created }, bearbeitet { $edited }
notes-date-today = heute um { $time }
notes-table-add-row = Zeile darunter einfügen
notes-table-add-column = Spalte danach einfügen
notes-table-delete-row = Zeile löschen
notes-table-delete-column = Spalte löschen
notes-surface-tag = Notizen mit dem Schlagwort { $tag } anzeigen
notes-surface-backlink = Verlinkende Notiz öffnen: { $title }
notes-parent-path = In { $path }
notes-linked-from = Verlinkt von
notes-title-placeholder = Ohne Titel
notes-save-failed-detail = Deine letzten Änderungen sind noch nicht gespeichert.
notes-affordance-new-note = Neue Notiz
notes-affordance-new-child = Neue Unternotiz
notes-affordance-note-actions = Weitere Notizaktionen

notes-scroll-position = Scrollposition der Notiz
notes-scroll-help = Zum Scrollen ziehen. Pfeiltasten bewegen schrittweise; Bild auf und Bild ab blättern; Pos1 und Ende springen zum Anfang oder Ende.
notes-title = Notizen
notes-menu-new-child = Neue Unternotiz
notes-menu-rename = Umbenennen
notes-menu-duplicate = Duplizieren
notes-menu-copy-link = Notizlink kopieren
notes-menu-export = Markdown exportieren
notes-menu-insert-image = Bild einfügen
notes-menu-delete = Löschen
notes-a11y-new-child-under = Neue Unternotiz in
notes-a11y-more-actions-for = Weitere Aktionen für

## Verlauf: was jede Änderung getan hat, nach Feld und nach Aktion

notes-page-body = { $item } bearbeitet
notes-page-tags = { $op ->
    [insert] { $item } verschlagwortet
   *[other] Schlagwort von { $item } entfernt
}
notes-tag-name = Schlagwort { $old } in { $new } umbenannt
notes-notebook-pages = { $op ->
    [insert] Notiz hinzugefügt
    [remove] Notiz entfernt
   *[other] Notiz verschoben
}
notes-notebook-tags = { $op ->
    [insert] Schlagwort hinzugefügt
   *[other] Schlagwort entfernt
}
notes-new-note = { $title } erstellt
notes-deleted = { $inside ->
    [0] { $title } gelöscht
    [one] { $title } und 1 Notiz darin gelöscht
   *[other] { $title } und { $inside } Notizen darin gelöscht
}
notes-undo = Rückgängig
notes-new-child = { $title } in { $item } erstellt
notes-rename = { $item } in { $title } umbenannt
notes-delete = { $item } gelöscht
notes-move-note = { $item } verschoben
notes-move-up = { $item } nach oben verschoben
notes-move-down = { $item } nach unten verschoben
notes-indent = { $item } eingerückt
notes-outdent = { $item } ausgerückt
notes-duplicate = { $item } dupliziert
notes-new-tag = Schlagwort { $name } erstellt
notes-tag = { $item } mit { $name } verschlagwortet
notes-delete-tag = Schlagwort { $item } gelöscht
notes-keep-image = Ein Bild im Notizbuch abgelegt
notes-place-image = Bild zu { $item } hinzugefügt
notes-import-markdown = { $title } importiert
notes-append-to-note = Zu { $title } hinzugefügt

## Keyboard shortcuts sheet
notes-help-title = Tastenkürzel
notes-help-group-notes = Notizen
notes-help-group-writing = Schreiben
notes-help-group-formatting = Formatierung
notes-help-group-code = Codeblöcke
notes-help-group-help = Hilfe
notes-help-new-note = Neue Notiz
notes-help-new-sub-note = Neue Unternotiz
notes-help-jump = Zu einer Notiz
notes-help-back-forward = Zurück oder vor
notes-help-rename = Notiz umbenennen
notes-help-note-actions = Notizaktionen
notes-help-note-move = Notiz nach oben oder unten
notes-help-note-indent = Notiz ein- oder ausrücken
notes-help-delete = Notiz löschen
notes-help-find = In Notiz suchen
notes-help-block-menu = Blockmenü
notes-help-link = Notiz verlinken
notes-help-embed = Notiz einbetten
notes-help-outline-indent = Ein- oder ausrücken
notes-help-block-indent = Mit Unterelementen ein- oder ausrücken
notes-help-block-move = Block nach oben oder unten
notes-help-block-duplicate = Block duplizieren
notes-help-editor-leave = Notiz verlassen
notes-help-bold = Fett
notes-help-italic = Kursiv
notes-help-inline-code = Code im Text
notes-help-headings = Überschrift 1, 2 oder 3
notes-help-bullet = Aufzählung
notes-help-numbered = Nummerierte Liste
notes-help-checklist = Checkliste
notes-help-quote = Zitat
notes-help-code = Codeblock
notes-help-code-exit = Darunter schreiben
notes-help-code-before = Darüber schreiben
notes-help-code-blank = Leerzeile
notes-help-code-indent = Ein- oder ausrücken
notes-help-code-copy = Code kopieren
notes-help-code-select = Code auswählen
notes-help-shortcuts = Tastenkürzel
notes-help-close = Schließen
notes-help-soft-break = Neue Zeile im selben Eintrag

## Window copy
notes-image-too-big = Das Bild ist größer als 1 MiB
notes-image-kind = Wähle ein PNG-, JPEG- oder WebP-Bild
notes-image-unfit = Das Bild ist zu groß für eine Notiz
notes-empty-excerpt = Leere Notiz
notes-image-paste-failed = Bild nicht eingefügt: { $error }
notes-image-import-failed = Bild nicht hinzugefügt: { $error }
notes-image-default-alt = Bild
notes-copy-link-failed = Link nicht kopiert: Die Zwischenablage ist nicht verfügbar
notes-window-description = Notizen und die geöffnete Notiz
notes-menu-cut = Ausschneiden
notes-menu-copy = Kopieren
notes-menu-paste = Einfügen
notes-menu-select-all = Alles auswählen
notes-imported-untitled = Importierte Notiz
notes-no-previous = Keine frühere Notiz
notes-no-next = Keine spätere Notiz
notes-link-copied = Link zu { $title } kopiert
notes-link-unsafe = Dieser Link lässt sich nicht sicher öffnen
notes-resource-link-invalid = Dieser Link führt hier zu nichts
notes-resource-link-open-failed = Der Link ließ sich nicht öffnen
notes-link-open-failed = Der Link wurde nicht geöffnet
notes-import-title = Markdown importieren
notes-import-accept = Importieren
notes-markdown-files = Markdown
notes-import-not-utf8 = Die Datei ist kein UTF-8-Text
notes-imported = { $title } importiert
notes-import-failed = Nicht importiert: { $error }
notes-export-title = Markdown exportieren
notes-export-accept = Exportieren
notes-insert-image-title = Bild einfügen
notes-insert-image-accept = Einfügen
notes-image-files = Bilder
notes-image-inserted = Bild hinzugefügt
notes-empty-title = Beginne deine erste Notiz
notes-empty-action = Neue Notiz
notes-a11y-page-title = Seitentitel
notes-a11y-note-title = Notiztitel
notes-a11y-first-note = Legt die erste Notiz in diesem Arbeitsbereich an
notes-a11y-header = Notizkopf
notes-a11y-header-description = Notiztitel und Scrollposition
notes-a11y-tag = Schlagwort: { $tag }
notes-a11y-backlink = Verlinkt von { $title }
notes-search-notes = Notizen
notes-search-blocks = Notizblöcke
notes-import-too-big = Das Markdown ist größer als 1 MiB
notes-import-too-many-blocks = Das Markdown hat mehr als 10.000 Blöcke
notes-status-saved = Gespeichert
notes-status-saving = Wird gespeichert…
notes-status-failed = Speichern fehlgeschlagen
notes-status-not-saved = Nicht gespeichert
notes-status-failed-because = Speichern fehlgeschlagen: { $reason }. Kopiere die Notiz an einen sicheren Ort und öffne Notizen erneut
notes-status-failed-detail = Speichern fehlgeschlagen. Kopiere die Notiz an einen sicheren Ort und öffne Notizen erneut
notes-status-no-storage = Nicht gespeichert: Diese Notiz hat auf diesem Gerät keinen Speicher. Kopiere sie an einen sicheren Ort und öffne Notizen erneut
notes-status-note-count = { $notes ->
    [one] 1 Notiz
   *[other] { $notes } Notizen
}
notes-status-counts = { $notes ->
    [one] 1 Notiz
   *[other] { $notes } Notizen
}, { $words ->
    [one] 1 Wort
   *[other] { $words } Wörter
}

notes-outline = Gliederung
notes-outline-empty = Keine Überschriften
notes-outline-untitled = Unbenannte Überschrift
