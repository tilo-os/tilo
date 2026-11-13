## History: what each change to the project did, by field and by verb

video-editor-project-name = Renamed the project to { $new }
video-editor-project-fps = Set the frame rate to { $new } fps
video-editor-project-width = Set the frame width to { $new }
video-editor-project-height = Set the frame height to { $new }
video-editor-project-format-explicit = { $new ->
    [true] Kept the project format
   *[false] Let the first clip set the project format
}
video-editor-project-mark-in = Moved the in mark
video-editor-project-mark-out = Moved the out mark

video-editor-media-item-name = Renamed the source { $old } to { $new }
video-editor-media-item-kind = Changed the kind of { $item }
video-editor-media-item-duration = Changed the length of { $item }
video-editor-media-item-width = Changed the width of { $item }
video-editor-media-item-height = Changed the height of { $item }
video-editor-media-item-fps = Changed the frame rate of { $item }
video-editor-media-item-palette = Changed the colour of { $item }
video-editor-media-item-path = Relinked { $item }
video-editor-media-item-has-audio = { $new ->
    [true] Marked { $item } as having sound
   *[false] Marked { $item } as silent
}
video-editor-text-source-content = Edited the text of { $item }
video-editor-text-source-size = Resized the text of { $item }
video-editor-text-source-color = Recoloured the text of { $item }
video-editor-text-source-align = Aligned the text of { $item }
video-editor-text-source-font = Changed the font of { $item }
video-editor-text-source-bold = { $new ->
    [true] Made { $item } bold
   *[false] Removed bold from { $item }
}
video-editor-text-source-italic = { $new ->
    [true] Made { $item } italic
   *[false] Made { $item } upright
}
video-editor-text-source-backdrop = Changed the backdrop of { $item }

video-editor-track-name = Renamed the track { $old } to { $new }
video-editor-track-kind = Changed the kind of the track { $item }
video-editor-track-index = Reordered the track { $item }
video-editor-track-muted = { $new ->
    [true] Muted { $item }
   *[false] Unmuted { $item }
}
video-editor-track-locked = { $new ->
    [true] Locked { $item }
   *[false] Unlocked { $item }
}
video-editor-track-solo = { $new ->
    [true] Soloed { $item }
   *[false] Unsoloed { $item }
}
video-editor-track-gain-db = Set the level of { $item } to { $new } dB
video-editor-track-pan = Panned { $item }

video-editor-clip-track = Moved a clip to another track
video-editor-clip-media = Replaced a clip's source
video-editor-clip-start = Moved a clip
video-editor-clip-in-point = Trimmed the start of a clip
video-editor-clip-out-point = Trimmed the end of a clip
video-editor-clip-opacity = Changed a clip's opacity
video-editor-clip-volume = Changed a clip's volume
video-editor-clip-transition = Changed a clip's transition
video-editor-clip-palette = Changed a clip's colour
video-editor-clip-curves = Adjusted a clip's curves
video-editor-clip-fade-in = Changed a clip's fade in
video-editor-clip-fade-out = Changed a clip's fade out
video-editor-clip-transition-duration = Changed the length of a clip's transition
video-editor-clip-anim = Changed a clip's keyframes
video-editor-clip-link = Changed what a clip is linked to
video-editor-clip-sound-split = { $new ->
    [true] Split a clip's sound from its picture
   *[false] Joined a clip's sound to its picture
}
video-editor-clip-link-follows = { $new ->
    [true] Made a linked clip follow its partner
   *[false] Let a linked clip move on its own
}
video-editor-filters-brightness = Changed a clip's brightness
video-editor-filters-contrast = Changed a clip's contrast
video-editor-filters-saturation = Changed a clip's saturation
video-editor-clip-effect-kind = Changed a clip's effect
video-editor-clip-effect-amount = Changed the strength of a clip effect
video-editor-transform-scale-x = Scaled a clip horizontally
video-editor-transform-scale-y = Scaled a clip vertically
video-editor-transform-pos-x = Moved a clip's picture horizontally
video-editor-transform-pos-y = Moved a clip's picture vertically
video-editor-transform-rotation = Rotated a clip
video-editor-transform-anchor-x = Moved a clip's anchor horizontally
video-editor-transform-anchor-y = Moved a clip's anchor vertically
video-editor-transform-crop-left = Cropped a clip's left edge
video-editor-transform-crop-right = Cropped a clip's right edge
video-editor-transform-crop-top = Cropped a clip's top edge
video-editor-transform-crop-bottom = Cropped a clip's bottom edge
video-editor-speed-rate = Set a clip's speed to { $new }×
video-editor-speed-reverse = { $new ->
    [true] Reversed a clip
   *[false] Played a clip forwards
}
video-editor-chroma-key-enabled = { $new ->
    [true] Turned on a clip's chroma key
   *[false] Turned off a clip's chroma key
}
video-editor-chroma-key-color = Picked a clip's key colour
video-editor-chroma-key-tolerance = Changed a clip's key tolerance
video-editor-chroma-key-softness = Changed a clip's key softness
video-editor-chroma-key-spill = Changed a clip's spill suppression
video-editor-mask-shape = Changed the shape of a clip's mask
video-editor-mask-x = Moved a clip's mask horizontally
video-editor-mask-y = Moved a clip's mask vertically
video-editor-mask-w = Resized a clip's mask horizontally
video-editor-mask-h = Resized a clip's mask vertically
video-editor-mask-rotation = Rotated a clip's mask
video-editor-mask-feather = Feathered a clip's mask
video-editor-mask-invert = { $new ->
    [true] Inverted a clip's mask
   *[false] Restored a clip's mask
}

