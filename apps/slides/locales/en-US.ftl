# Slides — editor chrome. Every string a user reads in the toolbar, the
# slide rail, the properties inspector, the insert menu, the overview and the
# presenter view. Keys are `slides-*` so they never collide with another
# app's bundle in the shared registry.

## Toolbar
slides-present = Present
slides-close = Close

## Slide rail
slides-slides = SLIDES
slides-add = Add
slides-dup = Dup
slides-del = Del
slides-drop-to-delete = Drop to delete
slides-rail-subtitle = Slide { $current } of { $total }  ·  { $palette }

## Insert tools (bar face / overflow-menu row)
slides-tool-slide = Slide
slides-tool-new-slide = New slide
slides-tool-text = Text
slides-tool-text-box = Text box
slides-tool-rect = Rect
slides-tool-rectangle = Rectangle
slides-tool-ellipse = Ellipse
slides-tool-line = Line
slides-tool-image = Image

## Properties inspector
slides-fill = FILL
slides-stroke = STROKE
slides-type = TYPE
slides-align = ALIGN
slides-arrange = ARRANGE
slides-layout = LAYOUT
slides-background = BACKGROUND
slides-deck-theme = DECK THEME
slides-slide-n = SLIDE { $n }
slides-speaker-notes = SPEAKER NOTES
slides-notes-placeholder = Click to add speaker notes…
slides-speaker-notes-editing = SPEAKER NOTES  (editing — Esc)
slides-custom-colour = Custom colour…
slides-custom-colour-hex = Custom  ·  { $hex }
slides-fill-colour = FILL COLOUR
slides-bring-front = Bring front
slides-send-back = Send back
slides-duplicate = Duplicate
slides-delete = Delete

## Overview (slide sorter)
slides-overview-hint = { $count } slides  ·  click to open  ·  Esc / G to return

## Presenter view
slides-presenter-view = PRESENTER VIEW
slides-elapsed = ELAPSED
slides-paused = PAUSED
slides-position = { $state }  ·  slide { $current } / { $total }
slides-current = CURRENT
slides-next = NEXT
slides-end-of-deck = End of deck
slides-no-notes = No notes for this slide.
slides-presenter-keys = ← →  navigate    Home / End  first · last    P  hide dashboard    K  pause timer    0  reset    Esc  exit

## Accessible names (read by assistive technology and the control socket)
slides-a11y-new-slide = New slide
slides-a11y-duplicate-slide = Duplicate slide
slides-a11y-delete-slide = Delete slide
slides-a11y-start-presentation = Start presentation
slides-a11y-slide-overview = Slide overview
slides-a11y-open-slide = Open slide { $n }
slides-a11y-change-theme = Change deck theme, currently { $name }
slides-a11y-insert = Insert { $kind }
slides-a11y-fill = Fill { $role }
slides-a11y-stroke = Stroke { $role }
slides-a11y-custom-fill = Choose custom fill colour
slides-a11y-bold = Bold
slides-a11y-italic = Italic
slides-a11y-align = Align { $align }
slides-a11y-bring-front = Bring to front
slides-a11y-send-back = Send to back
slides-a11y-duplicate-element = Duplicate element
slides-a11y-delete-element = Delete element
slides-a11y-layout = Layout { $name }
slides-a11y-background = Background { $name }
slides-a11y-deck-theme = Deck theme { $name }
slides-a11y-edit-notes = Edit speaker notes
slides-a11y-slides-panel = Slides panel
slides-a11y-properties-panel = Properties panel
slides-a11y-more-tools = More insert tools

## Tooltips
slides-tip-toggle-slides = Show or hide slides
slides-tip-toggle-properties = Show or hide properties
slides-tip-overview = Slide overview
slides-tip-theme = Change deck theme
slides-tip-more-tools = More insert tools

## Transient messages
slides-toast-open-failed = Could not open the presentation
slides-toast-import-failed = Could not start presentation import
slides-toast-export-failed = Could not start presentation export
slides-toast-chooser-failed = Could not open the file chooser
slides-toast-export-limit = Presentation exceeds the safe export limit
slides-toast-opened = Presentation opened
slides-toast-saved = Presentation saved
slides-toast-process-failed = Could not process the presentation
slides-toast-image-chooser-failed = Could not open the image chooser
slides-toast-image-import-failed = Could not import image (it may be too large or invalid)
slides-toast-opening-pptx = Opening PowerPoint presentation…
slides-toast-shared-limit = Shared presentation exceeds safe limits; update was not materialized

## File portal
slides-pick-image-title = Insert image
slides-pick-image-accept = Insert
slides-pick-images-filter = Images
slides-open-title = Open presentation
slides-open-accept = Open
slides-save-title = Save presentation
slides-save-accept = Save
slides-pick-presentations-filter = Presentations (.odp, .pptx)
