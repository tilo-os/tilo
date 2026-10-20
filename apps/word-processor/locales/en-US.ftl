# Word processor — source locale (en-US).
#
# Keys are grouped by UI surface. Single-letter toolbar faces (B/I/U)
# stay as literals in code and are NOT keyed here. Interpolating messages use
# Fluent placeables ({ $name }); see editor/src/i18n.rs (t_args).

## Toolbar button faces (the terse, visible labels)
toolbar-new = New
toolbar-table = Table
toolbar-link = Link
toolbar-styles = Styles
toolbar-ruler = Ruler
toolbar-toc = Contents
toolbar-row-add = Add row
toolbar-row-del = Delete row
toolbar-col-add = Add column
toolbar-col-del = Delete column
toolbar-bold = Bold
toolbar-italic = Italic
toolbar-underline = Underline
toolbar-strikethrough = Strikethrough
toolbar-superscript = Superscript
toolbar-subscript = Subscript
toolbar-size-down = Smaller
toolbar-size-up = Larger
toolbar-image = Image
toolbar-table-delete = Delete table

## Toolbar accessibility names / tooltips (spelled-out for screen readers)
a11y-group-formatting = Formatting
a11y-bold = Bold
a11y-italic = Italic
a11y-underline = Underline
a11y-strikethrough = Strikethrough
a11y-superscript = Superscript
a11y-subscript = Subscript
a11y-line-spacing = Line spacing
a11y-font = Font
a11y-size-up = Increase font size
a11y-size-down = Decrease font size
a11y-insert-image = Insert image
a11y-hyperlink = Add or change hyperlink
a11y-insert-table = Insert table
a11y-table-row-add = Add table row
a11y-table-row-del = Delete table row
a11y-table-col-add = Add table column
a11y-table-col-del = Delete table column
a11y-table-delete = Delete table
a11y-styles-panel = Styles panel
a11y-ruler = Ruler
a11y-document-outline = Document outline
a11y-insert-toc = Insert table of contents
a11y-new-from-template = New from template
a11y-text-color = Text colour #{ $hex }
a11y-text-color-default = Default text colour
a11y-highlight = Highlight #{ $hex }
a11y-highlight-clear = Clear highlight

## Status bar
status-untitled = Untitled

## Header / footer edit-mode tabs
band-header = Header
band-footer = Footer

## Review — context-menu rows
review-accept-insertion = Accept insertion
review-reject-insertion = Reject insertion
review-accept-deletion = Accept deletion
review-reject-deletion = Reject deletion
review-resolve-comment = Resolve comment
review-reply = Reply
review-thread-unavailable = That comment is no longer available
review-reply-placeholder = Write a reply
review-send-reply = Send
review-cancel-reply = Cancel
review-accept-all = Accept all changes
review-reject-all = Reject all changes

## Review / comment margin cards
review-comment = Comment
review-resolve = Resolve
review-insertion = Insertion
review-deletion = Deletion

## Edit context-menu (clipboard + spell)
menu-add-to-dictionary = Add to dictionary
menu-cut = Cut
menu-copy = Copy
menu-paste = Paste
menu-select-all = Select all

## Templates (the New menu)
template-blank = Blank
template-letter = Letter
template-report = Report
template-resume = Resume
template-meeting-notes = Meeting notes

## Built-in style display names (localized). Blocks store a stable English name
## for export interop; these are what the picker + style header actually show.
style-body = Body
style-heading1 = Heading 1
style-heading2 = Heading 2
style-heading3 = Heading 3
style-list = List item
style-quote = Quote

## Paragraph panel
panel-this-paragraph = This paragraph
panel-line-spacing = Line spacing
panel-style = Paragraph style
# Caption under the style name — makes the preset-vs-direct distinction explicit:
# the toolbar formats the selection; this section edits the shared style.
panel-style-hint = Edits below change every paragraph in this style.
panel-caret-hint = Place the caret in a paragraph
panel-font = Font
panel-font-serif = Serif
panel-font-mono = Mono
panel-font-sans = Sans
panel-weight = Emphasis
panel-bold = Bold
panel-italic = Italic
panel-colour = Colour
panel-colour-custom = Custom colour
panel-align = Alignment
panel-new-style = New style from this paragraph
panel-num-size = Size
panel-num-space-before = Space before
panel-num-space-after = Space after
panel-num-indent = Indent

## Image inspector (styles panel, when an image is selected)
panel-image = Image
panel-image-size = Size
panel-image-flip = Flip
panel-image-flip-h = Horizontal
panel-image-flip-v = Vertical
panel-image-smaller = Smaller
panel-image-larger = Larger
panel-image-crop = Crop
panel-image-crop-inset = Crop edges
panel-image-crop-reset = Reset
panel-image-position = Position
panel-image-inline = In line
panel-image-free = Free
panel-image-wrap-left = Left
panel-image-wrap-right = Right

