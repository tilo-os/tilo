snippets-body-placeholder = Snippet-Inhalt
snippets-variables = { $count ->
    [one] { $count } Variable
   *[other] { $count } Variablen
}
snippets-uses = { $count ->
    [0] Noch nicht kopiert
    [one] Einmal kopiert
   *[other] { $count }-mal kopiert
}

## Sidebar

snippets-library = Snippet-Bibliothek
snippets-section-library = Bibliothek
snippets-section-folders = Ordner
snippets-section-tags = Tags
snippets-folder-all = Alle Snippets
snippets-folder-favorites = Favoriten
snippets-folder-most-used = Meistgenutzt
snippets-folder-named = Ordner { $name }
snippets-tag-named = Tag { $name }

## List and snippet

snippets-untitled = Unbenanntes Snippet
snippets-new = Neues Snippet
snippets-empty-library = Noch keine Snippets
snippets-empty-library-message = Bewahre Text auf, den du immer wieder einfügst: Befehle, Antworten, Code. Kopiere ihn mit einem Klick, mit Lücken zum Ausfüllen, wenn er Variablen hat.
snippets-empty-favorites = Noch keine Favoriten
snippets-empty-most-used = Noch nichts kopiert
snippets-empty-place = Hier gibt es keine Snippets
snippets-none-selected = Kein Snippet ausgewählt
snippets-language = Sprache
snippets-name = Name
snippets-description = Beschreibung
snippets-description-placeholder = Wofür dieses Snippet gedacht ist
snippets-folder = Ordner
snippets-folder-placeholder = Einem Ordner zuordnen
snippets-add-tag = Tag hinzufügen
snippets-copied-clears = Kopiert; nach einer Minute ist die Zwischenablage wieder leer
snippets-copy-failed = Konnte nicht in die Zwischenablage kopieren
snippets-add-tag-placeholder = Tag hinzufügen
snippets-copy-only = Kopieren ist der Weg aus dieser Bibliothek: füge das Snippet dort ein, wo du es brauchst.
snippets-language-plain-text = Nur Text
snippets-set-language = Sprache auf { $language } setzen
snippets-copy = Kopieren
snippets-fill-and-copy = Ausfüllen und kopieren
snippets-add-favorite = Zu Favoriten hinzufügen
snippets-remove-favorite = Aus Favoriten entfernen
snippets-menu-duplicate = Duplizieren
snippets-menu-delete = Löschen
snippets-copy-title = { $title } (Kopie)

## Confirmations

snippets-copied = In die Zwischenablage kopiert

## Fill in variables

snippets-expand-title = Variablen ausfüllen
snippets-expand-value = Wert
snippets-expand-preview = Vorschau
snippets-cancel = Abbrechen

## History: what each change did, by field and by verb

snippets-snippet-title = { $item } umbenannt
snippets-snippet-body = Inhalt von { $item } bearbeitet
snippets-snippet-description = Beschreibung von { $item } bearbeitet
snippets-snippet-favorite = { $new ->
    [true] { $item } als Favorit markiert
   *[false] { $item } nicht mehr als Favorit markiert
}
snippets-snippet-folder = { $item } in den Ordner { $new } verschoben
snippets-snippet-language = Sprache von { $item } auf { $new ->
    [text] Nur Text
    [shell] Shell
    [rust] Rust
    [sql] SQL
    [git] Git
    [python] Python
    [javascript] JavaScript
    [json] JSON
    [yaml] YAML
    [markdown] Markdown
    [html] HTML
    [css] CSS
    [toml] TOML
    [dockerfile] Dockerfile
   *[other] { $new }
} gesetzt
snippets-snippet-tags = Tags von { $item } geändert
snippets-new-snippet = { $title } erstellt
snippets-duplicate = { $item } dupliziert
snippets-delete = { $item } gelöscht
snippets-record-use = { $item } kopiert
snippets-rename-folder = Ordner { $folder } in { $to } umbenannt
snippets-rename-tag = Tag { $tag } in { $to } umbenannt
snippets-remove-tag = Tag { $tag } von allen Snippets entfernt
