counter = Value: { $count }
tab-modified = Unsaved changes
tab-search-in = Search in { $title }
tab-held-by = { $name }’s window
tab-controls-desktop = Controls your desktop
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
host-zoom-in = Zoom in
host-zoom-out = Zoom out
host-zoom-reset = Actual size
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
host-window-restore = Restore
host-window-close = Close
window-toolbar-back = Back
window-toolbar-forward = Forward
window-toolbar-go = Go
window-toolbar-more = More actions
window-toolbar-views = View
window-toolbar-suggestions = Suggestions
window-toolbar-edited = Edited
segmented-choose = Choose
window-toolbar-show-sidebar = Show sidebar
window-toolbar-hide-sidebar = Hide sidebar
window-toolbar-show-inspector = Show inspector
window-toolbar-hide-inspector = Hide inspector
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
history-mode-type-to-confirm = Type “{ $phrase }” to confirm
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
history-op-splice = Text and structure
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
history-band-name = Name
history-band-rename = Rename
history-band-undo = Undo
history-band-restore = Restore
history-entry-revert = Revert
history-entry-copy = Copy
history-band-open = still open
history-band-shown = { $shown } of { $total } shown
history-redacted = Removed by { $who } { $when }: { $reason }
history-empty = No activity yet
history-empty-filtered = No activity matches these filters
history-empty-hidden = { $count ->
    [one] One change is loaded, and none is shown
   *[other] { $count } changes are loaded, and none are shown
}
history-empty-hidden-by = { $count ->
    [one] One change is hidden by { $filters }
   *[other] { $count } changes are hidden by { $filters }
}
history-filter-dates = a date range
history-filters-clear = Clear filters
history-entries = History entries
history-author-anyone = Anyone
history-work-task = Task
history-work-agent-run = Agent run
history-work-shortcut = Shortcut
history-selection-clear = Clear
history-sync-finish-edit = Finish the current edit before receiving remote changes
history-sync-queue-full = Remote document queue is full. Synchronization needs a fresh snapshot after the current edit finishes.
history-sync-owner-changed = Document connection or owner changed while remote updates were pending. Reconnect and synchronize before continuing.
history-sync-offline = Working offline. Changes will sync when you're back.
arriving-loading = Loading
arriving-from = From { $source }
arriving-failed = Couldn’t load this
arriving-retry = Try again
content-unavailable = None of the connected devices has it
room-sync-connecting = Connecting to { $workspace }…
room-sync-connecting-unnamed = Connecting…
room-sync-host-needs-update = This workspace’s host needs to update
room-sync-host-needs-update-body = Nothing here syncs until it has. Your changes stay on this device and reach it then.
room-sync-device-needs-update = Tilo needs to update to sync this workspace
room-sync-device-needs-update-body = The update is on its way. Your changes stay on this device until then.
room-sync-shell-gone = This window lost its connection to Tilo, so what this room holds can’t reach it. Open it again.
history-local-action-not-saved = Couldn't save this change to your history.
history-local-action-retry = Retry
history-local-action-unrecorded = Your changes in this window are saved, but your history doesn't record them.

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

block-image-untitled = Picture
block-image-missing = Not on this device yet
notes-image-edit = Edit description
notes-image-done = Done
notes-image-source = Image description and destination
table-edit-source = Edit source
table-edit-visual = Edit table
table-add-row = Add row
table-add-column = Add column

block-editor-empty = Write something…
block-editor-change-type = Change block type
block-editor-copy-code-hint = Copy code

