# Word processor — German (de) locale.
#
# Any key omitted here transparently falls back to the en-US string (see
# editor/src/i18n.rs). Glyph-only faces are not keyed and stay as in code.

## Toolbar button faces
toolbar-new = Neu
toolbar-table = Tabelle
toolbar-link = Link
toolbar-styles = Stile
toolbar-ruler = Lineal
toolbar-toc = Inhalt
toolbar-row-add = Zeile einfügen
toolbar-row-del = Zeile löschen
toolbar-col-add = Spalte einfügen
toolbar-col-del = Spalte löschen

## Toolbar accessibility names / tooltips
a11y-group-formatting = Formatierung
a11y-bold = Fett
a11y-italic = Kursiv
a11y-underline = Unterstrichen
a11y-strikethrough = Durchgestrichen
a11y-superscript = Hochgestellt
a11y-subscript = Tiefgestellt
a11y-heading1 = Überschrift 1
a11y-heading2 = Überschrift 2
a11y-body-text = Fließtext
a11y-bulleted-list = Aufzählungsliste
a11y-align-left = Linksbündig
a11y-align-center = Zentriert
a11y-align-right = Rechtsbündig
a11y-justify = Blocksatz
a11y-line-spacing = Zeilenabstand
a11y-font = Schriftart
a11y-size-up = Schrift vergrößern
a11y-size-down = Schrift verkleinern
a11y-font-size = Schriftgröße
a11y-insert-image = Bild einfügen
a11y-hyperlink = Link hinzufügen oder ändern
a11y-insert-table = Tabelle einfügen
a11y-table-row-add = Tabellenzeile hinzufügen
a11y-table-row-del = Tabellenzeile löschen
a11y-table-col-add = Tabellenspalte hinzufügen
a11y-table-col-del = Tabellenspalte löschen
a11y-table-delete = Tabelle löschen
a11y-styles-panel = Stil-Panel
a11y-ruler = Lineal
a11y-document-outline = Dokumentgliederung
a11y-insert-toc = Inhaltsverzeichnis einfügen
a11y-new-from-template = Neu aus Vorlage
a11y-text-color = Textfarbe #{ $hex }
a11y-text-color-default = Standard-Textfarbe
a11y-highlight = Hervorhebung #{ $hex }
a11y-highlight-clear = Hervorhebung entfernen

## Crash-recovery banner
recovery-banner = Nicht gespeicherte Änderungen aus einer früheren Sitzung wiederhergestellt.
recovery-restore = Wiederherstellen
recovery-discard = Verwerfen

## Status bar
status-untitled = Unbenannt
format-odt = ODF-Text
format-docx = Word-Dokument
format-markdown = Markdown
format-pdf = PDF
format-html = HTML
format-rtf = Rich Text
format-epub = EPUB

## Header / footer edit-mode tabs
band-header = Kopfzeile
band-footer = Fußzeile

## Review — context-menu rows
review-accept-insertion = Einfügung annehmen
review-reject-insertion = Einfügung ablehnen
review-accept-deletion = Löschung annehmen
review-reject-deletion = Löschung ablehnen
review-resolve-comment = Kommentar auflösen
review-reply = Antworten
review-thread = Kommentarthread
review-thread-unavailable = Dieser Kommentar ist nicht mehr verfügbar
review-reply-placeholder = Antwort schreiben
review-send-reply = Senden
review-cancel-reply = Abbrechen
review-you = Du
review-accept-all = Alle Änderungen annehmen
review-reject-all = Alle Änderungen ablehnen

## Review / comment margin cards
review-comment = Kommentar
review-resolve = Auflösen
review-insertion = Einfügung
review-deletion = Löschung

## Edit context-menu (clipboard + spell)
menu-add-to-dictionary = Zum Wörterbuch hinzufügen
menu-cut = Ausschneiden
menu-copy = Kopieren
menu-paste = Einfügen
menu-select-all = Alles auswählen

## Templates (the New menu)
template-blank = Leer
template-letter = Brief
template-report = Bericht
template-resume = Lebenslauf
template-meeting-notes = Besprechungsnotizen

