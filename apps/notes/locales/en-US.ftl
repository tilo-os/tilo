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
