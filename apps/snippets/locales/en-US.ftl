snippets-body-placeholder = Snippet body
snippets-count = { $count ->
    [one] { $count } snippet
   *[other] { $count } snippets
}
snippets-variables = { $count ->
    [one] { $count } variable
   *[other] { $count } variables
}
snippets-uses = { $count ->
    [0] Not copied yet
    [one] Copied once
   *[other] Copied { $count } times
}

## Sidebar

snippets-library = Snippet library
snippets-section-library = Library
snippets-section-folders = Folders
snippets-section-tags = Tags
snippets-folder-all = All snippets
snippets-folder-favorites = Favorites
snippets-folder-most-used = Most used
snippets-folder-named = Folder { $name }
snippets-tag-named = Tag { $name }

## List and snippet

snippets-untitled = Untitled snippet
snippets-new = New snippet
snippets-empty-library = No snippets yet
snippets-empty-favorites = No favorites yet
snippets-empty-most-used = Nothing copied yet
snippets-empty-place = No snippets here
snippets-none-selected = No snippet selected
snippets-language = Language
snippets-language-plain-text = Plain text
snippets-set-language = Set language to { $language }
snippets-copy = Copy
snippets-fill-and-copy = Fill in and copy
snippets-favorite = Favorite
snippets-add-favorite = Add to favorites
snippets-remove-favorite = Remove from favorites
snippets-more-actions = More actions
snippets-menu-duplicate = Duplicate
snippets-menu-delete = Delete
snippets-copy-title = { $title } (copy)

## Confirmations

snippets-copied = Copied to clipboard

## Fill in variables

snippets-expand-title = Fill in variables
snippets-expand-value = Value
snippets-expand-preview = Preview
snippets-cancel = Cancel

## History: what each change did, by field and by verb

snippets-snippet-title = Renamed { $item }
snippets-snippet-body = Edited the body of { $item }
snippets-snippet-description = Edited the description of { $item }
snippets-snippet-favorite = { $new ->
    [true] Starred { $item }
   *[false] Unstarred { $item }
}
snippets-snippet-folder = Moved { $item } to the folder { $new }
snippets-snippet-language = Set the language of { $item } to { $new ->
    [text] plain text
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
}
snippets-snippet-tags = Changed the tags of { $item }
snippets-new-snippet = Created { $title }
snippets-duplicate = Duplicated { $item }
snippets-delete = Deleted { $item }
snippets-record-use = Copied { $item }
snippets-rename-folder = Renamed the folder { $folder } to { $to }
snippets-rename-tag = Renamed the tag { $tag } to { $to }
snippets-remove-tag = Removed the tag { $tag } from every snippet
