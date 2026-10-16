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
notes-surface-rename = Click to rename this note
notes-surface-tag = Show notes tagged { $tag }
notes-surface-backlink = Open linking note: { $title }
notes-parent-path = In { $path }
notes-linked-from = Linked from
notes-title-placeholder = Untitled
notes-save-failed-detail = Your latest changes are not stored yet.

notes-affordance-new-note = New note
notes-affordance-new-child = New child note
notes-affordance-note-actions = More note actions
notes-affordance-note-row = Double-click to rename; right-click for note actions

notes-help-code = Code block
notes-help-bullet = Bullet list
notes-help-numbered = Numbered list
notes-help-checklist = Checklist

notes-help-close-compact = Esc to close
notes-help-more-compact = More below ↓
notes-help-earlier-compact = More above ↑

notes-help-code-exit = Write below code
notes-help-code-before = Write above code
notes-help-code-copy = Copy code block
notes-help-code-indent = Indent code

notes-help-code-blank = Keep blank code line
notes-help-code-select = Select code, then note

notes-scroll-position = Note scroll position
notes-scroll-help = Drag to scroll. Arrow keys move a little; Page Up and Page Down move a screen; Home and End jump to the beginning or end.
notes-help-outline-indent = Indent / outdent text or list
notes-help-editor-leave = Move focus out of the editor
notes-help-block-backspace = At block start: outdent, unstyle, then join
notes-help-block-indent = Indent / outdent a whole block
notes-title = Notes
notes-menu-new-child = New child note
notes-menu-rename = Rename
notes-menu-duplicate = Duplicate
notes-menu-copy-link = Copy note link
notes-menu-export = Export Markdown
notes-menu-insert-image = Insert image
notes-menu-delete = Delete
notes-a11y-new-child-under = New child note under
notes-a11y-more-actions-for = More actions for

notes-view-editor = Editor

notes-help-block-duplicate = Duplicate blocks and their children

notes-help-block-move = Move focused or selected blocks

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