## Built-in style display names (localized)
style-body = Fließtext
style-heading1 = Überschrift 1
style-heading2 = Überschrift 2
style-heading3 = Überschrift 3
style-list = Listenelement
style-quote = Zitat

## Paragraph panel
panel-this-paragraph = Dieser Absatz
panel-line-spacing = Zeilenabstand
panel-style = Absatzstil
panel-style-hint = Ändert jeden Absatz vom Typ "{ $name }"
panel-caret-hint = Cursor in einen Absatz setzen
panel-paragraph-styles = Absatzstile
panel-style-properties = Stileigenschaften
panel-font = Schriftart
panel-font-serif = Serif
panel-font-mono = Mono
panel-font-sans = Sans
panel-weight = Betonung
panel-bold = Fett
panel-italic = Kursiv
panel-colour = Farbe
panel-colour-custom = Eigene Farbe
panel-align = Ausrichtung
panel-new-style = Neuer Stil aus diesem Absatz
panel-num-size = Größe
panel-num-space-before = Abstand davor
panel-num-space-after = Abstand danach
panel-num-indent = Einzug

## Paragraph-style context menu (right-click a style chip)
style-menu-delete = Stil löschen
style-menu-duplicate = Stil duplizieren

## Transient toasts (confirmation cues)
toast-toc-updated = Inhaltsverzeichnis aktualisiert ({ $n } { $n ->
        [one] Eintrag
       *[other] Einträge
    })
toast-toc-empty = Erst Überschriften hinzufügen — ein Inhaltsverzeichnis listet sie auf
toast-style-deleted = Stil gelöscht
toast-style-duplicated = Stil dupliziert
toast-saved = Dokument gespeichert
toast-save-failed = Dokument konnte nicht gespeichert werden
toast-open-failed = Dokument konnte nicht geöffnet werden. Das aktuelle Dokument bleibt unverändert.
toast-view-only = Dieses Dokument ist nur zum Ansehen geöffnet.
unsaved-title = Speichern?
unsaved-body = Dieses Dokument vor dem Ersetzen speichern?
unsaved-save = Speichern
unsaved-discard = Änderungen verwerfen
unsaved-cancel = Abbrechen
unsaved-close = Schließen
toast-file-dialog-failed = Dateiauswahl konnte nicht geöffnet werden

## Outline / navigation pane
outline-header = Gliederung
outline-empty = Noch keine Überschriften
outline-untitled = Unbenannte Überschrift

## Page layout (ruler) bottom bar
layout-margins = Ränder
layout-spacing = Abstände
layout-margins-readout = Links { $l }   Oben { $t }   Rechts { $r }   Unten { $b }
layout-spacing-readout = Absätze „{ $name }“: { $px } px danach

## Canvas
a11y-document-editor = Dokumenteditor
a11y-group-review = Vorschläge prüfen
page-indicator = Seite { $n }
canvas-image-missing = Bild
canvas-embedded-content = Eingebetteter Inhalt
canvas-empty-placeholder = Beginne zu schreiben…
toolbar-history = Verlauf
toolbar-text-color = Farbe
toolbar-highlight = Hervorhebung
picker-text-color = Textfarbe
picker-highlight = Hervorhebung
a11y-text-colour-name = Textfarbe { $colour }
a11y-highlight-name = Hervorhebung { $colour }
color-black = Schwarz
color-gray = Grau
color-red = Rot
color-blue = Blau
color-green = Grün
color-amber = Bernstein
color-purple = Violett
color-yellow = Gelb
color-cyan = Cyan
color-pink = Rosa
color-orange = Orange
a11y-version-history = Versionsverlauf
panel-close = Schließen

## Inspector and status band
panel-image-crop-inset = Ränder zuschneiden
status-words = { $words } { $words ->
        [one] Wort
       *[other] Wörter
    }
status-chars = { $chars } { $chars ->
        [one] Zeichen
       *[other] Zeichen
    }
a11y-zoom = Zoom
line-spacing-single = Einfach
line-spacing-double = Doppelt
panel-align-left = Links
panel-align-center = Mitte
panel-align-right = Rechts
panel-align-justify = Blocksatz
