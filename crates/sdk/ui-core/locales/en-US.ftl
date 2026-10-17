counter = Value: { $count }
tab-modified = Unsaved changes
tab-search-in = Search in { $title }
tab-held-by = { $name }’s window
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
host-search = Search…
host-search-placeholder = Search
dropdown-search-placeholder = Search choices
dropdown-search-description = Type to filter choices. Use the arrow keys to choose and Enter to select.
host-undo = Undo
host-undo-named = Undo { $change }
host-redo = Redo
host-redo-named = Redo { $change }
host-history-show = Show History
host-history-hide = Hide History
host-history-compare = Compare Selection
host-history-recent = { $author } · { $change } — { $when }
host-history-unavailable = There is no document to show History for.
host-history-compare-unavailable = Select two or more changes to compare them.
host-menu-file = File
host-menu-edit = Edit
host-menu-view = View
host-menu-history = History
host-menu-window = Window
host-menu-help = Help
host-menu-about = About { $app }
host-menu-services = Services
host-menu-hide = Hide { $app }
host-menu-hide-others = Hide Others
host-menu-show-all = Show All
host-menu-quit = Quit { $app }
host-menu-close = Close Window
host-menu-minimize = Minimize
host-menu-zoom = Zoom
host-menu-front = Bring All to Front
host-window-minimize = Minimize
host-window-maximize = Maximize
host-window-close = Close
history-genesis = Created
history-edited-field = { $more ->
    [0] Edited { $path }
   *[other] Edited { $path } and { $more } more
}
history-removed-field = { $more ->
    [0] Removed { $path }
   *[other] Removed { $path } and { $more } more
}
history-edited-fields = { $count ->
    [one] Edited 1 field
   *[other] Edited { $count } fields
}
history-removed-fields = { $count ->
    [one] Removed 1 field
   *[other] Removed { $count } fields
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

# The History host's own controls and status, beside the live app.
history-mode-title = History
history-mode-unavailable = History unavailable
history-mode-content-unavailable = Historical content unavailable
history-mode-back-to-activity = Back to activity
history-mode-back-to-live = Back to live
history-mode-earlier = Show earlier changes
history-mode-before = Before
history-mode-after = After
history-mode-showing-before = Historical: Before
history-mode-showing-after = Historical: After
history-mode-restore = Restore…
history-mode-live-updates = Live content has updates
history-mode-page = Page
history-mode-previous-page = Previous page
history-mode-next-page = Next page
history-mode-pending = “{ $action }” waits until you leave History
history-mode-pending-unnamed = An action waits until you leave History
history-mode-pending-held = “{ $action }” was held while History was open
history-mode-pending-held-unnamed = An action was held while History was open
history-mode-continue = Continue
history-mode-confirm = Confirm
history-mode-cancel = Cancel
history-mode-confirmation-unavailable = confirmation unavailable
history-mode-type-to-confirm = Type { $phrase } to confirm
history-mode-owner-gone = History owner is no longer available
history-mode-custody-lost = The original component custody could not be recovered during History
history-mode-control-owner-closed = The original registered control owner is closed
history-mode-loading-media = Loading historical media
history-media-too-many = Historical media exceeds the 4096 asset preview limit
history-media-too-large = Historical media is too large
history-media-over-limit = Historical media exceeds the 128 MiB preview limit
history-media-hash-mismatch = Historical media does not match its recorded content hash

## History groups and catch-up briefs
history-group-changes = { $count ->
    [one] 1 change
   *[other] { $count } changes
}
history-group-edits = { $count ->
    [one] 1 edit
   *[other] { $count } edits
}
history-group-comments = { $count ->
    [one] 1 comment
   *[other] { $count } comments
}
history-group-access-changes = { $count ->
    [one] 1 permission change
   *[other] { $count } permission changes
}
history-group-deletions = { $count ->
    [one] 1 deletion
   *[other] { $count } deletions
}
history-group-over = { $count } over { $span }
history-group-chars = +{ $inserted } -{ $deleted } chars
history-group-from = from “{ $summary }”
history-group-named-by = named by { $name }
history-span-minutes = { $count } min
history-span-hours = { $count } h
history-span-days = { $count } d
history-authors-two = { $first } and { $second }
history-authors-more = { $first }, { $second } + { $rest }
history-outcome-access = Updated workspace access
history-outcome-discussion = Continued the discussion
history-outcome-discussed = Discussed { $resource }
history-outcome-organized-content = Organized shared content
history-outcome-organized = Organized { $resource }
history-outcome-updated-content = Updated shared content
history-outcome-worked-on = Worked on { $resource }
history-outcome-workspace = Updated the workspace
history-chip-all = All
history-chip-edits = Edits
history-chip-edits-short = Edit
history-chip-comments = Comments
history-chip-comments-short = Note
history-chip-access = Access
history-chip-access-short = Access
history-chip-deletions = Deletions
history-chip-deletions-short = Del.
history-op-splice = Text / structure
history-op-edit-meta = Metadata
history-op-move = Move
history-op-comment = Comments
history-op-tombstone = Delete
history-op-restore = Restore
history-op-govern = Access
history-op-compaction = Checkpoints
history-op-redact = Redaction
history-selection-count = { $count ->
    [one] 1 change selected
   *[other] { $count } changes selected
}
history-selection-name = Name these
history-selection-clear = Clear
history-sync-finish-edit = Finish the current edit before receiving remote changes
history-sync-queue-full = Remote document queue is full. Synchronization needs a fresh snapshot after the current edit finishes.
history-sync-owner-changed = Document connection or owner changed while remote updates were pending. Reconnect and synchronize before continuing.
history-sync-offline = Working offline. Changes will sync when you're back.
history-sync-refused = Syncing stopped: this workspace didn't accept a change. Your edits stay on this device.
history-sync-remote-waiting = Remote updates will apply when the current edit finishes.
history-sync-owner-closed = Document owner closed or changed. Reconnect before synchronizing its replacement.

# File history in the History host.
history-files-select-file = Select a file with versioned history
history-files-choose-parent = Choose an actual parent revision for Before
history-files-no-earlier = No earlier version
history-files-checking-compensation = File: { $file }

    Checking a fresh compensation review
history-files-refresh-activity = Refresh activity
history-files-check-restore = Check restore result
history-files-retry-restore = Retry same restore
history-files-redo-restore = Redo file restore…
history-files-undo-restore = Undo file restore…
history-files-current-unavailable = Current file revision pointer unavailable
history-files-current-revision = Current file revision: { $revision }
history-files-live-updates = Live file has updates; selected immutable version remains pinned
history-files-before-parent = Before parent { $revision }
history-files-showing-parent = Before parent: { $revision }
history-files-created = Created
history-files-edited = Edited
history-files-renamed = Renamed
history-files-moved = Moved
history-files-deleted = Deleted
history-files-restored = Restored
history-files-conflict-resolved = Conflict resolved
history-files-unknown-author = Unknown author
history-files-author = Author { $identity }
history-files-loading = Loading file history
history-files-loading-version = Loading immutable version
history-files-checking-permissions = Checking current file permissions and frontier
history-files-earlier-pending = An earlier restore is still pending; check its exact result
history-files-submitting = Submitting checked restore
history-files-version-count = { $count ->
        [one] { $count } immutable version
       *[other] { $count } immutable versions
    }
history-files-binding-changed = File binding changed; reopen History for the current file
history-files-unavailable-host = File history is unavailable on this host
history-files-historical-version = Historical version
history-files-foreign-response = History response belongs to another file
history-files-restore-explainer = Restore creates a new file revision and preserves existing history
history-files-foreign-review = Review belongs to another file or mount
history-files-invalid-result = Invalid backend result; the exact request remains unresolved
history-files-not-visible = This file is no longer visible
history-files-error-stale = File history changed; refresh activity and retry this version
history-files-error-missing-version = This immutable version is unavailable
history-files-error-invalid = Invalid history response
history-files-error-unavailable = File history is temporarily unavailable
history-files-error-too-large = Historical preview exceeds the supported size
history-files-error-no-content = This version has no recorded content reference
history-files-outcome-applied = Restored revision { $revision } (intent { $intent }). Current converged file content is shown separately. Prior reference: observed revision.
history-files-outcome-unchanged = Selected content already matches; no new revision or undo entry
history-files-outcome-pending = Restore is pending. Check result or retry the same intent
history-files-outcome-review = Live file changed. Refresh review; the selected old version remains pinned
history-files-outcome-read-only = This file is read-only; restore and compensation are unavailable
history-files-outcome-pending-writes = Pending file edits must settle before restore; buffered content is preserved
history-files-outcome-unsupported-owner = Unsupported checked content owner: { $mapping }
history-files-outcome-unavailable = Restore acknowledgement is unavailable. Keep this intent and check its result
history-files-outcome-invalid = Invalid checked restore request; no legacy fallback is available

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
widget-breadcrumb = Path

widget-filepicker-cancel = Cancel
widget-filepicker-open = Open
widget-filepicker-save = Save
widget-filepicker-choose-folder = Choose folder
widget-filepicker-replace = Replace
widget-filepicker-replace-title = Replace “{ $name }”?
widget-filepicker-replace-body = A file named “{ $name }” already exists here. Replacing it overwrites its contents.
widget-filepicker-replace-question = A file named “{ $name }” already exists here. Replace it?
widget-filepicker-replace-detail = Replacing it overwrites its contents.
widget-filepicker-up = Up
widget-filepicker-places = Places
widget-filepicker-files = Files
widget-filepicker-folders = Folders
widget-filepicker-file-type = File type
widget-filepicker-all-files = All files
widget-filepicker-name-label = Name
widget-filepicker-name-accessible = Filename
widget-filepicker-name-placeholder = Filename…
widget-filepicker-column-name = Name
widget-filepicker-column-size = Size
widget-filepicker-column-modified = Modified
widget-filepicker-empty = This folder is empty
widget-filepicker-place-home = Home
widget-filepicker-place-documents = Documents
widget-filepicker-place-downloads = Downloads
widget-filepicker-place-pictures = Pictures
widget-filepicker-place-music = Music
widget-filepicker-place-videos = Videos

widget-block-indent = Indent block and children
widget-block-text = Block text
widget-block-outdent = Outdent block and children
widget-maximized-restore = Restore
widget-maximized-fullscreen = Full screen
tree-rename-field = Rename
tree-move-up = Move up
tree-move-down = Move down
tree-move-indent = Move into the item above
tree-move-outdent = Move out of the parent item

# The show/hide control inside every password field.
text-input-show-password = Show password
text-input-hide-password = Hide password

# Authentication surfaces share the system theme and catalog.
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

# Coarse relative times ("3 days ago"), as history rows, lists and cards show them.
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
history-run-step-separator = {", "}
participant-agent = { $name } · Agent
participant-shortcut = { $name } · Shortcut
participant-app = { $name } · App
participant-bot = { $name } · Bot
# Two or more people in a room go by the same name: each is numbered, the
# same numbers for everyone in it.
participant-numbered = { $name } ({ $number })
# An agent, a Shortcut or an app acting for a person.
participant-acting-for = { $name }, for { $person }
interaction-agent = Agent
interaction-collaborator = Collaborator
interaction-action-status = { $action } ({ $phase }) - { $name }
interaction-inspecting = Inspecting
interaction-applying = Applying
interaction-waiting = Waiting
interaction-committed = Committed
interaction-failed = Failed
interaction-cancelled = Cancelled
interaction-progress = { $label } - { $done }/{ $total }
participant-unknown-agent = Unknown agent

# History as published actions, for the palette, scripts, agents and assistive technology.
history-action-resource = Document history
history-action-open = Open history
history-action-list = List changes
history-action-view-at = Preview a version
history-action-compare = Compare changes
history-action-close = Close preview
history-action-restore = Restore a version
history-action-param-resource = The document whose history to use, as History lists it. Defaults to the open document.
history-action-param-cursor = Where the previous page ended.
history-action-param-entry = One change, as History lists it.
history-action-param-entries = Changes, as History lists them.
history-action-param-session = A preview opened by Preview a version.
history-action-param-heads = The live heads the preview reported. Restore is refused if the document changed since.
history-action-denied = You don't have permission for this part of the document's history.
history-action-stale-document = That document is no longer open.
history-action-stale-cursor = The history changed since this page. List it again from the start.
history-action-unknown-entry = That change isn't in this document's history.
history-action-unknown-session = That preview isn't open.
history-action-invalid = That History request isn't valid.

# Command palette chrome: footer key hints, result count and empty states.
palette-name = Command palette
palette-results = Results
palette-placeholder = Search…
palette-hint-navigate = Navigate
palette-hint-select = Select
palette-hint-open = Open
palette-hint-run = Run
palette-hint-close = Close
palette-result-count = { $count ->
    [one] 1 result
   *[other] { $count } results
}
palette-empty = Nothing to show
palette-no-results = No results for “{ $query }”
palette-no-results-hint = Try fewer words, or widen the scope
palette-searching = Searching…

# Calendar section headings in the workspace activity timeline.
day-yesterday = Yesterday
day-today = Today
day-tomorrow = Tomorrow
history-day-this-week = This week
history-day-earlier = Earlier

save-status-saving = Saving…
save-status-stalled = Couldn't save yet. Retrying…
save-status-not-saved = Not saved
save-status-not-saved-detail = Your changes are still here.
save-status-not-saved-named = { $document } wasn't saved
save-status-not-saved-notice = Your changes are still in { $app }.
save-status-saved = Saved
save-status-failed = Save failed
save-status-retry = Try again
save-status-dismiss = Dismiss

# Find bar: the band at the top of a document that finds in it.
find-bar-name = Find
find-bar-placeholder = Find
find-bar-replace-placeholder = Replace with
find-bar-match-case = Match case
find-bar-whole-word = Whole word
find-bar-regex = Regular expression
find-bar-text = Text
find-bar-hex = Hex
find-bar-previous = Previous match
find-bar-next = Next match
find-bar-close = Close find
find-bar-replace = Replace
find-bar-replace-all = Replace all
find-bar-position = { $current } of { $total }
find-bar-no-matches = No matches
widget-block-duplicate = Duplicate block and children

widget-block-duplicate-selection = Duplicate selected or focused blocks

widget-block-move-selection-up = Move selected blocks up
widget-block-move-selection-down = Move selected blocks down

block-editor-replace-placeholder = Replace with
block-editor-replace = Replace

block-editor-replace-all = Replace all
block-editor-replace-all-help = Replace all — Shift+Enter in the replacement field

block-callout-note = Note
block-callout-tip = Tip
block-callout-important = Important
block-callout-warning = Warning
block-callout-caution = Caution

dictation-record-once = Record once
dictation-finish = Finish
dictation-cancel = Cancel

widget-sidebar-resize = Resize sidebar
widget-inspector-resize = Resize inspector

dictation-copy-transcript = Copy transcript
dictation-discard-transcript = Discard transcript

dropdown-no-matches = No matching choices
host-native-history-unobserved = The original field’s editing history has not been confirmed.
host-native-history-unconfirmed = This page’s history edit was not confirmed. Inspect the page before trying again.
host-native-page-history-scope = Undo and redo apply to this page’s editing history. The latest change may be in another field.

# Shared native screen topology canvas
display-arrangement-name = Display arrangement
display-arrangement-description = Drag a display to place it beside another. Each display's position menu does the same from the keyboard.
display-arrangement-empty = No displays to arrange
display-arrangement-item = Display { $number }: { $name }
display-arrangement-item-primary = Display { $number }: { $name }, primary
display-arrangement-item-position = { $width } × { $height } at { $x }, { $y }

device-display-session-unavailable = The Tilo session is unavailable.
device-display-request-pending = A display placement request is already pending.
device-display-session-disconnected = The Tilo session disconnected.

app-activate-window = Show this window
app-activate-window-description = Ask the platform to reveal the existing window, keeping its document owner.

terminal-service-control-unavailable = The shell connection is unavailable.
terminal-service-protocol-unavailable = Update the shell service before opening a session in this window.
terminal-service-invalid-session = The shell service returned an invalid session.
terminal-service-missing-lease = The shell session did not provide its lifetime connection.
terminal-service-missing-terminal = The shell session did not provide a terminal.
terminal-service-invalid-lease = The shell session connection could not be validated.

# Shown instead of an app's view while its record does not read as the app expects.
framework-unreadable = This cannot be opened
display-arrangement-offline = Offline
display-arrangement-excluded = Not in layout
display-arrangement-offline-excluded = Offline · Not in layout
display-arrangement-item-status = { $position }; { $status }
display-arrangement-caption-status = { $status } · { $name }

# Search: starting points in the empty applications palette
search-apps-group-start-here = Start here
search-apps-group-everyday = Everyday
search-apps-group-work = Work
search-apps-group-media = Media
search-apps-group-system-tools = System tools
search-apps-group-more-apps = More apps
search-apps-group-developer-tools = Developer tools

approved-check-reports-disabled = Phase reports unavailable

author-bot-chip = BOT
author-bot-details = { $bot } (bot, added by { $installer })
author-bot-details-as = { $name }, via { $bot } (bot, added by { $installer })
author-bot-linked = Linked to { $person } on Tilo
author-persona-suffix = { $name } · { $service }

# Context menu entry: ask the desktop agent about what is selected here.
ask-agent-about-selection = Ask agent…

widget-screenshot-saved = Saved
widget-screenshot-copied = Copied
widget-screenshot-send-workspace = Save to this workspace's files
widget-screenshot-send-no-device = No other device is online
widget-screenshot-send-device = Send to { $device }