video-editor-marker-t = Moved the marker { $item }
video-editor-marker-name = Renamed the marker { $old } to { $new }
video-editor-marker-palette = Changed the colour of the marker { $item }
video-editor-marker-note = Edited the note of the marker { $item }

video-editor-set-frame-size = Set the frame size to { $width } × { $height }
video-editor-set-frame-rate = Set the frame rate to { $fps } fps
video-editor-set-render-range = Set the render range
video-editor-mark-in = Marked in
video-editor-mark-out = Marked out
video-editor-clear-marks = Cleared the in and out marks
video-editor-add-title = Added the title { $text }
video-editor-set-title = Restyled a title
video-editor-add-clip = Placed a clip on the timeline
video-editor-move-clip = Moved a clip
video-editor-set-clip = Changed a clip's fades and levels
video-editor-remove-clip = { $ripple ->
    [true] Removed a clip and closed the gap
   *[other] Removed a clip
}
video-editor-add-marker = Added a marker
video-editor-remove-marker = Removed a marker
video-editor-add-track = Added a { $kind ->
    [Audio] sound
   *[other] picture
} track
video-editor-reset-mix = Reset a track's level and pan
video-editor-trim-lanes = Lowered the level of the tracks
video-editor-delete-track = Deleted a track
video-editor-place-clip = Placed a clip on the timeline
video-editor-delete-media = Deleted a source
video-editor-use-as-project-format = Matched the project format to a source
video-editor-delete = Deleted a clip
video-editor-ripple-delete = Deleted a clip and closed the gap
video-editor-split-clip = Split a clip
video-editor-trim-in-clip = Trimmed the start of a clip
video-editor-trim-out-clip = Trimmed the end of a clip
video-editor-set-speed = Set a clip's speed to { $rate }×
video-editor-toggle-reverse = Reversed a clip
video-editor-reset-speed = Reset a clip's speed
video-editor-set-param = Set a keyframe
video-editor-add-effect = Added an effect to a clip
video-editor-remove-effect = Removed an effect from a clip
video-editor-move-effect = { $up ->
    [true] Moved a clip effect up
   *[false] Moved a clip effect down
}
video-editor-set-effect-amount = Changed the strength of a clip effect
video-editor-clear-effects = Removed every effect from a clip
video-editor-unlink = Unlinked a clip
video-editor-link-audio = Linked a clip's sound
video-editor-link-with = Linked two clips