block-editor-code-copied = Copied
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
widget-filepicker-name-invalid = That isn’t a file name.
widget-filepicker-name-no-folder = There’s no folder “{ $folder }”.
widget-filepicker-name-not-allowed = This app can’t save in “{ $folder }”.
widget-filetable-column-name = Name
widget-filetable-column-size = Size
widget-filetable-column-modified = Modified
widget-filetable-column-kind = Kind
widget-filepicker-empty = This folder is empty
widget-filepicker-place-home = Home
widget-filepicker-place-documents = Documents
widget-filepicker-place-downloads = Downloads
widget-filepicker-place-pictures = Pictures
widget-filepicker-place-music = Music
widget-filepicker-place-videos = Videos
widget-filepicker-place-recent = Recent
widget-filepicker-search = Search
widget-filepicker-search-results = Search results
widget-filepicker-location = Location
widget-filepicker-recent-empty = Nothing has been chosen here yet
widget-filepicker-no-matches = No matches
widget-filepicker-wants-open-file = { $app } wants to open a file
widget-filepicker-wants-open-files = { $app } wants to open files
widget-filepicker-wants-save-file = { $app } wants to save a file
widget-filepicker-wants-choose-folder = { $app } wants to use a folder
widget-filepicker-title-open-file = Open a file
widget-filepicker-title-open-files = Open files
widget-filepicker-title-save-file = Save a file
widget-filepicker-title-choose-folder = Choose a folder

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

# The password prompt every secret in Tilo is asked by.
secret-prompt-continue = Continue
secret-prompt-cancel = Cancel
secret-prompt-username = User name
secret-prompt-password = Password
secret-prompt-pin = PIN
secret-prompt-puk = PUK
secret-prompt-new-pin = New PIN
secret-prompt-confirm-pin = Confirm new PIN
secret-prompt-caps-lock = Caps Lock is on
secret-prompt-pin-length = { $min } to { $max } digits
secret-prompt-pin-length-exact = { $count } digits
secret-prompt-attempts-left = { $count ->
    [one] One attempt left
   *[other] { $count } attempts left
}
secret-prompt-pins-differ = The new PINs don’t match
secret-prompt-keypad-delete = Delete
secret-prompt-keypad-enter = Done

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

# Dates, times and numbers (ui::format). Months are 1 = January … 12 =
# December; weekdays are 1 = Monday … 7 = Sunday.
format-month-short = { $month ->
        [1] Jan
        [2] Feb
        [3] Mar
        [4] Apr
        [5] May
        [6] Jun
        [7] Jul
        [8] Aug
        [9] Sep
        [10] Oct
        [11] Nov
       *[12] Dec
    }
format-month-name = { $month ->
        [1] January
        [2] February
        [3] March
        [4] April
        [5] May
        [6] June
        [7] July
        [8] August
        [9] September
        [10] October
        [11] November
       *[12] December
    }
format-weekday-short = { $weekday ->
        [1] Mon
        [2] Tue
        [3] Wed
        [4] Thu
        [5] Fri
        [6] Sat
       *[7] Sun
    }
format-weekday-name = { $weekday ->
        [1] Monday
        [2] Tuesday
        [3] Wednesday
        [4] Thursday
        [5] Friday
        [6] Saturday
       *[7] Sunday
    }
# A day this year in a list: "Jun 30".
format-month-day = { $month } { $day }
# A day in another year in a list: "Jun 30, 2025".
format-month-day-year = { $month } { $day }, { $year }
# The "Long" date style: "30 Jun 2026".
format-date-long = { $day } { $month } { $year }
# The default date style, the language's own convention: "Jun 30, 2026" (de "30.06.2026").
# $day and $month2 / $day2 are the zero-padded numbers, $month the short month name.
format-date-language = { $month } { $day }, { $year }
# A date and a time of day, the date numeric ("2026-06-30 13:42") or worded ("30 Jun 2026, 13:42").
format-date-time-numeric = { $date } { $time }
format-date-time-worded = { $date }, { $time }
format-date-time-language = { $date }, { $time }
# The top of a day: "Tuesday, June 30" and "Tuesday, June 30, 2026".
format-day-heading = { $weekday }, { $month } { $day }
format-day-heading-year = { $weekday }, { $month } { $day }, { $year }
# A day column or compact header: "Tue, Jun 30".
format-day-heading-short = { $weekday }, { $month } { $day }
# A span of days within one month: "Jun 23 – 29".
format-day-range-in-month = { $month } { $start } – { $end }
# Any span between two dates or times: "Jun 29 – Jul 5".
format-range = { $start } – { $end }
# The top of a month grid: "June 2026".
format-month-heading = { $month } { $year }
# The clock the language reads by default: 12 or 24 (here 12-hour).
format-clock-hours = 12
# The halves of a 12-hour clock: "2:05 PM".
format-time-am = AM
format-time-pm = PM
# How long something takes or took: "1 hr 5 min".
format-duration-days = { $count ->
        [one] { $count } day
       *[other] { $count } days
    }
