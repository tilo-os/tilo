# PDF-Viewer — deutsche Übersetzung (de-DE).

pdf-viewer-history-scope = PDF und seine Markierungen

## Verlauf: was jede Änderung getan hat, nach Feld und nach Aktion

pdf-viewer-page-occurrence-rotation = Eine Seite auf { $new } Grad gedreht
pdf-viewer-annotation-page-id = Markierungen auf eine andere Seite verschoben
pdf-viewer-annotation-mark = Die Form einer Markierung geändert
pdf-viewer-annotation-color = Eine Markierung eingefärbt
pdf-viewer-annotation-space = Eine Markierung verschoben
pdf-viewer-pdf-anchor-page-id = Einen Kommentar auf eine andere Seite verschoben
pdf-viewer-comment-text = Den Kommentar { $item } bearbeitet
pdf-viewer-comment-body = Die Formatierung des Kommentars { $item } geändert
pdf-viewer-comment-resolved = { $new ->
    [true] Den Kommentar { $item } erledigt
   *[false] Den Kommentar { $item } wieder geöffnet
}
pdf-viewer-comment-replies = Die Antworten auf { $item } geändert
pdf-viewer-reply-text = Die Antwort { $item } bearbeitet
pdf-viewer-reply-body = Die Formatierung der Antwort { $item } geändert
pdf-viewer-stored-signature-page-id = Eine Unterschrift auf eine andere Seite verschoben
pdf-viewer-stored-signature-rect = Eine Unterschrift verschoben
pdf-viewer-stored-signature-space = Eine Unterschrift verschoben
pdf-viewer-pdf-markup-pages = Die Seiten geändert
pdf-viewer-pdf-markup-annotations = Die Markierungen geändert
pdf-viewer-pdf-markup-comments = Die Kommentare geändert
pdf-viewer-pdf-markup-signatures = Die Unterschriften geändert
pdf-viewer-pdf-markup-form-values = Das Formular ausgefüllt
pdf-viewer-start-page-plan = Die Seiten angeordnet
pdf-viewer-pin-source = Das PDF festgehalten, auf dem die Markierungen liegen
pdf-viewer-adopt-existing = Die vorhandenen Markierungen im PDF übernommen
pdf-viewer-import-pages = Seiten importiert
pdf-viewer-duplicate-page = Eine Seite dupliziert
pdf-viewer-delete-page = Eine Seite gelöscht
pdf-viewer-move-page = Eine Seite verschoben
pdf-viewer-rotate-page = Eine Seite gedreht
pdf-viewer-add-annotation = Eine Seite markiert
pdf-viewer-delete-annotation = Eine Markierung gelöscht
pdf-viewer-create-note = Eine Notiz hinzugefügt
pdf-viewer-add-note = Die Notiz „{ $text }“ hinzugefügt
pdf-viewer-add-comment = Kommentiert
pdf-viewer-reply = „{ $text }“ auf { $item } geantwortet
pdf-viewer-delete-comment = Den Kommentar { $item } gelöscht
pdf-viewer-add-signature = Unterschrieben
pdf-viewer-add-typed-signature = Mit einem getippten Namen unterschrieben
pdf-viewer-delete-signature = Eine Unterschrift entfernt

## Statushinweise: was passiert ist und was als Nächstes zu tun ist

pdf-note-opening = Das PDF wird geöffnet …
pdf-note-open-failed = { $name } konnte nicht geöffnet werden. Prüfe, ob es ein PDF ist, und versuche es erneut.
pdf-note-document-not-opened = { $name } in diesem Arbeitsbereich wurde nicht geöffnet. Vielleicht ist es noch nicht auf diesem Gerät; es öffnet sich, sobald ein Gerät mit ihm online ist.
pdf-note-open-in-new-window = Dieses Fenster zeigt { $shown } und seine Markierungen. Öffne { $name } in einem neuen PDF-Viewer-Fenster.
pdf-note-open-in-new-window-unnamed = Dieses Fenster behält die Markierungen seines Dokuments. Öffne { $name } in einem neuen PDF-Viewer-Fenster.
pdf-note-source-changed = Diese Datei hat sich nach dem Markieren geändert. Prüfe, ob deine Markierungen noch an ihrer Stelle sitzen.
pdf-note-existing-markup-unread = Die vorhandenen Markierungen in diesem PDF konnten nicht gelesen werden. Du kannst weiterhin eigene hinzufügen.
pdf-note-pages-unavailable = Seiten können gerade nicht angezeigt werden. Öffne das PDF erneut, um es noch einmal zu versuchen.
pdf-note-page-not-shown = Seite { $page } konnte nicht angezeigt werden. Öffne das PDF erneut, um es noch einmal zu versuchen.
pdf-note-text-unavailable = Der Text in diesem PDF kann gerade nicht gelesen werden. Öffne das PDF erneut, um es noch einmal zu versuchen.
pdf-note-page-text-unread = Der Text auf Seite { $page } kann nicht gelesen werden und lässt sich daher nicht finden, markieren oder kopieren.
pdf-note-no-text = Dieses PDF besteht aus Bildern von Seiten, daher lässt sich sein Text nicht finden, markieren oder kopieren.
pdf-note-pages-missing = Einige Seiten sind noch nicht auf diesem Gerät. Sie kommen an, sobald jemand mit ihnen online ist.
pdf-note-pages-not-arranged = Die Seiten konnten nicht in die Reihenfolge gebracht werden, die die anderen hinterlassen haben. Öffne das PDF erneut, um es noch einmal zu versuchen.