## Accessibility
video-editor-a11y-resize = Resize the preview and the timeline
video-editor-a11y-window = Video editor
video-editor-a11y-bin-empty = No media yet. Press Ctrl+Shift+O to import, or drop files anywhere in the window.
video-editor-a11y-timeline-empty = Timeline is empty. Add media from the bin to start editing.
video-editor-a11y-move-playhead = Activate to move the playhead here
video-editor-a11y-export-progress = Export progress
video-editor-a11y-export-finished = Export finished
video-editor-a11y-export-failed = Export failed
video-editor-a11y-export-output = Output
video-editor-a11y-cancel-export = Cancel export
video-editor-a11y-start-export = Start export
video-editor-a11y-close-export = Close export
video-editor-a11y-export-details = Export failure details
video-editor-a11y-export-blocked = Why export cannot run
video-editor-a11y-marker-edit-note = Edit the note on { $marker }
video-editor-a11y-marker-rename = Rename { $marker }
video-editor-a11y-marker-delete = Delete { $marker }
video-editor-a11y-close-shortcuts = Close shortcuts
video-editor-a11y-chroma-key = { $on ->
    [true] Switch chroma key off
   *[false] Switch chroma key on
}
video-editor-a11y-mask-shape = Mask shape { $shape }
video-editor-a11y-match-media = Match media

## The shortcut sheet
video-editor-shortcuts-title = Keyboard shortcuts
video-editor-shortcut-project = Project
video-editor-shortcut-playback = Playback
video-editor-shortcut-editing = Editing
video-editor-shortcut-clipboard = Clipboard
video-editor-shortcut-view = View
video-editor-shortcut-open-project = Open a project file
video-editor-shortcut-export-project = Export a project file
video-editor-shortcut-import-media = Import media
video-editor-shortcut-export-video = Export video
video-editor-shortcut-play-pause = Play or pause
video-editor-shortcut-shuttle = Shuttle back, stop, forward
video-editor-shortcut-jump-start-end = Jump to start or end
video-editor-shortcut-marker-step = Previous or next marker
video-editor-shortcut-clipped = Go to where the mix clipped
video-editor-shortcut-timecode = Go to timecode
video-editor-shortcut-trim = Trim the clip's start or end
video-editor-shortcut-mark-range = Mark the render range
video-editor-shortcut-clear-range = Clear the render range
video-editor-shortcut-split = Split at the playhead
video-editor-shortcut-add-marker = Add a marker
video-editor-shortcut-add-title = Add a title
video-editor-shortcut-delete-clip = Delete the clip
video-editor-shortcut-ripple-delete = Delete and close the gap
video-editor-shortcut-nudge = Nudge, or step the playhead
video-editor-shortcut-cut-copy-paste = Cut, copy, paste
video-editor-shortcut-duplicate = Duplicate the selection
video-editor-shortcut-select-all = Select all clips
video-editor-shortcut-undo = Undo
video-editor-shortcut-redo = Redo
video-editor-shortcut-pane = Next or previous pane
video-editor-shortcut-scroll-timeline = Scroll the timeline
video-editor-shortcut-zoom-fit = Zoom to fit
video-editor-shortcut-zoom = Zoom in or out
video-editor-shortcut-bin = Toggle the media bin
video-editor-shortcut-inspector = Toggle the inspector
video-editor-shortcut-sheet = This sheet
video-editor-shortcut-close = Esc or ? to close

video-editor-a11y-mix-clipped = Go to the next place the mix clipped
video-editor-a11y-mix-level = Mix level

## Window and accessibility copy
video-editor-a11y-timecode = Timecode

## What the window shows
video-editor-drop-import = Drop to import
video-editor-scroll-more-shortcuts = Scroll for more shortcuts
video-editor-scroll-earlier-shortcuts = Scroll up for earlier shortcuts
video-editor-go-to-timecode = Go to timecode
video-editor-finishing-file = Finishing the file
video-editor-rendering = Rendering, { $percent }%
video-editor-export-done = Done: { $path }
video-editor-marker = Marker
video-editor-pause = Pause
video-editor-transition-named = Transition { $transition }
video-editor-label-colour = Label colour { $slot }
video-editor-link-audio-video = Link audio and video
video-editor-reset-gain-pan = Reset gain and pan
video-editor-reset-track = Reset { $track } to 0.0 dB, centre
video-editor-media = Media
video-editor-no-media = No media yet
video-editor-remove-by-dragging = Drag out of the box to remove
video-editor-drag-to-shape = Curve points
video-editor-frame-rate = Frame rate
video-editor-match = Match { $effect }
video-editor-nothing-marked = Nothing marked yet.
video-editor-transition = Transition
video-editor-label = Label
video-editor-picture-in-picture = Picture in picture
video-editor-side-by-side = Side by side
video-editor-crop = Crop
video-editor-anchor = Anchor
video-editor-lane-clip-only = Lane { $lane }, this clip only
video-editor-lane-one-other = Lane { $lane }, with 1 other clip
video-editor-lane-other-clips = Lane { $lane }, with { $count } other clips
video-editor-key-colour = Key colour
video-editor-words = Words
video-editor-accept-cancel-keys = Ctrl+Enter to accept, Esc to cancel
video-editor-type-title = Title text
video-editor-font = Font
video-editor-colour = Colour
video-editor-preview-too-small = Not enough room for the preview
video-editor-preview-empty = No clip at playhead
video-editor-preview-unavailable = This device can’t show the preview
video-editor-inspector-empty = No clip selected
video-editor-no-audio = No audio
video-editor-mix = Mix
video-editor-no-preview = No preview
video-editor-unlink-audio-video = Unlink audio and video
video-editor-split-off-audio = Split off audio
video-editor-link-state-linked = Linked audio and video
video-editor-link-state-unlinked = Not linked
video-editor-link-state-no-audio = No linked audio clip
video-clip-missing-media = Missing media

