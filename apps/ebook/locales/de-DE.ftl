ebook-library = Bücher

## Verlauf: was jede Änderung getan hat, nach Feld und nach Aktion

ebook-shelve = { $title } ins Regal gestellt
ebook-shelve-found = { $title } in einem überwachten Ordner gefunden
ebook-remove-from-library = Ein Buch aus der Bibliothek entfernt
ebook-record-progress = In einem Buch weitergelesen
ebook-add-note = Eine Notiz hinzugefügt
ebook-add-comment = Eine Markierung kommentiert
ebook-toggle-bookmark = Eine Seite mit einem Lesezeichen versehen oder das Lesezeichen entfernt
ebook-highlight = Eine Passage markiert
ebook-change-color = Eine Markierung { $color } eingefärbt
ebook-delete-highlight = Eine Markierung gelöscht
ebook-delete-annotation = Eine Anmerkung gelöscht
ebook-remove-font = Die Schrift { $item } entfernt
ebook-delete-scheme = Das Farbschema { $item } gelöscht

ebook-library-entry-title = Den Titel von { $item } geändert
ebook-library-entry-author = Den Autor von { $item } geändert
ebook-library-entry-subject = Das Thema von { $item } geändert
ebook-ebook-entries = Das Regal geändert
ebook-ebook-progress = Die Leseposition geändert
ebook-ebook-default-font = Die Standardschrift gewählt
ebook-ebook-fonts = Die Schriften geändert
ebook-ebook-schemes = Die Farbschemata geändert
ebook-ebook-annotations = Die Anmerkungen geändert
ebook-progress-chapter = Zu Kapitel { $new } gewechselt
ebook-progress-block = Innerhalb eines Kapitels gewechselt
ebook-progress-fraction = Weitergelesen
ebook-progress-opened = Ein Buch geöffnet
ebook-settings-font-scale = Die Textgröße auf { $new } gesetzt
ebook-settings-line-spacing = Den Zeilenabstand auf { $new } gesetzt
ebook-settings-font-id = Eine Leseschrift gewählt
ebook-settings-theme = Die Seitentönung auf { $new } gesetzt
ebook-settings-scheme-id = Ein Farbschema gewählt
ebook-settings-mode = Das Layout auf { $new ->
    [paged] Seiten
    [continuous] Scrollen
   *[two_up] zwei Seiten
} gesetzt
ebook-settings-composition = Den Textsatz auf { $new ->
    [ragged] Flattersatz
   *[book] Blocksatz
} gesetzt
ebook-library-prefs-sort = Das Regal nach { $new } sortiert
ebook-library-prefs-hide-read = { $new ->
    [true] Gelesene Bücher ausgeblendet
   *[false] Gelesene Bücher eingeblendet
}
ebook-font-entry-kind = Die Schriftart von { $item } geändert
ebook-font-entry-name = Die Schrift { $item } umbenannt
ebook-font-entry-size-mult = Die Größe der Schrift { $item } geändert
ebook-font-entry-shown = { $new ->
    [true] { $item } in den Schnelleinstellungen eingeblendet
   *[false] { $item } aus den Schnelleinstellungen ausgeblendet
}
ebook-color-scheme-name = Das Farbschema { $item } umbenannt
ebook-color-scheme-page = Die Seitenfarbe von { $item } geändert
ebook-color-scheme-ink = Die Textfarbe von { $item } geändert
ebook-color-scheme-accent = Die Akzentfarbe von { $item } geändert
ebook-color-scheme-dark = Geändert, ob { $item } dunkel ist
ebook-color-scheme-enabled = { $new ->
    [true] Das Farbschema { $item } eingeschaltet
   *[false] Das Farbschema { $item } ausgeschaltet
}
ebook-mark-chapter = „{ $item }“ verschoben
ebook-mark-block = „{ $item }“ verschoben
ebook-mark-start = „{ $item }“ verschoben
ebook-mark-end = „{ $item }“ verschoben
ebook-mark-excerpt = Eine Anmerkung nach „{ $new }“ verschoben
ebook-mark-text = In „{ $item }“ geschrieben
ebook-mark-color = „{ $item }“ eingefärbt
ebook-mark-created = Eine Notiz zu „{ $item }“ abgeschlossen

## Wie die Leseansicht und ihre Panels für assistive Technik heißen

ebook-a11y-reading-page = Aktuelle Leseseite
ebook-a11y-progress-hint = Ziehe oder nutze die Pfeiltasten links und rechts, um durch das Buch zu blättern
ebook-field-name = Name
ebook-a11y-comments = Kommentare zur Markierung
ebook-a11y-edit-note = Notiz bearbeiten
ebook-a11y-choose-colour = Farbe wählen

## Fenster- und Barrierefreiheits-Texte
ebook-drop-epub = EPUB hier ablegen oder eines auswählen

## Fakten, Eigenschaftsnamen und Bereichstitel
ebook-key-reserved = { $key } ist für einen eingebauten Lesebefehl reserviert. Wähle eine andere Taste.
ebook-a11y-annotations = { $annotations ->
    [one] Eine Anmerkung. Scrolle durch diese Liste, um sie zu sehen.
   *[other] Anmerkungen: { $annotations } insgesamt. Scrolle durch diese Liste, um alle zu sehen.
}
ebook-a11y-contents = Inhalt: Kapitel { $chapter } von { $chapters }. Scrolle durch diese Liste, um alle Kapitel zu sehen.
ebook-a11y-annotation-hit = { $kind } in Kapitel { $chapter } von { $title }

