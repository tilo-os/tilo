ebook-library = Books

## History: what each change did, by field and by verb

ebook-shelve = Put { $title } on the shelf
ebook-shelve-found = Found { $title } in a watched folder
ebook-remove-from-library = Removed a book from the library
ebook-record-progress = Read on in a book
ebook-add-note = Added a note
ebook-add-comment = Commented on a highlight
ebook-toggle-bookmark = Bookmarked a page, or took its bookmark off
ebook-highlight = Highlighted a passage
ebook-change-color = Recoloured a highlight { $color }
ebook-delete-highlight = Deleted a highlight
ebook-delete-annotation = Deleted an annotation
ebook-remove-font = Removed the font { $item }
ebook-delete-scheme = Deleted the colour scheme { $item }

ebook-library-entry-title = Retitled { $item }
ebook-library-entry-author = Changed the author of { $item }
ebook-library-entry-subject = Changed the subject of { $item }
ebook-ebook-entries = Changed the shelf
ebook-ebook-progress = Changed where the reader is
ebook-ebook-default-font = Chose the default font
ebook-ebook-fonts = Changed the fonts
ebook-ebook-schemes = Changed the colour schemes
ebook-ebook-annotations = Changed the annotations
ebook-progress-chapter = Moved to chapter { $new }
ebook-progress-block = Moved within a chapter
ebook-progress-fraction = Read on
ebook-progress-opened = Opened a book
ebook-settings-font-scale = Set the text size to { $new }
ebook-settings-line-spacing = Set the line spacing to { $new }
ebook-settings-font-id = Chose a reading font
ebook-settings-theme = Set the page tint to { $new }
ebook-settings-scheme-id = Chose a colour scheme
ebook-settings-mode = Set the layout to { $new ->
    [paged] pages
    [continuous] scrolling
   *[two_up] two pages
}
ebook-settings-composition = Set the text to { $new ->
    [ragged] ragged
   *[book] justified
}
ebook-library-prefs-sort = Sorted the shelf by { $new }
ebook-library-prefs-hide-read = { $new ->
    [true] Hid finished books
   *[false] Showed finished books
}
ebook-font-entry-kind = Changed the face of { $item }
ebook-font-entry-name = Renamed the font { $item }
ebook-font-entry-size-mult = Resized the font { $item }
ebook-font-entry-shown = { $new ->
    [true] Showed { $item } in quick settings
   *[false] Hid { $item } from quick settings
}
ebook-color-scheme-name = Renamed the colour scheme { $item }
ebook-color-scheme-page = Changed the page colour of { $item }
ebook-color-scheme-ink = Changed the text colour of { $item }
ebook-color-scheme-accent = Changed the accent colour of { $item }
ebook-color-scheme-dark = Changed whether { $item } is dark
ebook-color-scheme-enabled = { $new ->
    [true] Turned on the colour scheme { $item }
   *[false] Turned off the colour scheme { $item }
}
ebook-mark-chapter = Moved “{ $item }”
ebook-mark-block = Moved “{ $item }”
ebook-mark-start = Moved “{ $item }”
ebook-mark-end = Moved “{ $item }”
ebook-mark-excerpt = Moved an annotation to “{ $new }”
ebook-mark-text = Wrote on “{ $item }”
ebook-mark-color = Recoloured “{ $item }”
ebook-mark-created = Finished a note on “{ $item }”

## What the reader and its panels are called for assistive technology

ebook-a11y-reading-page = Current reading page
ebook-a11y-progress-hint = Drag or use left and right arrow keys to move through the book
ebook-field-name = Name
ebook-a11y-comments = Highlight comments
ebook-a11y-edit-note = Edit note
ebook-a11y-choose-colour = Choose colour

## Window and accessibility copy
ebook-drop-epub = Drop an EPUB here or choose one

## Facts, property labels and section titles
ebook-key-reserved = { $key } is reserved for a built-in reader command. Choose another key.
ebook-a11y-annotations = { $annotations ->
    [one] One annotation. Scroll this list to browse it.
   *[other] Annotations: { $annotations } total. Scroll this list to browse every annotation.
}
ebook-a11y-contents = Contents: chapter { $chapter } of { $chapters }. Scroll this list to browse every chapter.
ebook-a11y-annotation-hit = { $kind } in chapter { $chapter } of { $title }

