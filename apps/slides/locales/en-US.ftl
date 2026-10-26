# Slides — editor chrome. Every string a user reads in the window bar, the
# slide sidebar, the properties inspector, the menus, the overview, the
# presenter view and the keyboard reference. Keys are `slides-*` so they never
# collide with another app's bundle in the shared registry.

## Window bar
slides-deck-name = Presentation title
slides-untitled = Untitled presentation
slides-view = View
slides-view-editor = Editor
slides-view-overview = Overview
slides-insert = Insert
slides-show-properties = Show properties
slides-hide-properties = Hide properties

## Slide sidebar
slides-drop-to-delete = Drop to delete

## Insert tools
slides-tool-text = Text
slides-tool-rectangle = Rectangle
slides-tool-ellipse = Ellipse
slides-tool-line = Line
slides-tool-image = Image

## Properties inspector
slides-fill = Fill
slides-stroke = Stroke
slides-font = Font
slides-alignment = Alignment
slides-arrange = Arrange
slides-layout = Layout
slides-background = Background
slides-deck-theme = Deck theme
slides-look = Look
slides-look-light = Light
slides-look-dark = Dark
slides-slide-n = Slide { $n }
slides-speaker-notes = Speaker notes
slides-notes-placeholder = Add speaker notes
slides-custom-colour = Custom colour
slides-custom-colour-hex = Custom colour { $hex }
slides-fill-colour = Fill colour
slides-bold = Bold
slides-italic = Italic
slides-bulleted-list = Bulleted list
slides-numbered-list = Numbered list
slides-bring-front = Bring to front
slides-send-back = Send to back
slides-duplicate = Duplicate
slides-delete = Delete

## Colour roles
slides-colour-none = No stroke
slides-colour-accent = Accent
slides-colour-accent-soft = Soft accent
slides-colour-surface = Surface
slides-colour-deep = Deep
slides-colour-text = Text
slides-colour-white = White
slides-colour-green = Green
slides-colour-amber = Amber
slides-colour-red = Red
slides-colour-violet = Violet

## Version history
slides-history-no-slides = This version has no slides.
slides-history-no-deck = This version has no presentation to restore.

## Slide content placeholders
slides-prompt-title = Add a title
slides-prompt-text = Add text
slides-image-loading = Loading image
slides-image-unavailable = Image unavailable

## Presenter view
slides-position = Slide { $current } of { $total }
slides-position-paused = Paused on slide { $current } of { $total }
slides-current = Current
slides-next = Next
slides-end-of-deck = End of the presentation
slides-no-notes = No notes for this slide.
slides-presenter-keys = Left and right arrows move between slides, P hides the dashboard, K pauses the timer, 0 resets it, Esc ends the presentation

## Keyboard reference
slides-shortcuts-title = Keyboard shortcuts
slides-shortcuts-editing = Editing
slides-shortcuts-presenting = Presenting
slides-key-new-slide = New slide
slides-key-present = Present from the current slide
slides-key-overview = Show all slides
slides-key-insert = Insert a text box, rectangle, ellipse or line
slides-key-cycle = Select the next object on the slide
slides-key-edit-text = Edit the selected text box
slides-key-nudge = Move the selected object
slides-key-restack = Send back or bring to front
slides-key-duplicate = Duplicate the selected object
slides-key-delete = Delete the selected object
slides-key-undo = Undo or redo
slides-key-help = Show or hide this reference
slides-key-step = Previous or next slide
slides-key-first-last = First or last slide
slides-key-jump = Go to that slide
slides-key-presenter = Show or hide the presenter dashboard
slides-key-notes = Show or hide speaker notes
slides-key-blank = Black or white screen
slides-key-timer = Pause or reset the timer
slides-key-exit = End the presentation

## Accessible names (read by assistive technology and the control socket)
slides-a11y-new-slide = New slide
slides-a11y-duplicate-slide = Duplicate slide
slides-a11y-delete-slide = Delete slide
slides-a11y-start-presentation = Start presentation
slides-a11y-open-slide = Open slide { $n }
slides-a11y-insert = Insert { $kind }
slides-a11y-fill = Fill { $role }
slides-a11y-stroke = Stroke { $role }
slides-a11y-custom-fill = Choose a custom fill colour
slides-a11y-bold = Bold
slides-a11y-italic = Italic
slides-a11y-bulleted-list = Bulleted list
slides-a11y-numbered-list = Numbered list
slides-a11y-align = Align { $align }
slides-a11y-bring-front = Bring to front
slides-a11y-send-back = Send to back
slides-a11y-duplicate-element = Duplicate object
slides-a11y-delete-element = Delete object
slides-a11y-layout = Layout { $name }
slides-a11y-layout-menu = Slide layout, { $name }
slides-a11y-background = Background { $name }
slides-a11y-deck-theme = Deck theme { $name }
slides-a11y-deck-look = Deck look { $name }
slides-a11y-edit-notes = Edit speaker notes

## Transient messages
slides-toast-open-failed = Could not open the presentation
slides-toast-import-failed = Could not start presentation import
slides-toast-export-failed = Could not start presentation export
slides-toast-chooser-failed = Could not open the file chooser
slides-toast-export-limit = Presentation exceeds the safe export limit
slides-toast-process-failed = Could not process the presentation
slides-toast-image-chooser-failed = Could not open the image chooser
slides-toast-image-import-failed = Could not import image (it may be too large or invalid)
slides-toast-opening-pptx = Opening PowerPoint presentation…
slides-toast-shared-limit = The shared presentation is too large to show

## File portal
slides-pick-image-title = Insert image
slides-pick-image-accept = Insert
slides-pick-images-filter = Images
slides-open-title = Open presentation
slides-open-accept = Open
slides-save-title = Save presentation
slides-save-accept = Save
slides-pick-presentations-filter = Presentations (.odp, .pptx)

## History: what each change did, by field and by verb
slides-deck-title = Renamed the deck
slides-deck-palette = Changed the theme to { $new }
slides-deck-look = Changed the look to { $new }
slides-slide-layout = Changed a slide's layout to { $new }
slides-slide-background = Changed a slide's background to { $new }
slides-slide-notes = Edited the speaker notes
slides-element-x = Moved { $item }
slides-element-y = Moved { $item }
slides-element-w = Resized { $item }
slides-element-h = Resized { $item }
slides-element-body = Edited { $item }
slides-element-src = Changed the picture of { $item }
slides-element-fill = Changed the colour of { $item }
slides-element-stroke = Changed the outline of { $item }
slides-element-stroke-width = Changed the outline width of { $item }
slides-element-font-size = Changed the font size of { $item }
slides-element-align = Aligned { $item } { $new }
slides-element-bold = { $new ->
    [true] Made { $item } bold
   *[other] Made { $item } not bold
}
slides-element-italic = { $new ->
    [true] Made { $item } italic
   *[other] Made { $item } not italic
}
slides-element-radius = Rounded the corners of { $item }
slides-add-slide = Added a slide
slides-insert-slide = Added a slide
slides-duplicate-slide = Duplicated a slide
slides-delete-slide = Deleted a slide
slides-move-slide = Moved a slide
slides-set-text = Rewrote a slide's text
slides-add-element = Added an element to a slide
slides-insert-element = Added an element to a slide
slides-delete-element = Deleted { $item }
slides-bring-to-front = Brought { $item } to the front
slides-send-to-back = Sent { $item } to the back
slides-replace-deck = Replaced the deck with { $title }

## Zoom: how large the slide is drawn in the editing pane

slides-zoom-fit = Fit to window
slides-zoom-at = Zoom: { $zoom }