## What the window shows
ebook-view-library = Bücherbibliothek
ebook-view-library-detail = Bücher ansehen und öffnen
ebook-view-reader = Buchleser
ebook-view-reader-detail = Das aktuelle Buch lesen und anmerken
ebook-view-settings = Bucheinstellungen
ebook-view-settings-detail = Leseschriften, Farben und Bibliotheksordner verwalten
ebook-view-search = Bücher durchsuchen
ebook-view-search-detail = Treffer in deinen Büchern und im Laden
ebook-add-note-button = Notiz hinzufügen
ebook-no-passage = Keine Textstelle
ebook-query-get = { $book } aus { $store } holen
ebook-query-open = { $title } öffnen
ebook-duplicate = Duplizieren
ebook-edit = Bearbeiten
ebook-add-system-font = Systemschrift hinzufügen
ebook-new-scheme = Neues Farbschema
ebook-highlighted-passage = Markierte Textstelle
ebook-selected-passage = Ausgewählte Textstelle
ebook-manage-fonts = Schriften und Farbthemen verwalten
ebook-close-appearance = Leseerscheinung schließen
ebook-use-font = Schrift { $font } verwenden
ebook-reading-theme = { $theme ->
    [light] Helles Lesethema
    [sepia] Sepia-Lesethema
    [dark] Dunkles Lesethema
   *[high-contrast] Kontrastreiches Lesethema
}
ebook-use-scheme = Farbthema { $scheme } verwenden
ebook-untitled = Ohne Titel

ebook-get = Holen

## What an action is called, in a tooltip and for assistive technology
ebook-action-open-book = Buch öffnen
ebook-action-share-book = Buch im Arbeitsbereich freigeben
ebook-action-share-book-title = { $book } im Arbeitsbereich freigeben
ebook-action-remove-book = Buch aus der Bibliothek entfernen
ebook-action-remove-book-title = { $book } aus der Bibliothek entfernen
ebook-action-back-to-library = Zurück zur Bibliothek
ebook-action-previous-page = Vorherige Seite
ebook-action-next-page = Nächste Seite
ebook-action-reading-progress = Lesefortschritt: { $progress }
ebook-action-go-to-chapter = Zu Kapitel { $number }
ebook-action-contents = Inhalt
ebook-action-annotations = Anmerkungen
ebook-action-reading-appearance = Leseerscheinung
ebook-action-bookmark-page = Diese Seite merken
ebook-action-increase-text = Schrift vergrößern
ebook-action-decrease-text = Schrift verkleinern
ebook-action-increase-spacing = Zeilenabstand vergrößern
ebook-action-decrease-spacing = Zeilenabstand verkleinern
ebook-action-next-typeface = Nächste Schrift verwenden
ebook-action-mode = { $mode ->
    [paged] Seiten-Layout
    [continuous] Scroll-Layout
   *[two_up] Doppelseiten-Layout
}
ebook-action-composition = { $style ->
    [book] Blocksatz
   *[ragged] Flatterrand
}
ebook-action-highlight = { $colour ->
    [yellow] In Gelb hervorheben
    [green] In Grün hervorheben
    [pink] In Rosa hervorheben
    [blue] In Blau hervorheben
    [purple] In Lila hervorheben
   *[custom] In eigener Farbe hervorheben
}
ebook-action-copy-selection = Ausgewählten Text kopieren
ebook-action-reveal-annotation = Anmerkung zeigen
ebook-action-delete-annotation = Anmerkung löschen
ebook-action-annotation = { $action }: { $text }
ebook-action-open-comments = Kommentare zur Markierung öffnen
ebook-action-close-comments = Kommentare schließen
ebook-action-delete-highlight = Markierung löschen
ebook-action-add-comment = Kommentar hinzufügen
ebook-action-edit-comment = Kommentar bearbeiten
ebook-action-delete-comment = Kommentar löschen
ebook-action-watch-folder = { $folder } beobachten
ebook-keep-catalogues = Buchkataloge auf diesem Gerät behalten
ebook-tab-library = Bibliothek
ebook-tab-fonts = Schriften
ebook-tab-schemes = Farbschemata
ebook-tab-shortcuts = Tastenkürzel
ebook-action-press-key = Taste für { $shortcut } drücken; Escape bricht ab
ebook-action-shortcut-error = { $shortcut }: { $error }
ebook-action-change-shortcut = { $shortcut } ändern, derzeit { $keys }
ebook-action-show-scheme = { $scheme } im Leser anzeigen
ebook-action-duplicate-scheme = { $scheme } duplizieren
ebook-action-delete-scheme = { $scheme } löschen
ebook-action-edit-scheme = { $scheme } bearbeiten
ebook-action-done-editing-theme = Farbthema fertig bearbeitet
ebook-action-new-colour-theme = Neues Farbthema
ebook-action-edit-scheme-colour = { $slot ->
    [page] Seitenfarbe von { $scheme } bearbeiten
    [text] Textfarbe von { $scheme } bearbeiten
    [accent] Akzentfarbe von { $scheme } bearbeiten
   *[custom] Eigene Farbe von { $scheme } bearbeiten
}
ebook-action-rename-scheme = { $scheme } umbenennen
ebook-action-show-font = { $font } im Leser anzeigen
ebook-action-remove-font = { $font } entfernen
ebook-action-add-font = Schrift hinzufügen
ebook-action-close-font-picker = Schriftauswahl schließen
ebook-action-add-family = { $family } hinzufügen
ebook-action-rename-font = { $font } umbenennen
ebook-action-open-result = Treffer öffnen
ebook-action-get-book = Buch holen
ebook-action-back = Zurück
ebook-empty-comment = Leerer Kommentar