## What the window shows
ebook-view-library = Books library
ebook-view-library-detail = Browse and open books
ebook-view-reader = Book reader
ebook-view-reader-detail = Read and annotate the current book
ebook-view-settings = Book settings
ebook-view-settings-detail = Manage reading fonts, colours, and library folders
ebook-view-search = Search books
ebook-view-search-detail = Results in your books and the store
ebook-add-note-button = Add note
ebook-no-passage = No passage
ebook-query-get = Get { $book } from { $store }
ebook-query-open = Open { $title }
ebook-duplicate = Duplicate
ebook-edit = Edit
ebook-add-system-font = Add a system font
ebook-new-scheme = New scheme
ebook-highlighted-passage = Highlighted passage
ebook-selected-passage = Selected passage
ebook-manage-fonts = Manage fonts and colour themes
ebook-close-appearance = Close reading appearance
ebook-use-font = Use { $font } font
ebook-reading-theme = { $theme ->
    [light] Light reading theme
    [sepia] Sepia reading theme
    [dark] Dark reading theme
   *[high-contrast] High-contrast reading theme
}
ebook-use-scheme = Use { $scheme } colour theme
ebook-untitled = Untitled

ebook-get = Get

## What an action is called, in a tooltip and for assistive technology
ebook-action-open-book = Open book
ebook-action-share-book = Share book with workspace
ebook-action-share-book-title = Share { $book } with workspace
ebook-action-remove-book = Remove book from library
ebook-action-remove-book-title = Remove { $book } from library
ebook-action-back-to-library = Back to library
ebook-action-previous-page = Previous page
ebook-action-next-page = Next page
ebook-action-reading-progress = Reading progress: { $progress }
ebook-action-go-to-chapter = Go to chapter { $number }
ebook-action-contents = Contents
ebook-action-annotations = Annotations
ebook-action-reading-appearance = Reading appearance
ebook-action-bookmark-page = Bookmark this page
ebook-action-increase-text = Increase text size
ebook-action-decrease-text = Decrease text size
ebook-action-increase-spacing = Increase line spacing
ebook-action-decrease-spacing = Decrease line spacing
ebook-action-next-typeface = Use next typeface
ebook-action-mode = { $mode ->
    [paged] Pages layout
    [continuous] Scrolling layout
   *[two_up] Two-page layout
}
ebook-action-composition = { $style ->
    [book] Justified text
   *[ragged] Ragged text
}
ebook-action-highlight = { $colour ->
    [yellow] Highlight in yellow
    [green] Highlight in green
    [pink] Highlight in pink
    [blue] Highlight in blue
    [purple] Highlight in purple
   *[custom] Highlight in a custom colour
}
ebook-action-copy-selection = Copy selected text
ebook-action-reveal-annotation = Reveal annotation
ebook-action-delete-annotation = Delete annotation
ebook-action-annotation = { $action }: { $text }
ebook-action-open-comments = Open highlight comments
ebook-action-close-comments = Close comments
ebook-action-delete-highlight = Delete highlight
ebook-action-add-comment = Add comment
ebook-action-edit-comment = Edit comment
ebook-action-delete-comment = Delete comment
ebook-action-watch-folder = Watch { $folder }
ebook-keep-catalogues = Keep book catalogues on this device
ebook-tab-library = Library
ebook-tab-fonts = Fonts
ebook-tab-schemes = Colour schemes
ebook-tab-shortcuts = Shortcuts
ebook-action-press-key = Press a key for { $shortcut }; Escape cancels
ebook-action-shortcut-error = { $shortcut }: { $error }
ebook-action-change-shortcut = Change { $shortcut } shortcut, currently { $keys }
ebook-action-show-scheme = Show { $scheme } in reader
ebook-action-duplicate-scheme = Duplicate { $scheme }
ebook-action-delete-scheme = Delete { $scheme }
ebook-action-edit-scheme = Edit { $scheme }
ebook-action-done-editing-theme = Done editing the theme
ebook-action-new-colour-theme = New colour theme
ebook-action-edit-scheme-colour = { $slot ->
    [page] Edit the page colour of { $scheme }
    [text] Edit the text colour of { $scheme }
    [accent] Edit the accent colour of { $scheme }
   *[custom] Edit the custom colour of { $scheme }
}
ebook-action-rename-scheme = Rename { $scheme }
ebook-action-show-font = Show { $font } in reader
ebook-action-remove-font = Remove { $font }
ebook-action-add-font = Add font
ebook-action-close-font-picker = Close font picker
ebook-action-add-family = Add { $family }
ebook-action-rename-font = Rename { $font }
ebook-action-open-result = Open result
ebook-action-get-book = Get book
ebook-action-back = Back
ebook-empty-comment = Empty comment