## The export dialog
video-editor-export-title = Export video

## The export dialog's buttons
video-editor-start-export = Export
video-editor-close-export = Close
video-editor-cancel-export = Cancel

## The window bar's slots
video-editor-hide-inspector = Hide inspector
video-editor-show-inspector = Show inspector
video-editor-open-project-file = Open project file…
video-editor-export-project-file = Export project file…
video-editor-choose-media = Choose media…

## A project nobody has named yet
video-editor-untitled = Untitled

## The bin, the inspector and the timeline's own controls
video-editor-title-button = Title
video-editor-new-title = New title
video-editor-import-media = Import media
video-editor-fill-frame = Fill frame
video-editor-reset-curve = Reset { $channel }
video-editor-sample-centre = Sample centre
video-editor-no-frame-to-sample = No frame to sample
video-editor-fit-to-picture = Fit to picture
video-editor-title-colour = Title colour
video-editor-add-effect-button = Add effect
video-editor-split = Split
video-editor-delete-button = Delete
video-editor-video-track = Video track
video-editor-audio-track = Audio track

## An empty timeline, and the marker list
video-editor-timeline-empty-title = Nothing on the timeline
video-editor-timeline-import-media = Import media, then drag it here.
video-editor-timeline-drag-clip = Drag a clip from the media bin onto a track.
video-editor-timeline-import = Import media
video-editor-timeline-show-media = Show media
video-editor-marker-add = Add marker at playhead
video-editor-go-to-clip = Go to clip

## Actions

video-editor-mark-in-button = Mark in
video-editor-mark-out-button = Mark out
video-editor-effect-reveal-track = Reveal track
video-editor-effect-reveal-clip = Reveal clip
video-editor-effect-reveal-effect = Reveal effect
video-editor-effect-reveal-marker = Reveal marker
video-editor-effect-reveal-media = Reveal media
video-editor-effect-open-project-file = Open project file
video-editor-effect-export-project-file = Export project file
video-editor-effect-import-content = Import content
video-editor-effect-export-timeline-content = Export timeline
video-editor-effect-show-export = Show export settings
video-editor-effect-toggle-media = Show or hide media
video-editor-effect-toggle-inspector = Show or hide the inspector
video-editor-effect-split-selection = Split at playhead
video-editor-effect-render-timeline = Render video
video-editor-effect-export-video-content = Export video content
video-editor-effect-export-video-status = Read export status
video-editor-effect-cancel-video-export = Cancel video export
video-editor-effect-view-frame-content = Read frame content
video-editor-project = Project
video-editor-project-set-frame-size = Set frame size
video-editor-project-set-frame-rate = Set frame rate
video-editor-project-set-render-range = Set render range
video-editor-project-clear-marks = Clear marks
video-editor-project-add-title = Add title
video-editor-project-set-title = Restyle title
video-editor-project-add-clip = Add clip
video-editor-project-move-clip = Move clip
video-editor-project-set-clip = Set clip properties
video-editor-project-remove-clip = Remove clip
video-editor-project-add-marker = Add marker
video-editor-project-place-two-angles = Place two angles
video-editor-project-unplace-two-angles = Unplace two angles
video-editor-project-remove-marker = Remove marker
video-editor-project-remove-marker-here = Remove marker “{ $name }”
video-editor-project-add-track = Add track
video-editor-project-reset-mix = Reset track mix
video-editor-project-trim-lanes = Trim lanes
video-editor-project-delete-track = Delete track
video-editor-project-place-clip = Place clip
video-editor-project-delete-media = Delete media
video-editor-project-use-as-project-format = Match project format
video-editor-project-delete = Delete clip
video-editor-project-ripple-delete = Ripple delete
video-editor-project-split-clip = Split clip
video-editor-project-trim-in-clip = Trim clip start
video-editor-project-trim-out-clip = Trim clip end
video-editor-project-set-speed = Set clip speed
video-editor-project-toggle-reverse = Reverse clip
video-editor-project-reset-speed = Reset clip speed
video-editor-project-set-param = Set a value at a time
video-editor-project-remove-effect = Remove effect
video-editor-project-move-effect = Move effect
video-editor-project-set-effect-amount = Set effect strength
video-editor-project-clear-effects = Clear effects
video-editor-project-unlink = Unlink clip
video-editor-project-link-audio = Separate audio
video-editor-project-link-with = Link clips

