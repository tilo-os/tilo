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
status-count = { $words } words    { $chars } characters
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

## Styles panel
panel-style = STYLE
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
panel-align = Align
panel-new-style = + New style from this
panel-num-size = Size
panel-num-space-before = Space before
panel-num-space-after = Space after
panel-num-indent = Indent

## Outline / navigation pane
outline-header = OUTLINE
outline-empty = No headings yet
outline-untitled = (untitled)

## Page layout (ruler) bottom bar
layout-margins = Margins
layout-spacing = Spacing
layout-none-hint = Page layout — pick Margins or Spacing, then drag on the page

## Canvas
page-indicator = Page { $n }