pdf-note-save-no-document = Öffne zuerst ein PDF und speichere dann eine Kopie mit deinen Markierungen.
pdf-note-save-waiting-pages = Die Seiten werden noch angeordnet. Speichere gleich noch einmal.
pdf-note-saving = Eine Kopie mit deinen Markierungen wird als { $name } gespeichert …
pdf-note-saved = Eine Kopie mit deinen Markierungen wurde als { $name } gespeichert.
pdf-note-save-failed = { $name } wurde nicht gespeichert. Wähle einen anderen Ordner oder Namen.
pdf-note-save-picker-failed = Das Speicherfenster konnte nicht geöffnet werden. Versuche das Speichern erneut.

pdf-note-import-no-document = Öffne zuerst ein PDF und importiere dann Seiten hinein.
pdf-note-import-busy = Seiten werden bereits importiert. Importiere mehr, sobald sie da sind.
pdf-note-import-unavailable = Seiten können gerade nicht importiert werden. Öffne das PDF erneut und importiere dann.
pdf-note-import-picker-failed = Das Dateifenster konnte nicht geöffnet werden. Versuche den Import erneut.
pdf-note-importing = Seiten werden importiert …
pdf-note-imported = { $count ->
    [one] 1 Seite hinzugefügt. Speichere eine Kopie, um ein PDF damit zu behalten.
   *[other] { $count } Seiten hinzugefügt. Speichere eine Kopie, um ein PDF damit zu behalten.
}
pdf-note-import-failed = Diese Seiten konnten nicht importiert werden. Wähle ein PDF oder ein Bild und versuche es erneut.
pdf-note-imported-arriving = Importierte Seiten sind von jemandem in diesem Arbeitsbereich unterwegs.
pdf-note-imported-waiting = Importierte Seiten kommen an, sobald jemand mit ihnen online ist.
pdf-note-imported-unavailable = Importierte Seiten sind nicht auf diesem Gerät. Sie kommen an, sobald jemand mit ihnen online ist.
pdf-note-imported-unshown = Importierte Seiten können auf diesem Gerät nicht angezeigt werden.

pdf-note-copying = Wird kopiert …
pdf-note-copied-text = { $count ->
    [one] 1 Zeichen kopiert
   *[other] { $count } Zeichen kopiert
}
pdf-note-copying-area = Der ausgewählte Bereich wird kopiert …
pdf-note-copied-area = Der ausgewählte Bereich wurde als Bild kopiert
pdf-note-area-not-copied = Der ausgewählte Bereich konnte nicht kopiert werden. Versuche es erneut.

pdf-note-find-result-gone = Diese Fundstelle ist nicht mehr im Dokument. Suche erneut, um zu sehen, was jetzt da ist.
pdf-note-item-gone = Das ist nicht mehr in diesem Dokument.
pdf-note-field-read-only = „{ $field }“ ist gesperrt und kann nicht ausgefüllt werden.

pdf-note-signature-draw-first = Zeichne zuerst deine Unterschrift und klicke dann auf eine Seite.
pdf-note-signature-name-first = Tippe deinen Namen unter Unterschriften ein und klicke dann auf eine Seite.
pdf-note-signature-not-kept = Deine Unterschrift wurde nicht für das nächste Mal behalten. Ändere sie erneut, um sie zu behalten.