## What the app is called in a window and in the launcher
video-editor-app-title = Video Editor
## Actions bound to what the person is looking at

video-editor-project-mark-in-here = Mark in at playhead
video-editor-project-mark-out-here = Mark out at playhead
video-editor-project-split-here = Split clip at playhead
video-editor-project-trim-in-here = Trim clip start to playhead
video-editor-project-trim-out-here = Trim clip end to playhead
video-editor-project-place-two-angles-here = Place side by side
video-editor-project-trim-mix = Trim the mix to fit

## What the editor's panes are called
video-editor-pane-bin = Media bin
video-editor-pane-transport = Playback
video-editor-pane-timeline = Timeline
video-editor-pane-inspector = Inspector

## The inspector's sections, and what a folded one reports
video-editor-section-curves = Curves
video-editor-section-transform = Transform
video-editor-section-speed = Speed
video-editor-section-mix = Mix
video-editor-section-key = Chroma key
video-editor-section-mask = Mask
video-editor-section-title = Title
video-editor-section-effects = Effects
video-editor-section-markers = Markers
video-editor-section-project = Project
video-editor-summary-cropped = cropped
video-editor-summary-animated = animated
video-editor-summary-moved = moved
video-editor-summary-clipping = { $level }, clipping
video-editor-summary-bypassed = { $channels }, bypassed
video-editor-summary-curve-amount = { $channels } at { $percent }%
video-editor-summary-inverted = { $shape }, inverted
video-editor-summary-key-nothing = on, keys nothing
video-editor-summary-keyed = keyed { $percent }%
video-editor-summary-title-untitled = no words yet
video-editor-summary-effects = { $count ->
    [one] { $count } effect
   *[other] { $count } effects
}
video-editor-summary-markers = { $count ->
    [one] { $count } marker
   *[other] { $count } markers
}

## What the inspector's controls are called
video-editor-field-opacity = Opacity
video-editor-field-volume = Volume
video-editor-field-brightness = Brightness
video-editor-field-contrast = Contrast
video-editor-field-saturation = Saturation
video-editor-field-fade-in = Fade in
video-editor-field-fade-out = Fade out
video-editor-field-transition-length = Transition length
video-editor-field-scale-x = Scale X
video-editor-field-scale-y = Scale Y
video-editor-field-position-x = Position X
video-editor-field-position-y = Position Y
video-editor-field-rotation = Rotation
video-editor-field-crop-left = Left
video-editor-field-crop-right = Right
video-editor-field-crop-top = Top
video-editor-field-crop-bottom = Bottom
video-editor-field-key-tolerance = Tolerance
video-editor-field-key-softness = Softness
video-editor-field-key-spill = Spill removal
video-editor-field-mask-x = Centre X
video-editor-field-mask-y = Centre Y
video-editor-field-mask-width = Width
video-editor-field-mask-height = Height
video-editor-field-mask-rotation = Rotation
video-editor-field-mask-feather = Feather
video-editor-field-speed = Rate
video-editor-field-title-size = Size
video-editor-field-title-backdrop = Backdrop
video-editor-field-curve-amount = Amount
video-editor-field-track-gain = Gain
video-editor-field-track-pan = Pan