format-duration-hours = { $count } hr
format-duration-minutes = { $count } min
format-duration-seconds = { $count } sec
# How many things a folder, a selection or a result holds: "12 items".
format-items = { $count ->
        [one] { $formatted } item
       *[other] { $formatted } items
    }
format-percent = { $value }%
# Abbreviated large counts: "12.3K", "4.1M", "1.1B".
format-compact-thousands = { $value }K
format-compact-millions = { $value }M
format-compact-billions = { $value }B
# Narrow relative times for tight list columns: "now", "3m", "2h", "4d", "2w", "3mo", "1y".
relative-compact-now = now
relative-compact-minutes = { $count }m
relative-compact-hours = { $count }h
relative-compact-days = { $count }d
relative-compact-weeks = { $count }w
relative-compact-months = { $count }mo
relative-compact-years = { $count }y

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
history-action-name = Name changes
history-action-param-resource = The document whose history to use, as History lists it. Defaults to the open document.
history-action-param-cursor = Where the previous page ended.
history-action-param-entry = One change, as History lists it.
history-action-param-entries = Changes, as History lists them.
history-action-param-session = A preview opened by Preview a version.
history-action-param-heads = The live heads the preview reported. Restore is refused if the document changed since.
history-action-param-first = The first change of the run, as History lists it.
history-action-param-last = The last change of the run, as History lists it.
history-action-param-label = The run's name. Leave it empty to remove the name.
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
palette-empty = Nothing to show
palette-no-results = No results for “{ $query }”
palette-no-results-hint = Check the spelling, or try another word
palette-searching = Searching…

# Calendar section headings in the workspace activity timeline.
day-yesterday = Yesterday
day-today = Today
day-tomorrow = Tomorrow
history-day-this-week = This week
history-day-earlier = Earlier

save-status-stalled = Couldn't save yet. Retrying…
save-status-not-saved = Not saved
save-status-not-saved-named = { $document } wasn't saved
save-status-not-saved-notice = Your changes are still in { $app }.
save-status-saved = Saved
save-status-failed = Save failed
save-status-retry = Try again

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
widget-mention-agent = Agent
widget-mention-shortcut = Shortcut
widget-mention-app = App
widget-mention-bot = Bot
widget-mention-list = People to mention
widget-block-duplicate = Duplicate block and children

widget-block-duplicate-selection = Duplicate selected or focused blocks

widget-block-move-selection-up = Move selected blocks up
widget-block-move-selection-down = Move selected blocks down

block-callout-note = Note
block-callout-tip = Tip
block-callout-important = Important
block-callout-warning = Warning
block-callout-caution = Caution

slash-menu-name = Block types
slash-menu-filter = Filter blocks…
slash-paragraph = Paragraph
slash-heading-1 = Heading 1
slash-heading-2 = Heading 2
slash-heading-3 = Heading 3
slash-bullet = Bullet list
slash-numbered = Numbered list
slash-checklist = Checklist
slash-page = Sub-note
slash-link-page = Link to note
slash-image = Image
slash-quote = Quote
slash-code = Code block
slash-table = Table
slash-tex = TeX formula
slash-typst = Typst formula
slash-divider = Divider
link-popover-name = Link notes
link-popover-filter = Find a note…
filter-menu-no-matches = No matches

dictation-record-once = Record once
dictation-finish = Finish
dictation-cancel = Cancel

widget-sidebar-resize = Resize sidebar
widget-inspector-resize = Resize inspector
widget-inspector = Inspector
widget-sheet-height = Height
widget-sheet-peek = Lowered
widget-sheet-half = Half height
widget-sheet-full = Full height