pdf-note-link-page-gone = Die Seite, auf die dieser Link zeigt, ist nicht mehr im Dokument.
pdf-note-link-opening = { $address } wird geöffnet
pdf-note-link-failed = { $address } konnte nicht geöffnet werden. Versuche den Link erneut.
pdf-note-link-unsupported = Links wie { $address } werden nicht aus einem PDF geöffnet.
pdf-note-link-file = Links zu anderen Dateien werden nicht aus einem PDF geöffnet. Öffne { $path } stattdessen in Dateien.

## Das leere Fenster: was es vor dem Öffnen eines Dokuments sagt und warum ein
## Öffnen gescheitert ist. Wird sowohl gezeichnet als auch vorgelesen.

pdf-empty-heading = Kein Dokument geöffnet
pdf-empty-opening = PDF wird geöffnet …
pdf-empty-open = PDF öffnen
pdf-empty-open-another = Anderes PDF öffnen
pdf-empty-open-failed = Dieses Dokument konnte nicht geöffnet werden
pdf-empty-detail-password = Dieses PDF ist passwortgeschützt und kann daher nicht geöffnet werden.
pdf-empty-detail-picker = Das Dateifenster konnte nicht geöffnet werden. Versuche das Öffnen erneut.
pdf-empty-detail-engine = Der PDF-Viewer kann auf diesem Gerät gerade keine PDFs öffnen. Versuche es erneut.
pdf-empty-detail-unreadable = Die Datei ist nicht verfügbar oder kein lesbares PDF.
pdf-empty-detail-other = Prüfe die Datei und versuche es erneut.
pdf-empty-waiting-title = Warten auf das Dokument
pdf-empty-waiting = { $name } öffnet sich, sobald ein Gerät mit dieser Datei online ist.
pdf-empty-elsewhere-title = Nicht auf diesem Gerät
pdf-empty-elsewhere = Kein verbundenes Gerät hat gerade { $name }. Es öffnet sich, sobald ein Gerät mit dieser Datei online ist.
pdf-empty-arriving-title = Das Dokument wird geholt …
pdf-empty-arriving = { $name } kommt von einem Gerät in diesem Arbeitsbereich, das die Datei hat.
pdf-empty-the-document = Das Dokument

## Die Tabs der Prüf-Seitenleiste: ein Name pro Tab, bei jeder Breite
## derselbe.

pdf-tab-comments = Kommentare
pdf-tab-contents = Inhalt
pdf-tab-signing = Signieren

## Die Prüf-Seitenleiste: die Kommentare der Lesenden und die platzierten
## Unterschriften.

pdf-comments-empty-title = Noch keine Kommentare
pdf-comments-empty-message = Markiere Text, um ihn zu kommentieren, oder füge mit dem Notiz-Werkzeug eine Notiz hinzu.
pdf-comments-one-open = 1 Kommentar, offen
pdf-comments-one-resolved = 1 Kommentar, erledigt
pdf-comments-all-resolved = { $total } Kommentare, alle erledigt
pdf-comments-open = { $total } Kommentare, { $open } offen
pdf-comment-reply = Antworten
pdf-comment-reply-placeholder = Antworten …
pdf-comment-resolve = Erledigen
pdf-comment-reopen = Wieder öffnen
pdf-comment-edit = Kommentar bearbeiten
pdf-comment-delete = Kommentar löschen
pdf-viewer-comment-placeholder = Kommentar schreiben …
pdf-comment-empty = Leerer Kommentar
pdf-author-you = Du
pdf-author-someone = Jemand
pdf-contents-empty = Dieses Dokument hat keinen Inhalt

pdf-viewer-signature-your = Deine Unterschrift
pdf-viewer-signing-as = Du unterschreibst als
pdf-signature-your-name = Dein Name
pdf-viewer-signature-placed = Platziert
pdf-viewer-signature-placed-note = Noch keine Unterschrift platziert.
pdf-signature-type = Tippen
pdf-signature-draw = Zeichnen
pdf-signature-draw-hint = Zeichne deine Unterschrift hier
pdf-signature-clear = Leeren
pdf-signature-place = Auf der Seite platzieren
pdf-signature-cancel = Abbrechen
pdf-signature-place-hint = Klicke auf eine Seite, um deine Unterschrift zu platzieren.
pdf-signature-delete = Unterschrift löschen
pdf-signature-annotation = Unterschrieben von { $signer } am { $date }
pdf-signature-unnamed = Unterschrift

## Importierte Seiten: eine Seite, die noch unterwegs ist, auf der Seite
## selbst gesagt.