## Paragraph-style context menu (right-click a style chip)
style-menu-delete = Delete style
style-menu-duplicate = Duplicate style

## Transient toasts (confirmation cues)
toast-toc-empty = Add headings first — a table of contents lists them
toast-save-failed = Could not save the document
toast-view-only = This document is open to view only.
toast-file-dialog-failed = Could not open the file chooser

## Outline / navigation pane
outline-header = Outline
outline-empty = No headings yet
outline-untitled = Untitled heading

## Page layout (ruler) bottom bar
layout-margins = Margins
layout-spacing = Spacing
layout-margins-readout = Left { $l } px   Top { $t } px   Right { $r } px   Bottom { $b } px
layout-spacing-readout = { $name } paragraphs: { $px } px after

## Canvas
a11y-document-editor = Document editor
a11y-group-review = Review suggestions
a11y-group-image-placement = Image position
a11y-image-inline = Image in line with text
a11y-image-inline-description = Keep the image in the text flow, between paragraphs
a11y-image-free-position = Free image position
a11y-image-free-position-description = Place the image freely on the page without wrapping text
a11y-image-wrap-left = Image left with text wrap
a11y-image-wrap-left-description = Place the image on the left and wrap text to its right
a11y-image-wrap-right = Image right with text wrap
a11y-image-wrap-right-description = Place the image on the right and wrap text to its left
page-indicator = Page { $n }
# On-page placeholders (a missing/undecodable image; an embedded object we render
# as a fidelity box rather than its content).
canvas-image-missing = image
canvas-embedded-content = Embedded content
canvas-empty-placeholder = Start writing…
toolbar-text-color = Colour
toolbar-highlight = Highlight
picker-text-color = Text colour
picker-highlight = Highlight
a11y-text-colour-name = { $colour } text
a11y-highlight-name = { $colour } highlight
color-black = black
color-gray = gray
color-red = red
color-blue = blue
color-green = green
color-amber = amber
color-purple = purple
color-yellow = yellow
color-cyan = cyan
color-pink = pink
color-orange = orange
panel-close = Close

# A selected file differs from the current admitted workspace document.
file-choice-title = Choose the document to open
file-choice-body = This file differs from the workspace document. Keep the workspace version, or import the selected file into its history.
file-choice-keep = Keep workspace version
file-choice-use = Use selected file

## Inspector and status band
status-words = { $words } { $words ->
        [one] word
       *[other] words
    }
status-chars = { $chars } { $chars ->
        [one] character
       *[other] characters
    }
a11y-zoom = Zoom
line-spacing-single = Single
line-spacing-double = Double
panel-align-left = Left
panel-align-center = Centre
panel-align-right = Right
panel-align-justify = Justify

## History: what each change did, by field and by verb
word-processor-document-body = { $op ->
    [splice] Edited the text
    [mark] { $on ->
        [true] Made { $characters } characters { $mark ->
            [strong] bold
            [em] italic
            [underline] underlined
            [strike] struck through
           *[other] { $mark }
        }
       *[false] Removed { $mark } from { $characters } characters
    }
    [set] { $kind ->
        [heading] Made a heading of level { $level }
        [list_item] Made a list
        [paragraph] Made a body paragraph
       *[other] Changed the paragraph style
    }
    [insert] { $object ->
        [table] Inserted a table
        [image] Inserted an image
        [page-break] Inserted a page break
       *[other] Inserted into the text
    }
    [remove] Removed { $blocks } objects
    [replace] Replaced the whole text
   *[other] Edited the text
}
word-processor-append-paragraph = Added a paragraph
word-processor-insert-table = Inserted a table of { $rows } by { $columns }
word-processor-add-row = Added a row to a table
word-processor-add-column = Added a column to a table
word-processor-delete-row = Deleted a row of a table
word-processor-delete-column = Deleted a column of a table
word-processor-delete-table = Deleted a table
word-processor-delete-image = Deleted an image
word-processor-accept-change = Accepted a change
word-processor-reject-change = Rejected a change
word-processor-accept-all-changes = Accepted every change
word-processor-reject-all-changes = Rejected every change
word-processor-add-comment = Commented: { $text }
word-processor-reply-to-comment = Replied: { $text }
word-processor-resolve-comment = Resolved a comment
word-processor-set-page-margins = Changed the page margins
word-processor-generate-toc = Added a table of contents
word-processor-note-file-written = Saved to its file
