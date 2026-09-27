counter = Value: { $count }
btn_1 = Click Me
tab-modified = Unsaved changes
standalone-search-title = Search
standalone-history-title = History
standalone-history-close = Close history
standalone-history-select-hint = Select a change to see details.
standalone-history-select = Select changes
standalone-history-compare = Compare selected changes
standalone-history-entry-ids = Changes
standalone-history-selected-details = Selected changes
standalone-history-restore = Restore this version…
standalone-history-restore-title = Restore this version?
standalone-history-restore-body = The document goes back to how it was after “{ $version }”. This adds a new change: later versions stay in History, and Undo reverses the restore.
standalone-history-restore-confirm = Restore version
standalone-history-restore-cancel = Cancel
standalone-history-restore-action = Restore version
standalone-history-restore-done = Version restored. Undo reverses it.
standalone-history-restore-stale = The document changed while you were reviewing. Select the version again to restore it.
standalone-history-restore-pending = Finish the current edit, then restore the version.
standalone-history-restore-denied = Only editors can restore a version.
standalone-history-restore-unavailable = This version can't be restored here.
standalone-history-preview-resource = Document
history-edited-field = { $more ->
    [0] Edited { $path }
   *[other] Edited { $path } and { $more } more
}
history-removed-field = { $more ->
    [0] Removed { $path }
   *[other] Removed { $path } and { $more } more
}
history-review-title = Restore { $resource }
history-review-confirm = Restore version
history-review-refresh = Refresh review
history-review-cancel = Cancel
history-review-body = Version: { $version }

    Restoring returns the document to exactly this version as one new change. Later changes stay in history, and Undo reverses the restore.
history-review-selected-version = Selected version
history-review-not-content = This activity does not contain restorable document content.
history-review-no-clock = The current time is unavailable. Restore cannot check permission expiry.
history-restore-unsupported = This version cannot be restored safely with this application's content format.
history-preview-description = { $resource } as of this version

music-open-audio-file = Open audio file

close-title = Close document
close-unsaved = Save your changes before closing?
close-save-failed = Could not save changes
close-saving = Waiting for the save to finish…
close-save-cancelled = Saving was cancelled. Your document is still open.
close-save = Save
close-discard = Discard changes
close-retry = Retry
close-keep-editing = Keep Editing
close-broker-disconnected = The save service disconnected. Your document is still open.
terminal-find-scrollback = Find in scrollback

notes-image-edit = Edit image
notes-image-done = Done
notes-image-source = Image description and destination
table-edit-source = Edit source
table-edit-visual = Edit table
table-add-row = Add row
table-add-column = Add column

block-editor-drag-hint = Click for block type · Drag to move · Alt+↑/↓
block-editor-find-placeholder = Find in note
block-editor-find-no-results = No results
block-editor-find-previous = Previous match
block-editor-find-next = Next match
block-editor-find-close = Close find
block-editor-change-type = Change block type
block-editor-copy-code-hint = Copy code

block-editor-code-exit-hint = Ctrl+Enter to leave code
block-editor-code-exit-compact = Ctrl+Enter ↵
block-editor-code-copied = Copied
block-editor-code-keyboard-hint = Ctrl+Enter: write below · Ctrl+Shift+Enter: write above · Tab / Shift+Tab: indent / outdent · Ctrl+Shift+C: copy code
widget-block-indent = Indent block and children
widget-block-text = Block text
widget-block-outdent = Outdent block and children
tree-rename-field = Rename
tree-move-up = Move up
tree-move-down = Move down
tree-move-indent = Move into the item above
tree-move-outdent = Move out of the parent item

# Authentication surfaces share the system theme and catalog.
greeter-welcome-back = Welcome back to Tilo
greeter-sign-in = Sign in to continue
greeter-passcode-intro = Enter your passcode to open your desktop.
lock-welcome-back = Welcome back

# Now playing: Quick Settings and the lock screen.
now-playing-title = Now playing
now-playing-play = Play
now-playing-pause = Pause
now-playing-previous = Previous track
now-playing-next = Next track
now-playing-seek = Playback position
now-playing-seek-help = Use Left and Right Arrow to move five seconds
now-playing-choose-player = Choose player
now-playing-choose-output = Choose where to play
now-playing-output-local = On this computer
now-playing-output-device = One of your devices
now-playing-output-speaker-group = One of your speaker groups
now-playing-route-gone = That destination is no longer available

# The inline permission prompt a sandboxed app raises.
permission-prompt-allow = Allow
permission-prompt-session = This session
permission-prompt-always = Always allow
permission-prompt-deny = Deny
permission-prompt-requested = { $capability } requested by { $app }

# Relative times, as history and activity rows show them.
relative-time-just-now = just now
relative-time-minutes = { $count ->
        [one] { $count } min ago
       *[other] { $count } min ago
    }
relative-time-hours = { $count ->
        [one] { $count } h ago
       *[other] { $count } h ago
    }
relative-time-days = { $count ->
        [one] { $count } d ago
       *[other] { $count } d ago
    }

# Coarse relative times ("3 days ago"), as lists and cards show them.
relative-ago-just-now = just now
relative-ago-minutes = { $count } min ago
relative-ago-hours = { $count } hr ago
relative-ago-yesterday = yesterday
relative-ago-days = { $count ->
        [one] { $count } day ago
       *[other] { $count } days ago
    }
relative-ago-weeks = { $count ->
        [one] { $count } week ago
       *[other] { $count } weeks ago
    }
relative-ago-months = { $count ->
        [one] { $count } month ago
       *[other] { $count } months ago
    }
relative-ago-years = { $count ->
        [one] { $count } year ago
       *[other] { $count } years ago
    }

# A software participant in history, presence and activity rows.
participant-agent = { $name } · Agent
participant-unknown-agent = Unknown agent