## The five transport buttons
video-editor-transport-play = Play
video-editor-transport-step-back = Step back one frame
video-editor-transport-step-forward = Step forward one frame
video-editor-transport-jump-start = Jump to start
video-editor-transport-jump-end = Jump to end

## Why a source's bytes cannot be used, on the bin row, the clip and the
## preview alike
video-editor-media-not-local = Not on this device
video-editor-media-not-local-short = Not on device
video-editor-media-unreadable = Unreadable media
video-editor-media-unreadable-short = Unreadable

## Effects, mask shapes, alignment, media kinds and transitions
video-editor-effect-grayscale = Grayscale
video-editor-effect-sepia = Sepia
video-editor-effect-invert = Invert
video-editor-effect-warm = Warm
video-editor-effect-cool = Cool
video-editor-effect-vignette = Vignette
video-editor-mask-off = Off
video-editor-mask-rect = Rect
video-editor-mask-ellipse = Ellipse
video-editor-align-left = Left
video-editor-align-centre = Centre
video-editor-align-right = Right
video-editor-kind-video = Video
video-editor-kind-audio = Audio
video-editor-kind-image = Image
video-editor-kind-title = Title
video-editor-transition-cut = Cut
video-editor-transition-dissolve = Dissolve
video-editor-transition-fade = Fade
video-editor-transition-dip = Dip
video-editor-transition-wipe = Wipe
video-editor-interp-linear = Linear
video-editor-interp-hold = Hold
video-editor-interp-ease = Ease
video-editor-speed-shown = { $rate }×
video-editor-speed-shown-reverse = { $rate }× reverse

## The project's own format
video-editor-format-set-by-clip = Set by the first clip
video-editor-format-shown = { $width }×{ $height }, { $fps } fps
video-editor-format-size = { $width }×{ $height }

## The export sheet
video-editor-export-resolution = Resolution
video-editor-export-format = Format
video-editor-fps-shown = { $rate } fps

## What a run of the inspector is called, and the buttons its rows carry
video-editor-its-track = its track
video-editor-selection-count = { $count ->
    [one] { $count } clip selected
   *[other] { $count } clips selected
}
video-editor-selection-count-locked = { $count ->
    [one] { $count } clip selected, some locked
   *[other] { $count } clips selected, some locked
}
video-editor-selection-on-track = On { $track }
video-editor-selection-on-track-locked = On { $track }, locked
video-editor-match-media = Match media
video-editor-marker-edit-note = Edit note
video-editor-marker-rename = Rename
video-editor-marker-delete = Delete
video-editor-play-backwards = Play backwards
video-editor-key-a-colour = Key a colour out
video-editor-invert-button = Invert
video-editor-bold = Bold
video-editor-italic = Italic
video-editor-effect-move-up = Move up
video-editor-effect-move-down = Move down
video-editor-effect-remove = Remove effect

## The keyboard's, the menus' and the timeline's words
video-editor-key-remove = Remove key here
video-editor-key-add = Add key here
video-editor-key-previous = Previous key
video-editor-key-next = Next key
video-editor-key-remove-all = Remove all keys
video-editor-menu-add-to-timeline = Add to timeline
video-editor-menu-rename = Rename
video-editor-menu-delete = Delete
video-editor-menu-mute = Mute track
video-editor-menu-unmute = Unmute track
video-editor-menu-solo = Solo track
video-editor-menu-unsolo = Unsolo track
video-editor-menu-lock = Lock track
video-editor-menu-unlock = Unlock track
video-editor-menu-add-video-track = Add video track
video-editor-menu-add-audio-track = Add audio track
video-editor-menu-delete-track = Delete track
video-editor-menu-cut = Cut
video-editor-menu-copy = Copy
video-editor-menu-duplicate = Duplicate
video-editor-menu-split = Split at playhead
video-editor-menu-ripple-delete = Ripple delete

