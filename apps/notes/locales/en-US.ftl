notes-created-date = Created { $date }
notes-edited-date = Edited { $date }
notes-dates = Created { $created }, edited { $edited }
notes-date-today = today at { $time }
notes-date-day = { $day } { $month }
notes-date-day-year = { $day } { $month } { $year }
notes-table-add-row = Add row below
notes-table-add-column = Add column after
notes-table-delete-row = Delete row
notes-table-delete-column = Delete column
notes-surface-tag = Show notes tagged { $tag }
notes-surface-backlink = Open linking note: { $title }
notes-parent-path = In { $path }
notes-linked-from = Linked from
notes-title-placeholder = Untitled
notes-save-failed-detail = Your latest changes are not stored yet.

notes-affordance-new-note = New note
notes-affordance-new-child = New sub-note
notes-affordance-note-actions = More note actions

notes-scroll-position = Note scroll position
notes-scroll-help = Drag to scroll. Arrow keys move a little; Page Up and Page Down move a screen; Home and End jump to the beginning or end.
notes-title = Notes
notes-menu-new-child = New sub-note
notes-menu-rename = Rename
notes-menu-duplicate = Duplicate
notes-menu-copy-link = Copy note link
notes-menu-export = Export Markdown
notes-menu-insert-image = Insert image
notes-menu-delete = Delete
notes-a11y-new-child-under = New sub-note in
notes-a11y-more-actions-for = More actions for

notes-view-editor = Editor

## History: what each change did, by field and by verb

notes-page-body = Edited { $item }
notes-page-tags = { $op ->
    [insert] Tagged { $item }
   *[other] Untagged { $item }
}
notes-tag-name = Renamed the tag { $old } to { $new }
notes-notebook-pages = { $op ->
    [insert] Added a note
    [remove] Removed a note
   *[other] Moved a note
}
notes-notebook-tags = { $op ->
    [insert] Added a tag
   *[other] Removed a tag
}
notes-new-note = Created { $title }
notes-deleted = { $inside ->
    [0] Deleted { $title }
    [one] Deleted { $title } and 1 note inside
   *[other] Deleted { $title } and { $inside } notes inside
}
notes-undo = Undo
notes-new-child = Created { $title } in { $item }
notes-rename = Renamed { $item } to { $title }
notes-delete = Deleted { $item }
notes-move-note = Moved { $item }
notes-move-up = Moved { $item } up
notes-move-down = Moved { $item } down
notes-indent = Indented { $item }
notes-outdent = Outdented { $item }
notes-duplicate = Duplicated { $item }
notes-new-tag = Created the tag { $name }
notes-tag = Tagged { $item } with { $name }
notes-delete-tag = Deleted the tag { $item }
notes-keep-image = Kept a picture with the notebook
notes-place-image = Added a picture to { $item }
notes-import-markdown = Imported { $title }

## Keyboard shortcuts sheet
notes-help-title = Keyboard shortcuts
notes-help-group-notes = Notes
notes-help-group-writing = Writing
notes-help-group-formatting = Formatting
notes-help-group-code = Code blocks
notes-help-group-help = Help
notes-help-new-note = New note
notes-help-new-sub-note = New sub-note
notes-help-jump = Go to a note
notes-help-back-forward = Back / forward
notes-help-rename = Rename note
notes-help-note-actions = Note actions
notes-help-note-move = Move note up / down
notes-help-note-indent = Nest / unnest note
notes-help-delete = Delete note
notes-help-find = Find in note
notes-help-block-menu = Block menu
notes-help-link = Link a note
notes-help-embed = Embed a note
notes-help-outline-indent = Indent / outdent
notes-help-block-indent = Indent / outdent with children
notes-help-block-move = Move block up / down
notes-help-block-duplicate = Duplicate block
notes-help-editor-leave = Leave the note
notes-help-bold = Bold
notes-help-italic = Italic
notes-help-inline-code = Inline code
notes-help-headings = Heading 1 / 2 / 3
notes-help-bullet = Bullet list
notes-help-numbered = Numbered list
notes-help-checklist = Checklist
notes-help-quote = Quote
notes-help-code = Code block
notes-help-code-exit = Write below
notes-help-code-before = Write above
notes-help-code-blank = Blank line
notes-help-code-indent = Indent / outdent
notes-help-code-copy = Copy code
notes-help-code-select = Select code
notes-help-shortcuts = Keyboard shortcuts
notes-help-close = Close
notes-help-soft-break = New line in the same item

## Window copy
notes-image-too-big = Image is larger than 1 MiB
notes-image-kind = Choose a PNG, JPEG or WebP image
notes-image-unfit = Image is too large to keep in a note
notes-empty-excerpt = Empty note
notes-image-paste-failed = Image not pasted: { $error }
notes-image-import-failed = Image not added: { $error }
notes-image-default-alt = Image
notes-copy-link-failed = Link not copied: the clipboard isn't available
notes-window-description = Notes and the open note
notes-menu-cut = Cut
notes-menu-copy = Copy
notes-menu-paste = Paste
notes-menu-select-all = Select all
notes-imported-untitled = Imported note
notes-no-previous = No earlier note
notes-no-next = No later note
notes-link-copied = Copied link to { $title }
notes-link-unsafe = This link can't be opened safely
notes-link-open-failed = The link didn't open
notes-import-title = Import Markdown
notes-import-accept = Import
notes-markdown-files = Markdown
notes-import-not-utf8 = The file isn't UTF-8 text
notes-imported = Imported { $title }
notes-import-failed = Not imported: { $error }
notes-export-title = Export Markdown
notes-export-accept = Export
notes-insert-image-title = Insert image
notes-insert-image-accept = Insert
notes-image-files = Images
notes-image-inserted = Image added
notes-empty-title = Start your first note
notes-empty-action = New note
notes-a11y-page-title = Page title
notes-a11y-note-title = Note title
notes-a11y-first-note = Creates the first note in this workspace
notes-a11y-header = Note header
notes-a11y-header-description = Note title and scroll position
notes-a11y-tag = Tag: { $tag }
notes-a11y-backlink = Linked from { $title }
notes-search-notes = Notes
notes-search-blocks = Note blocks
notes-import-too-big = The Markdown is larger than 1 MiB
notes-import-too-many-blocks = The Markdown has more than 10,000 blocks
notes-status-saved = Saved
notes-status-saving = Saving…
notes-status-failed = Save failed
notes-status-not-saved = Not saved
notes-status-failed-because = Save failed: { $reason }. Copy this note somewhere safe, then reopen Notes
notes-status-failed-detail = Save failed. Copy this note somewhere safe, then reopen Notes
notes-status-no-storage = Not saved: this note has no storage on this device. Copy it somewhere safe, then reopen Notes
notes-status-note-count = { $notes ->
    [one] 1 note
   *[other] { $notes } notes
}
notes-status-counts = { $notes ->
    [one] 1 note
   *[other] { $notes } notes
}, { $words ->
    [one] 1 word
   *[other] { $words } words
}
