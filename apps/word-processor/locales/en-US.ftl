# Word processor — source locale (en-US).
#
# Keys are grouped by UI surface. Single-glyph icon faces (B/I/U, ✓/✗, arrows)
# stay as literals in code and are NOT keyed here. Interpolating messages use
# Fluent placeables ({ $name }); see editor/src/i18n.rs (t_args).

## Toolbar button faces (the terse, visible labels)
toolbar-new = New ▾
toolbar-body = Body
toolbar-list = • List
toolbar-table = ⊞ Table
toolbar-image = 🖼 Image
toolbar-link = Link
toolbar-styles = Styles
toolbar-ruler = Ruler
toolbar-outline = Outline
toolbar-toc = TOC
toolbar-row-add = +Row
toolbar-row-del = −Row
toolbar-col-add = +Col
toolbar-col-del = −Col

## Toolbar accessibility names / tooltips (spelled-out for screen readers)
a11y-group-formatting = Formatting
a11y-zoom-out = Zoom out
a11y-zoom-out-description = Reduce the page scale by 10 percent
a11y-zoom-reset = Reset zoom
a11y-zoom-reset-description = Return the page scale to 100 percent
a11y-zoom-in = Zoom in
a11y-zoom-in-description = Increase the page scale by 10 percent
a11y-bold = Bold
a11y-italic = Italic
a11y-underline = Underline
a11y-strikethrough = Strikethrough
a11y-superscript = Superscript
a11y-subscript = Subscript
a11y-heading1 = Heading 1
a11y-heading2 = Heading 2
a11y-body-text = Body text
a11y-bulleted-list = Bulleted list
a11y-align-left = Align left
a11y-align-center = Align center
a11y-align-right = Align right
a11y-justify = Justify
a11y-line-spacing = Line spacing
a11y-font = Font
a11y-size-up = Increase font size
a11y-size-down = Decrease font size
a11y-font-size = Font size
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
a11y-text-color = Text color #{ $hex }
a11y-text-color-default = Default text color
a11y-highlight = Highlight #{ $hex }
a11y-highlight-clear = Clear highlight

## Crash-recovery banner
recovery-banner = Recovered unsaved changes from a previous session.
recovery-restore = Restore
recovery-discard = Discard

## Status bar
status-count = { $words } { $words ->
        [one] word
       *[other] words
    }    { $chars } { $chars ->
        [one] character
       *[other] characters
    }
status-untitled = Untitled
format-odt = ODF Text
format-docx = Word Document
format-markdown = Markdown
format-pdf = PDF
format-html = HTML
format-rtf = Rich Text
format-epub = EPUB

## Header / footer edit-mode tabs
band-header = Header
band-footer = Footer

## Find / find-and-replace bar
find-find = Find
find-replace-with = Replace with
find-bad-pattern = Bad pattern
find-no-results = No results
find-count = { $cur } / { $total }
find-replace = Replace
find-replace-all = All

## Review — context-menu rows
review-accept-insertion = Accept insertion
review-reject-insertion = Reject insertion
review-accept-deletion = Accept deletion
review-reject-deletion = Reject deletion
review-resolve-comment = Resolve comment
review-reply = Reply
review-thread = Comment thread
review-thread-unavailable = That comment is no longer available
review-reply-placeholder = Write a reply…
review-send-reply = Send
review-cancel-reply = Cancel
review-you = You
review-accept-all = Accept all changes
review-reject-all = Reject all changes

## Review / comment margin cards
review-comment = Comment
review-resolve = ✓ Resolve
review-insertion = Insertion
review-deletion = Deletion

## Edit context-menu (clipboard + spell)
menu-add-to-dictionary = Add to dictionary
menu-cut = Cut
menu-copy = Copy
menu-paste = Paste
menu-select-all = Select All

## Templates (New ▾ menu)
template-blank = Blank
template-letter = Letter
template-report = Report
template-resume = Resume
template-meeting-notes = Meeting Notes