## What a reader is told about a lane, a clip, a source and the selection
video-editor-a11y-clip-value = { $start } to { $end }, { $length } long, on { $track }
video-editor-a11y-bin-downloading = { $kind }, { $length }, not downloaded yet
video-editor-a11y-bin-kind-length = { $kind }, { $length }
video-editor-a11y-track-toggle = { $toggle } { $track }
video-editor-toggle-mute = Mute
video-editor-toggle-solo = Solo
video-editor-toggle-lock = Lock
video-editor-a11y-track-value = { $kind } track, { $count } clip{ $plural }, gain { $gain }, pan { $pan }
video-editor-a11y-track-value-clipping = { $kind } track, { $count } clip{ $plural }, gain { $gain }, pan { $pan }, clipping
video-editor-a11y-track-kind-video = video
video-editor-a11y-track-kind-audio = audio
video-editor-a11y-timeline-value = playhead at { $playhead }, { $count } of { $total } clips selected, timeline view starts at { $view }
video-editor-a11y-geometry = scale { $scale }×, position { $x }, { $y }, rotation { $rotation }°
video-editor-a11y-animated = { $geometry }, animated
video-editor-a11y-no-lane = no lane
video-editor-a11y-key-nothing = on, but the tolerance keys nothing
video-editor-a11y-key-values = keying { $tolerance }% tolerance, { $softness }% softness, { $spill }% spill removal
video-editor-a11y-mask-values = { $shape } { $width }% by { $height }%, { $side }
video-editor-a11y-mask-inside = keeping the inside
video-editor-a11y-mask-outside = keeping the outside
video-editor-a11y-title-properties = title properties
video-editor-a11y-export-either = { $first } or { $last }
video-editor-summary-off = Off
video-editor-summary-unchanged = unchanged
video-editor-summary-collapsed = collapsed
video-editor-a11y-preset-size = { $name }, { $width } by { $height }
video-editor-bin-size = { $height }p
video-editor-bin-size-unknown = Size unknown
video-editor-bin-length-unknown = Length unknown

## Why an import was refused, and what the refused file really is
video-editor-probe-unreadable = could not be read ({ $error })
video-editor-probe-empty = is empty
video-editor-probe-not-media = is not a media file, it is { $detected }
video-editor-probe-unrecognised = is not in a media format this editor recognises
video-editor-file-pdf = a PDF document
video-editor-file-zip = a ZIP archive
video-editor-file-elf = an ELF executable
video-editor-file-windows = a Windows executable
video-editor-file-rar = a RAR archive
video-editor-file-gzip = a gzip archive
video-editor-file-bzip2 = a bzip2 archive
video-editor-file-xz = an xz archive
video-editor-file-7z = a 7-Zip archive
video-editor-file-sqlite = a SQLite database
video-editor-file-rtf = an RTF document
video-editor-file-office = an Office document
video-editor-file-binary = a compiled binary
video-editor-file-pem = a PEM key or certificate
video-editor-file-html = an HTML page
video-editor-file-xml = an XML document
video-editor-file-svg = an SVG image
video-editor-file-playlist = a playlist, not the media itself
video-editor-file-json = a JSON document
video-editor-file-script = a script
video-editor-file-markup = a markup document
video-editor-file-text = a text file

## What an import batch reports
video-editor-import-counted = { $count ->
    [one] { $count } clip imported
   *[other] { $count } clips imported
}
video-editor-import-none = No media could be imported
video-editor-import-partial = Imported { $count } of { $total }
video-editor-import-refused = { $head }: { $why }
video-editor-import-more = { $why } and { $hidden } more

## Opening and writing project files, and starting a render
video-editor-opened = Opened { $name }
video-editor-opened-partly = Opened { $name }. { $state }: { $sources }
video-editor-exported-project = Exported the project file
video-editor-exported-name = Exported { $name }
video-editor-export-failed = Could not export: { $reason }
video-editor-save-where = Choose where to save…
video-editor-export-saving-to = { $reason }. Saving to { $path }
video-editor-export-missing-sources = This cut refers to { $sources }, which this device does not have

## What a search result says under its name
video-editor-search-track = { $kind } track
video-editor-search-clip = In { $track } at { $time }
video-editor-search-effect = { $effect } effect
video-editor-search-effect-where = In { $media } on { $track } at { $strength }% strength
video-editor-search-in-bin = { $kind } in the bin
video-editor-search-used = { $kind } used { $count ->
    [one] time
   *[other] times
}
video-editor-search-marker = At { $time }: { $note }
video-editor-pan-centre = Centre
video-editor-undo = Undo
video-editor-redo = Redo
video-editor-nothing-to-undo = Nothing to undo
video-editor-nothing-to-redo = Nothing to redo
video-editor-undo-command = { $verb }: { $summary }