pdf-imported-page-loading = Importierte Seite wird geladen …
pdf-imported-page-waiting = Die importierte Seite kommt an, sobald jemand, der sie hat, online ist.
pdf-imported-page-unavailable = Importierte Seite ist nicht verfügbar

## Die Seite und der Zoom

pdf-page-line = Seite { $page } von { $pages }, { $zoom } %
pdf-rail-import = Importieren …

## Die Leiste des Fensters, ihre Werkzeug- und Farbmenüs

pdf-bar-hide-review = Seitenleiste ausblenden
pdf-bar-show-review = Seitenleiste einblenden
pdf-bar-zoom-in = Vergrößern
pdf-bar-zoom-out = Verkleinern
pdf-bar-fit-page = Ganze Seite einpassen
pdf-bar-fit-width = Breite einpassen
pdf-bar-tools = Werkzeuge
pdf-bar-marker-colour = Markierfarbe
pdf-bar-zoom = Zoom
pdf-bar-previous-page = Vorherige Seite
pdf-bar-next-page = Nächste Seite
pdf-bar-save = PDF mit Markierungen speichern
pdf-tool-select = Auswählen
pdf-tool-highlight = Markieren
pdf-tool-draw = Zeichnen
pdf-tool-note = Notiz
pdf-tool-text = Text
pdf-tool-sign = Unterschreiben
pdf-colour-yellow = Gelb
pdf-colour-green = Grün
pdf-colour-pink = Pink
pdf-colour-blue = Blau
pdf-colour-purple = Violett

## Die Dateifenster, die das System zum Öffnen, Importieren und Speichern
## zeigt

pdf-chooser-open = PDF öffnen
pdf-chooser-open-accept = Öffnen
pdf-chooser-filter-pdf = PDF-Dokumente
pdf-chooser-import = Seiten aus PDF oder Bild importieren
pdf-chooser-import-accept = Importieren
pdf-chooser-filter-import = PDF oder Bild
pdf-chooser-save-accept = Speichern

## Weitere Befehle und Ergebnisse

pdf-delete = Löschen
pdf-try-again = Erneut versuchen

## Was die App der Suche des Schreibtischs anbietet

pdf-search-open = PDF-Dokument öffnen
pdf-search-bookmark = Lesezeichen: { $title }
pdf-search-contents = Inhalt auf Seite { $page }
pdf-search-result = Treffer für „{ $query }“
pdf-search-annotation = { $kind } auf Seite { $page }
pdf-search-comment = Kommentar von { $author }
pdf-search-signed = Seite { $page } unterschrieben am { $date }
pdf-search-pages = { $count ->
    [one] 1 Seite
   *[other] { $count } Seiten
}
pdf-search-no-pages = Noch keine Seiten

## Wie eine Markierung auf einer Seite heißt

pdf-annotation-highlight = Markierung
pdf-annotation-underline = Unterstreichung
pdf-annotation-strikeout = Durchstreichung
pdf-annotation-squiggly = Wellenlinie
pdf-annotation-drawing = Zeichnung
pdf-annotation-note = Notiz
pdf-annotation-text = Textfeld

## Wie das Fenster und seine Teile für assistive Technik heißen

pdf-title = PDF-Viewer
pdf-a11y-pages = Seiten
pdf-a11y-page-markup = Enthält Markierungen
pdf-a11y-canvas = Zeichenfläche der PDF-Seite. Pfeiltasten scrollen; Bild auf und Bild ab wechseln die Seite.
pdf-a11y-search-results = Suchergebnisse
pdf-a11y-tabs = Registerkarten
pdf-a11y-review-sidebar = Seitenleiste
pdf-a11y-panel-resize = Größe der Seitenleiste
pdf-a11y-document = Dokument
pdf-a11y-zoom = Zoom { $zoom } %
pdf-a11y-page = Seite { $page }
pdf-a11y-page-of = Seite { $current } von { $pages }
pdf-a11y-match = Treffer { $match } auf Seite { $page }
pdf-a11y-comment = { $author } auf Seite { $page }; { $replies ->
    [one] eine Antwort
   *[other] { $replies } Antworten
}
pdf-a11y-signature-by = Signatur von { $signer }
pdf-a11y-signed = Signiert am { $date } auf Seite { $page }
pdf-drawn-signed = Signiert am { $date }

## What the window shows
pdf-viewer-untitled = Unbenanntes Dokument
pdf-viewer-page-unreadable = Seite { $page } lässt sich nicht lesen: { $detail }