## Built-in style display names (localized). Blocks store a stable English name
## for export interop; these are what the picker + style header actually show.
style-body = Body
style-heading1 = Heading 1
style-heading2 = Heading 2
style-heading3 = Heading 3
style-list = List Item
style-quote = Quote

## Paragraph panel
panel-this-paragraph = THIS PARAGRAPH
panel-line-spacing = Line spacing
panel-style = PARAGRAPH STYLE
# Caption under the style name — makes the preset-vs-direct distinction explicit:
# the toolbar formats the selection; this section edits the shared style.
panel-style-hint = Changes every "{ $name }" paragraph
panel-caret-hint = Place the caret in a paragraph
panel-paragraph-styles = PARAGRAPH STYLES
panel-style-properties = STYLE PROPERTIES
panel-font = Font
panel-font-serif = Serif
panel-font-mono = Mono
panel-font-sans = Sans
panel-weight = Weight
panel-bold = Bold
panel-italic = Italic
panel-colour = Colour
panel-colour-custom = Custom…
panel-align = Align
panel-new-style = + New style from this
panel-num-size = Size
panel-num-space-before = Space before
panel-num-space-after = Space after
panel-num-indent = Indent

## Image inspector (styles panel, when an image is selected)
panel-image = IMAGE
panel-image-size = Size
panel-image-width = Width
panel-image-flip = Flip
panel-image-flip-h = Horizontal
panel-image-flip-v = Vertical
panel-image-smaller = Smaller
panel-image-larger = Larger
panel-image-crop = Crop
panel-image-crop-inset = Crop 5%
panel-image-crop-reset = Reset
panel-image-position = Position
panel-image-free = Free
panel-image-wrap-left = Left
panel-image-wrap-right = Right

## Paragraph-style context menu (right-click a style chip)
style-menu-delete = Delete style
style-menu-duplicate = Duplicate style

## Transient toasts (confirmation cues)
toast-toc-updated = Table of contents updated ({ $n } { $n ->
        [one] entry
       *[other] entries
    })
toast-toc-empty = Add headings first — a table of contents lists them
toast-style-deleted = Style deleted
toast-style-duplicated = Style duplicated
toast-saved = Document saved
toast-save-failed = Could not save the document
toast-open-failed = Could not open the document. Your current document is unchanged.
toast-view-only = This document is open to view only.
unsaved-title = Save changes?
unsaved-body = Save this document before replacing it.
unsaved-save = Save
unsaved-discard = Discard changes
unsaved-cancel = Cancel
unsaved-close = Close
toast-file-dialog-failed = Could not open the file chooser

## Outline / navigation pane
outline-header = OUTLINE
outline-empty = No headings yet
outline-untitled = (untitled)

## Page layout (ruler) bottom bar
layout-margins = Margins
layout-spacing = Spacing
layout-none-hint = Page layout — pick Margins or Spacing, then drag on the page
layout-margins-readout = margins  L { $l }  T { $t }  R { $r }  B { $b }   ·   drag = opposite pair · Shift = one side · Ctrl = all proportional
layout-spacing-readout = { $name } spacing  { $px } px   ·   drag the handle — every "{ $name }" block follows

## Canvas
a11y-document-editor = Document editor
a11y-group-review = Review suggestions
a11y-group-image-placement = Image position
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
canvas-embedded-content = ⟦ embedded content ⟧
canvas-empty-placeholder = Start writing…
toolbar-history = History
toolbar-more = More ▾
toolbar-text-color = Text color…
toolbar-highlight = Highlight…
picker-text-color = Text color
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
a11y-more-tools = More formatting and document tools
a11y-version-history = Version history
panel-close = Close

# A selected file differs from the current admitted workspace document.
file-choice-title = Choose the document to open
file-choice-body = This file differs from the workspace document. Keep the workspace version, or import the selected file into its history.
file-choice-keep = Keep workspace version
file-choice-use = Use selected file
