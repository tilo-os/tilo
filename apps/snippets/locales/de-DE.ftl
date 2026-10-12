snippets-body-placeholder = Snippet-Inhalt
snippets-count = { $count ->
    [one] { $count } Snippet
   *[other] { $count } Snippets
}
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
snippets-empty-favorites = Noch keine Favoriten
snippets-empty-most-used = Noch nichts kopiert
snippets-empty-place = Hier gibt es keine Snippets
snippets-none-selected = Kein Snippet ausgewählt
snippets-language = Sprache
snippets-language-plain-text = Nur Text
snippets-set-language = Sprache auf { $language } setzen
snippets-copy = Kopieren
snippets-fill-and-copy = Ausfüllen und kopieren
snippets-favorite = Favorit
snippets-add-favorite = Zu Favoriten hinzufügen
snippets-remove-favorite = Aus Favoriten entfernen
snippets-more-actions = Weitere Aktionen
snippets-menu-duplicate = Duplizieren
snippets-menu-delete = Löschen
snippets-copy-title = { $title } (Kopie)

## Confirmations

snippets-copied = In die Zwischenablage kopiert
snippets-created = Neues Snippet erstellt
snippets-duplicated = Snippet dupliziert
snippets-deleted = Snippet gelöscht

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