dictation-copy-transcript = Copy transcript
dictation-discard-transcript = Discard transcript

dropdown-no-matches = No matching choices
host-native-history-unobserved = The original field’s editing history has not been confirmed.
host-native-history-unconfirmed = This page’s history edit was not confirmed. Inspect the page before trying again.
host-native-page-history-scope = Undo and redo apply to this page’s editing history. The latest change may be in another field.

# Shared native screen topology canvas
display-arrangement-name = Display arrangement
display-arrangement-description = Press a display to choose it and drag it to place it beside another. From the keyboard, the display picker chooses one and the arrow keys move it.
display-arrangement-empty = No displays to arrange
display-arrangement-item = Display { $number }: { $name }
display-arrangement-item-primary = Display { $number }: { $name }, main display
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
framework-unreadable = This can't be opened
framework-unreadable-hint = This version of the app can't read what is saved here. Nothing was changed; an update to the app may open it.
# The disclosure under an empty or error state that holds what went wrong technically.
empty-state-details = Details
# Under an app's "open a file" empty state: the file can also be dragged in.
file-open-drop-hint = Or drop a file here
# The empty state while a file is dragged over the window.
file-open-release = Release to open
# Shown instead of a window's document while it cannot be opened.
record-unavailable-title = Can't open this document
record-unavailable-hint = It isn't available on this device right now. It may still be on its way, or it was moved or deleted.
display-arrangement-offline = Offline
display-arrangement-excluded = Not in layout
display-arrangement-offline-excluded = Offline, not in the layout
display-arrangement-item-status = { $position }; { $status }
display-arrangement-caption-status = { $status } · { $name }

# Search: the row that lists the rest of a group the palette cut short
search-show-all = Show all { $count }
search-no-matches = No matches

approved-check-reports-disabled = Phase reports unavailable

author-bot-chip = Bot
author-via-chip = via { $service }
author-bot-details = { $bot } (bot, added by { $installer })
author-bot-details-as = { $name }, via { $bot } (bot, added by { $installer })
author-bot-linked = Linked to { $person } on Tilo
author-persona-suffix = { $name } · { $service }

# Context menu entry: tell an agent to do something with what the menu is about.
tell-agent = Tell an agent…

widget-screenshot-saved = Saved
widget-screenshot-copied = Copied
widget-screenshot-copied-saved = Copied and saved
widget-screenshot-copy = Copy
widget-screenshot-edit = Edit
widget-screenshot-open = Open
widget-screenshot-reveal = Show in Files
widget-screenshot-send = Send…
widget-capture-title = Capture
widget-capture-dismiss = Dismiss
widget-capture-mode-screenshot = Screenshot
widget-capture-mode-recording = Recording
widget-capture-mode-pip = PiP
widget-capture-mode-pip-name = Picture in picture
widget-capture-source-region = Selection
widget-capture-source-window = Window
widget-capture-source-display = Screen
widget-capture-source-strip = Strip
widget-capture-options = Options
widget-capture-delay = Delay
widget-capture-delay-none = No delay
widget-capture-delay-choice = { $seconds } seconds
widget-capture-pointer = Show pointer
widget-capture-system-audio = System audio
widget-capture-microphone = Microphone
widget-capture-keep-recent = Keep last 30 s
widget-capture-combine = Combine selections
widget-capture-layout-auto = Arranged
widget-capture-layout-grid = Grid
widget-capture-layout-stack = Stacked
widget-capture-take = Take screenshot
widget-capture-record = Start recording
widget-capture-stop = Stop recording
widget-capture-create-pip = Create picture in picture
widget-capture-remove-selection = Remove this selection
widget-recording-saved = Recording saved
widget-recording-play = Play
widget-replay-save = Save last 30 s
widget-replay-stop = Stop keeping the last 30 s
widget-screenshot-send-workspace = Save to this workspace's files
widget-screenshot-send-no-device = No other device is online
widget-screenshot-send-device = Send to { $device }
widget-markup-title = Markup
widget-markup-close = Close
widget-markup-undo = Undo
widget-markup-redo = Redo
widget-markup-tools-pen = Pen tools
widget-markup-tools-highlighter = Highlight tools
widget-markup-tools-arrow = Arrow tools
widget-markup-tools-rectangle = Shape tools
widget-markup-tools-redact = Redact tools
widget-markup-tool-pen = Pen
widget-markup-tool-highlighter = Highlight
widget-markup-tool-arrow = Arrow
widget-markup-tool-rectangle = Rectangle
widget-markup-tool-redact = Redact
widget-markup-fit = Fit image
widget-markup-colors = Colours
widget-markup-color-red = Red
widget-markup-color-yellow = Yellow
widget-markup-color-blue = Blue
widget-markup-color-green = Green
widget-markup-color-white = White
widget-markup-open = Open in app
widget-markup-copy = Copy
widget-markup-save = Save copy

