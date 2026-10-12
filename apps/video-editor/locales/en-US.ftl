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
   *[false] Made { $item } not bold
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
video-editor-clip-effect-kind = Changed a clip effect
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
   *[false] Uninverted a clip's mask
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
video-editor-add-marker = Added a marker
video-editor-remove-marker = Removed a marker
video-editor-add-track = Added a { $kind ->
    [Audio] sound
   *[other] picture
} track
video-editor-reset-mix = Reset a track's level and pan
video-editor-trim-lanes = Levelled the tracks
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
