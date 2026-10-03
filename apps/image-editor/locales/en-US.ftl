# Image Editor — user-facing strings.

# Header commands.
image-editor-open = Open
image-editor-open-image = Open image
image-editor-undo = Undo
image-editor-redo = Redo
image-editor-export = Export

# Adjustments, as toolbar commands and as the steps they add.
image-editor-exposure = Exposure
image-editor-contrast = Contrast
image-editor-colour = Color
image-editor-mono = Black and white
image-editor-rotate = Rotate
image-editor-crop = Crop

# View commands.
image-editor-fit = Fit
image-editor-compare = Compare
image-editor-graph = Graph

# The adjustment list and the selected step's settings.
image-editor-adjustments = Adjustments
image-editor-step = Step { $number }
image-editor-saturation = Saturation
image-editor-rotation = Rotation
image-editor-rotate-left = Rotate left
image-editor-rotate-right = Rotate right
image-editor-remove = Remove adjustment
image-editor-crop-hint = Drag the corners on the image to crop it.

# The canvas with no image.
image-editor-empty-title = No image open
image-editor-empty-message = Open an image or drop one here to start editing.

# The processing graph.
image-editor-original = Original
image-editor-result = Result
image-editor-unused = Not in the result
image-editor-graph-hint = Drag from a step's output to another step's input to reconnect them. Press G to go back.

# Transient status.
image-editor-added = Added { $step }
image-editor-removed = { $count ->
    [one] Adjustment removed
   *[other] { $count } adjustments removed
}
image-editor-undone = Change undone
image-editor-redone = Change redone
image-editor-nothing-to-undo = Nothing to undo
image-editor-nothing-to-redo = Nothing to redo
image-editor-opened = Image opened
image-editor-unsupported = That file is not a supported image
image-editor-picker-failed = The file picker could not open: { $error }
image-editor-picker-stopped = The file picker stopped responding
image-editor-exported = Exported { $file }
image-editor-export-failed = Export failed: { $error }
image-editor-export-needs-image = Open an image before exporting
image-editor-connected = Connection updated
image-editor-connect-refused = Those steps can't be connected
image-editor-connect-choose = Choose an input with Tab, then press Enter
image-editor-connect-select = Select a step before connecting it
image-editor-connect-no-output = This step has no output to connect
image-editor-connect-no-input = No compatible input is available
image-editor-connect-cancelled = Connection cancelled

# Picker titles.
image-editor-pick-title = Open an image
image-editor-pick-filter = Images
image-editor-save-title = Export image

# Names read out by assistive technology.
image-editor-edited-preview = Edited image
image-editor-original-preview = Original image
image-editor-after-step = Image after { $step }
image-editor-canvas = Image and adjustments
image-editor-graph-canvas = Processing graph
image-editor-connect-from = Connect from { $step }
image-editor-connect-to = Connect to { $step }
image-editor-enabled = Enabled
image-editor-bypassed = Bypassed
image-editor-untitled = Untitled image

# Search.
image-editor-search-project = Image project
image-editor-search-source = Original image
image-editor-search-no-source = No image open
image-editor-search-export = Export PNG
image-editor-search-result = Final image