## Block editor
block-a11y-formula-source = Source editor
block-a11y-formula-visual = Visual editor
block-a11y-copy-lang = Copy { $lang } code
block-a11y-move-up = Move block up
block-a11y-move-down = Move block down
block-a11y-open-embed = Open embedded note { $title }
block-a11y-embed-description = Live preview of a linked note
block-a11y-table-column = Column { $column }
block-a11y-table-cell = Cell { $row }, { $column }
block-a11y-table-header-row = Header row
block-a11y-table-row = Row { $row }
block-a11y-table = Table
block-a11y-open-link = Open linked note
block-a11y-open-external = Open web link
block-a11y-block = { $kind } block { $position }
block-a11y-level = Level { $level }
block-a11y-document = Drag a block's handle to reorder it. Alt+Up and Alt+Down move the focused block. Tab nests it under the block above and Shift+Tab moves it back out; Escape, then Tab, leaves the editor.
block-formula-empty = Type a formula above to preview it
block-formula-diagnostic = Preview: { $message }
block-formula-source = Source
block-formula-visual = Visual
block-kind-paragraph = Paragraph
block-kind-quote = Quote
block-kind-heading = Heading
block-kind-bullet = Bullet
block-kind-numbered = Numbered item
block-kind-checklist = Checklist item
block-kind-code = Code block
block-kind-table = Table
block-kind-divider = Divider
block-kind-callout = Callout
block-kind-named-callout = { $name } callout
block-embed-missing-title = Note not found
block-embed-missing = The linked note was deleted.
block-kind-code-short = Code
block-table-add-cell = Add cell

window-search-name = Search in this window
window-search-placeholder = Search
window-search-app = Search { $app }
window-search-this-app = Search this app
window-search-everywhere = Search everywhere
window-search-close = Close search
window-search-options = Find options
window-search-replace-options = Find options and replace
history-panel-title = History
history-panel-selected-changes = { $count } changes
history-mode-restore-change = Restore just this change
history-panel-name = Name these changes
history-preview-after = Earlier version, read-only
history-preview-before = Before this change, read-only
history-preview-now = Back to now
workspace-identity-required-title = Choose who you are here
workspace-identity-required-body = This workspace needs to know who’s working in it before { $app } can open.
workspace-identity-required-choose = Choose identity

# The menu a secondary click or a long press opens on editable words.
text-menu-cut = Cut
text-menu-copy = Copy
text-menu-paste = Paste
text-menu-paste-plain = Paste without formatting
text-menu-select-all = Select all
text-menu-bold = Bold
text-menu-italic = Italic
text-menu-underline = Underline
text-menu-strikethrough = Strikethrough
text-menu-bulleted-list = Bulleted list
text-menu-numbered-list = Numbered list

# A reference to something another app keeps, shown as a chip; $label is its name.
reference-open = Open { $label }

# A signal meter's name, by how strong the signal is (a Wi-Fi or mobile network).
signal-meter-unknown = Signal strength unknown
signal-meter-weak = Weak signal
signal-meter-fair = Fair signal
signal-meter-good = Good signal
signal-meter-strong = Strong signal
