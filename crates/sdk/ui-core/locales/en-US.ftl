counter = Value: { $count }
tab-modified = Unsaved changes
tab-loading = Loading
tab-playing = Playing audio
tab-muted = Muted
tab-close-named = Close { $title }
tab-mute-named = Mute { $title }
tab-search-in = Search in { $title }
tab-held-by = { $name }’s window
tab-opened-by = { $agent } opened this window
tab-opened-by-run = { $run }
standalone-search-title = Search
history-mode-title = History
history-mode-select-hint = Show this change in History
history-mode-entry-ids = Change
history-mode-preview-resource = Document
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
host-history-recent = { $change } by { $author }, { $when }
host-history-unavailable = There is no document to show History for.
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
host-window-snap = Snap back into the strip
host-window-collapse = Collapse
host-window-expand = Expand
host-window-close = Close
window-toolbar-back = Back
window-toolbar-forward = Forward
window-toolbar-new-tab = New tab
window-toolbar-go = Go
window-toolbar-more = More actions
window-toolbar-views = View
window-toolbar-suggestions = Suggestions
window-toolbar-edited = Edited
window-toolbar-view-only = View only
segmented-choose = Choose
window-toolbar-show-sidebar = Show sidebar
window-toolbar-hide-sidebar = Hide sidebar
window-toolbar-show-inspector = Show inspector
window-toolbar-hide-inspector = Hide inspector
# A window shared live into a conversation, from its own menu.
window-share-to-channel = Share to channel…
window-stop-sharing-in = Stop sharing in { $place }
history-loading = Loading History…
lazy-loading = Loading…
history-genesis = Created
history-moved = Moved between workspaces
history-review-title = Restore { $resource }
history-review-confirm = Restore version
history-review-refresh = Refresh review
history-review-cancel = Cancel
history-review-body = Version: { $version }

    Restoring returns the document to exactly this version as one new change. Later changes stay in history, and Undo reverses the restore.
history-review-selected-version = Selected version
history-review-not-content = This activity does not contain restorable document content.
history-review-no-clock = This computer's clock is unavailable, so Restore can't check permissions.
history-restore-unsupported = This app can't restore this version safely.
history-preview-description = { $resource } as of this version

# The History host's own controls and status, beside the live app.
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
history-mode-confirm = Allow
history-mode-cancel = Cancel
history-mode-type-to-confirm = Type “{ $phrase }” to confirm
history-mode-owner-gone = This document is no longer open here
history-mode-custody-lost = History couldn't bring back what this action needed
history-mode-control-owner-closed = The app that asked for this has closed
history-mode-loading-media = Loading historical media
history-media-too-many = This version has too many pictures to show
history-media-too-large = This version's pictures are too large to show
history-media-over-limit = This version's pictures are too large to show
history-media-hash-mismatch = This version's pictures are damaged and can't be shown

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
history-work-agent-run = Run
history-work-shortcut = Shortcut
history-selection-clear = Clear
history-sync-finish-edit = Finish your edit before changes from others arrive
history-sync-queue-full = Too many changes from others are waiting. They arrive once your edit finishes.
history-sync-owner-changed = The connection changed while changes from others were waiting. Reconnect to receive them.
history-sync-offline = Working offline. Changes will sync when you're back.
history-sync-reconnecting = Connecting. Changes will sync once you're connected.
history-sync-join-refused = This workspace didn't let this app join: { $reason }. Your changes stay on this device.
arriving-loading = Loading
arriving-from = From { $source }
arriving-received-of = { $received } of { $total }
arriving-received = { $received } received
arriving-failed = Couldn’t load this
arriving-retry = Try again
content-unavailable = None of the connected devices has it
workspace-sync-connecting = Connecting to { $workspace }…
workspace-sync-connecting-unnamed = Connecting…
workspace-sync-unreachable = Can’t reach { $workspace } right now
workspace-sync-unreachable-unnamed = Can’t reach this workspace’s host right now
workspace-sync-unreachable-body = This is what this device has. Your changes stay here and sync once its host is back.
workspace-sync-host-needs-update = This workspace’s host needs to update
workspace-sync-host-needs-update-body = Nothing here syncs until it has. Your changes stay on this device and reach it then.
workspace-sync-device-needs-update = Tilo needs to update to sync this workspace
workspace-sync-device-needs-update-body = The update is on its way. Your changes stay on this device until then.
workspace-sync-shell-gone = This window lost its connection to Tilo, so what this workspace holds can’t reach it. Open it again.
workspace-sync-not-yet-synced = Not yet synced. Your changes are on this device until { $workspace }’s host has them.
workspace-sync-not-yet-synced-unnamed = Not yet synced. Your changes are on this device until the workspace’s host has them.
workspace-sync-cannot-keep = Can’t sync right now. This device has no space to keep your changes to { $workspace }.
workspace-sync-cannot-keep-unnamed = Can’t sync right now. This device has no space to keep your changes.
history-local-action-not-saved = Couldn't save this change to your history.
history-local-action-retry = Retry
history-local-action-unrecorded = Your changes in this window are saved, but your history doesn't record them.

history-sync-refused = This workspace didn't accept one change. Everything else keeps syncing; the change stays on this device.
history-sync-unreadable = This record's shared copy couldn't be read here. Your own changes stay on this device and keep saving.
history-sync-remote-waiting = Changes from others arrive when your edit finishes.
history-sync-owner-closed = The document's connection closed. Reconnect to receive changes from others.

# File history in the History host.
history-files-select-file = Select a file with versioned history
history-files-choose-parent = Choose which earlier version to compare with
history-files-no-earlier = No earlier version
history-files-checking-compensation = File: { $file }

    Checking the file again
history-files-refresh-activity = Refresh activity
history-files-check-restore = Check restore result
history-files-retry-restore = Retry same restore
history-files-redo-restore = Redo file restore…
history-files-undo-restore = Undo file restore…
history-files-current-unavailable = The current version can't be read
history-files-current-revision = Current version: { $revision }
history-files-live-updates = The file has changed since. The version you picked stays
history-files-before-parent = Compare with version { $revision }
history-files-showing-parent = Comparing with version { $revision }
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
history-files-loading-version = Loading the version
history-files-checking-permissions = Checking permissions…
history-files-earlier-pending = An earlier restore hasn't finished. Check its result
history-files-submitting = Restoring…
history-files-version-count = { $count ->
        [one] { $count } version
       *[other] { $count } versions
    }
history-files-binding-changed = This window now shows another file. Open History again
history-files-unavailable-host = This file has no history here
history-files-historical-version = Earlier version
history-files-foreign-response = That history answer is for another file. Try again
history-files-restore-explainer = Restoring makes a new version; the ones before it stay in history
history-files-foreign-review = That review is for another file. Try again
history-files-invalid-result = Couldn't tell whether the restore ran. Check the file's history
history-files-not-visible = This file is no longer visible
history-files-error-stale = The file's history changed. Refresh, then try this version again
history-files-error-missing-version = This version is no longer available
history-files-error-invalid = Couldn't read this file's history
history-files-error-unavailable = File history is temporarily unavailable
history-files-error-too-large = This version is too large to show
history-files-error-no-content = This version has no content to show
history-files-outcome-applied = Restored version { $revision }. The file shows its current content
history-files-outcome-unchanged = The file already matches this version. Nothing was restored
history-files-outcome-pending = The restore hasn't finished. Check its result, or try again
history-files-outcome-review = The file changed. Refresh the review; the version you picked stays selected
history-files-outcome-read-only = This file is read-only, so it can't be restored
history-files-outcome-pending-writes = Save your edits before restoring. Nothing you typed is lost
history-files-outcome-unsupported-owner = This app can't restore this file's content
history-files-outcome-unavailable = The restore may not have happened. Check the file's history before trying again
history-files-outcome-invalid = That restore didn't run. Try again

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
# The title a row shows for an item nobody has named yet.
list-row-untitled = Untitled
list-row-check = Mark { $title } done
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
tree-rename-field = Rename

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

# Spoken when Ctrl+Super+Enter (or both volume keys held) turns the screen reader on or off.
screen-reader-turned-on = Screen reader on
screen-reader-turned-off = Screen reader off

# Authentication surfaces share the system theme and catalog.

# The names the now-playing row's two choosers are read by.
now-playing-choose-player = Choose player
now-playing-choose-output = Choose where to play

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
relative-ahead-moment = in a moment
relative-ahead-minutes = in { $count } min
relative-ahead-hours = in { $count } hr
relative-ahead-tomorrow = tomorrow
relative-ahead-days = { $count ->
        [one] in { $count } day
       *[other] in { $count } days
    }
relative-ahead-weeks = { $count ->
        [one] in { $count } week
       *[other] in { $count } weeks
    }
relative-ahead-months = { $count ->
        [one] in { $count } month
       *[other] in { $count } months
    }
relative-ahead-years = { $count ->
        [one] in { $count } year
       *[other] in { $count } years
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
# A day this year and a time of day: "Jun 30, 14:05".
format-month-day-time = { $month } { $day }, { $time }
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
# Two already-formatted values in a list: "encrypted, 134 GB free".
format-list-join = { $head }, { $tail }
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
# Two or more people in a workspace go by the same name: each is numbered, the
# same numbers for everyone in it.
participant-numbered = { $name } ({ $number })
interaction-agent = Agent
interaction-collaborator = Collaborator
# What an actor is doing: its name, then the action in the present tense.
interaction-editing-field = editing { $field }
# The text a write put in a note, a page or a field stands there: what the
# person watching sees happen, where the action's own name only says which
# place it acts on.
interaction-added-text = added text
interaction-working = working
interaction-action-doing = { $name }: { $doing }
# The same, with the phase said where only a person can settle it.
interaction-action-status = { $name }: { $doing }. { $phase }
interaction-inspecting = Inspecting
interaction-applying = Applying
interaction-waiting = Waiting
interaction-committed = Committed
interaction-failed = Failed
interaction-cancelled = Cancelled
interaction-progress = { $label } - { $done }/{ $total }

# What one step of work is, as a person reads it beside the actor's face, in
# the people list and on the card drawn where the step happens: the kind of
# work the action's own declaration names, on the record or item it acts in.
# The record is the item's title where the step names one.
step-creating = Creating in { $record }
# A create whose declaration names the collection it adds to says what it makes,
# by the noun the app's own catalogue gives one of them, with its article
# ("Creating an issue in Issues").
step-creating-one = Creating { $noun } in { $record }
step-writing = Writing in { $record }
step-editing = Editing { $record }
step-deleting = Deleting { $record }
step-moving = Moving { $record }
step-reading = Reading { $record }
step-running = Running { $command }
step-running-in = Running in { $record }
step-working = Working in { $record }
step-working-alone = Working

# What a run is at when it is not a step of a record: waiting on its person,
# looking at the apps or at one of them, searching for words.
step-waiting = Waiting
step-looking-at = Looking at { $what }
step-looking-at-apps = Looking at the apps
step-searching = Searching for { $what }
# A peer whose caret a text widget draws in its own text: where they are, said
# to a reader. A selection says that instead.
presence-writing-here = { $name } is writing here
presence-selecting-here = { $name } is selecting here
participant-unknown-agent = Unknown agent


# Command palette chrome: footer key hints, result count and empty states.
palette-name = Command palette
palette-argument-name = Action confirmation
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
# A date fact as a list heading, a row's trailing fact, a chip or a card reads it: a day gone
# by, and a day and the time of it as one fact.
day-overdue = Overdue
day-past = { $overdue } since { $when }
day-at-time = { $day } { $time }
history-day-this-week = This week
history-day-earlier = Earlier

save-status-stalled = Couldn't save yet. Retrying…
save-status-not-saved = Not saved
save-status-not-saved-named = { $document } wasn't saved
save-status-not-saved-notice = Your changes are still in { $app }.
save-status-saved = Saved
save-status-failed = Save failed
save-status-failed-detail = Your latest changes are not stored yet.
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
rich-text-reference-unreadable = Not shared with you
rich-text-edit-not-kept = Your last edit wasn't kept
rich-text-too-long = This paste is too long to keep (limit { $limit }). Paste it in smaller parts.
rich-text-changed-while-typing = This paragraph changed while you were typing. Your edit wasn't kept.
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
slash-embed-page = Embed note
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
host-native-history-unobserved = Undo isn't ready for this page yet.
host-native-history-unconfirmed = The page didn't confirm its undo. Check the page, then try again.
host-native-page-history-scope = Undo and redo act on this page. Your last change may have been in another field.

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
app-activate-window-description = Bring this app's window to the front.

terminal-service-control-unavailable = The terminal service isn't answering. Try again.
terminal-service-protocol-unavailable = This window needs a newer Tilo on this computer. Update, then try again.
terminal-service-invalid-session = The terminal didn't start. Try again.
terminal-service-missing-lease = The terminal didn't start. Try again.
terminal-service-missing-terminal = The terminal didn't start. Try again.
terminal-service-invalid-lease = The terminal's connection couldn't be checked. Try again.

# Shown instead of an app's view while its record does not read as the app expects.
framework-unreadable = This can't be opened
framework-unreadable-hint = This version of the app can't read what is saved here. Nothing was changed; an update to the app may open it.
# The disclosure under an empty or error state that holds what went wrong technically.
empty-state-details = Details
# Under an app's "open a file" empty state: the file can also be dragged in.
file-open-drop-hint = Or drop a file here
# The empty state while a file is dragged over the window.
file-open-release = Release to open
# The page rail's title, above its thumbnails, unless the app names it.
page-sidebar-title = Pages
# Above the documents an open-a-file empty state lists: the ones the
# workspace already holds, whatever made them.
widget-documents-in-workspace = In this workspace
# Shown instead of a window's document while it cannot be opened.
record-unavailable-title = Can't open this document
record-unavailable-hint = It isn't available on this device right now. It may still be on its way, or it was moved or deleted.
display-arrangement-offline = Offline
display-arrangement-excluded = Not in layout
display-arrangement-offline-excluded = Offline, not in the layout
display-arrangement-item-status = { $position }; { $status }
display-arrangement-caption-status = { $status }: { $name }

# Search: the row that lists the rest of a group the palette cut short
search-show-all = Show all { $count }
search-no-matches = No matches
# What a group header and its kind chip say the results are
search-kind-actions = Actions
search-kind-apps = Apps
search-kind-notes = Notes
search-kind-records = Records
search-kind-files = Files
search-kind-folders = Folders
search-kind-creators = Creators
search-kind-videos = Videos
search-kind-playlists = Playlists
search-kind-tabs = Tabs
search-kind-window-groups = Window groups
window-root-name = Tilo application window
# What the palette's empty field says the query covers, not what it is called.
action-palette-query = Search actions and results
search-field-filter = Filter
search-field-placeholder = Search
# What the field itself is called, apart from the placeholder inside it.
search-field-name = Search
search-field-filter-name = Filter
presentation-window-title = Audience presentation

approved-check-reports-disabled = Working

author-bot-chip = Bot
author-via-chip = via { $service }
author-from-service = From { $service }
author-from-service-as = From { $service } as { $handle }
author-bot-details = { $bot } (bot, added by { $installer })
author-bot-details-as = { $name }, via { $bot } (bot, added by { $installer })
author-bot-linked = Linked to { $person } on Tilo
author-persona-suffix = { $name }, via { $service }

# A context menu's query field: typing narrows its entries, and its send row
# carries the typing into the send-to box that opens beside the menu.
menu-query-placeholder = Type to filter
menu-query-name = Filter or send
# The last row of every context menu, which opens the send-to box; once
# something is typed it names the sentence it carries.
menu-send = Send to…
menu-ask-typed = Send: { $text }

# The send-to box: whom a message goes to, what it is about, and its words.
# It stands beside the menu whose send row opened it, and alone for a report.
send-to-name = Send to
# The same box opened to the one place reports go.
send-to-report-name = Report a problem
# The line over a window asked to point at what a report is about.
send-to-point-hint = Click what is not working
send-to-point-escape = Escape leaves
send-to-new-agent = New agent
send-to-recipients-name = Add someone
send-to-search-placeholder = Add someone…
send-to-no-results = Nothing found
send-to-add-subject = Add a subject
send-to-message-name = Message
send-to-message-placeholder = Write a message
send-to-report-placeholder = What went wrong?
send-to-send = Send

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
widget-recording-skipped = { $count ->
    [one] 1 frame was skipped: the disk was too slow
   *[other] { $count } frames were skipped: the disk was too slow
}
widget-recording-incomplete = Recording incomplete: { $reason }
widget-recording-not-saved = Recording not saved: { $reason }
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
block-a11y-open-resource = Open { $label }
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

# A form's date field: what it reads as with no date, its calendar, the button that empties it,
# and the row that picks the time of day the date is due at.
date-field-add = Add date
date-field-calendar = Calendar
date-field-clear = Clear date
date-field-named = { $label }, { $day }
time-field-name = Time of day
time-field-add = Add time
time-field-remove = Remove time

# A form's person field: what it reads as with nobody picked, and the entry that picks you.
person-field-none = No one
person-field-me = Me

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
text-menu-heading-1 = Heading 1
text-menu-heading-2 = Heading 2
text-menu-heading-3 = Heading 3
text-menu-body-text = Body text
text-menu-bulleted-list = Bulleted list
text-menu-numbered-list = Numbered list

# A reference to something another app keeps, shown as a chip; $label is its name.
reference-open = Open { $label }

# What someone else (an agent's run, mostly) changed that the person has not seen yet.
# A field they changed, on hover: $who changed it; $before is what it held.
change-field = Changed by { $who }
change-field-before = Changed by { $who }. Before: { $before }
# The mark on a row or wherever a view places it: who, and the task run it was part of.
change-mark-who = { $who }
# What the mark says when read out or hovered: what they did.
change-mark-created = Made by { $who }
change-mark-changed = Changed by { $who }
change-mark-removed = Removed by { $who }
# A row’s mark, read out and on hover: what they did, in which task run.
change-mark-in-run = { $what } in “{ $run }”
# A window's title, with how many changes there are the person has not seen, and by whom.
change-window = { $count ->
    [one] One change by { $who }
   *[other] { $count } changes by { $who }
}
# A window's mark when the type names what an item of it is ($noun, plural, and $one, its
# singular): everyone who changed what the window shows, and how many items they changed.
change-window-changed = { $who } changed { $count ->
    [one] one { $one }
   *[other] { $count } { $noun }
}
# A window's mark when every change it counted only put items in: what is new, and who put it
# there ("7 new messages from Mia and Leo").
change-window-new = { $count ->
    [one] One new { $one } from { $who }
   *[other] { $count } new { $noun } from { $who }
}
# The same, for a mark that counted a page of what it shows, with more than it says.
change-window-new-more = { $count ->
    [one] At least one new { $one } from { $who }
   *[other] At least { $count } new { $noun } from { $who }
}
# The names a window's mark lists, as a sentence reads them; past three it names the first two
# and counts the rest, as its faces do.
change-mark-names-two = { $first } and { $second }
change-mark-names-three = { $first }, { $second } and { $third }
change-mark-names-more = { $first }, { $second } and { $count } others
# A row's mark, under its title: what they did, in the words history says it with, and how
# many more changes of the row wait behind it.
change-mark-said = { $who } { $said }
change-mark-said-more = { $said } and { $count ->
    [one] one more
   *[other] { $count } more
}
# A window's mark that counted a page of what it shows, with more than it says.
change-window-more = { $who } changed at least { $count ->
    [one] one change
   *[other] { $count } changes
}
# The same, when the type names what an item of it is ($noun, plural, and $one, singular).
change-window-changed-more = { $who } changed at least { $count ->
    [one] one { $one }
   *[other] { $count } { $noun }
}

# One step of an agent's task: the program it ran, what it answered, what it
# changed and what it proposes.
program-step-program = Program the agent ran
program-step-show-all = Show all { $count } lines
program-step-undo = Undo
program-step-approve = Approve
program-step-decline = Decline
program-step-waits = Waits for you to confirm it.
program-step-planned = Nothing changed yet. Once confirmed, this changes:

# A signal meter's name, by how strong the signal is (a Wi-Fi or mobile network).
signal-meter-unknown = Signal strength unknown
signal-meter-weak = Weak signal
signal-meter-fair = Fair signal
signal-meter-good = Good signal
signal-meter-strong = Strong signal

# Reactions: the pills under a message or on a page, one per emoji. $emoji is
# the emoji itself, or a workspace emoji written as :name:.
reaction-strip-add = Add reaction
reaction-strip-react = Add { $emoji } reaction
reaction-strip-unreact = Remove { $emoji } reaction
reaction-strip-count = { $count ->
    [one] One reaction
   *[other] { $count } reactions
}
# Who reacted, in the tooltip of a pill; $emoji is the emoji's name.
reaction-strip-you = You
reaction-strip-reacted-one = { $first } reacted with { $emoji }
reaction-strip-reacted-two = { $first } and { $second } reacted with { $emoji }
reaction-strip-reacted-three = { $first }, { $second } and { $third } reacted with { $emoji }
reaction-strip-reacted-more = { $first }, { $second } and { $others ->
    [one] one other
   *[other] { $others } others
} reacted with { $emoji }

# The emoji picker: the workspace's own emoji, the recently used ones and
# Unicode's groups as tabs.
emoji-chooser-title = Emoji
emoji-chooser-search = Search emoji
emoji-chooser-groups = Emoji categories
emoji-chooser-workspace = Workspace
emoji-chooser-recent = Recently used
emoji-chooser-results = Matching emoji
emoji-chooser-no-results = No emoji found
emoji-chooser-arriving = More emoji are on their way
emoji-chooser-add = Add emoji
emoji-bar = Quick reactions
emoji-more = More emoji
emoji-chooser-add-failed = Couldn't add that picture: { $reason }
emoji-chooser-tones = Skin tones
emoji-chooser-tone-hint = Pick a skin tone
emoji-chooser-toned = { $name }, { $tone }
emoji-chooser-tone-light = light skin tone
emoji-chooser-tone-medium-light = medium-light skin tone
emoji-chooser-tone-medium = medium skin tone
emoji-chooser-tone-medium-dark = medium-dark skin tone
emoji-chooser-tone-dark = dark skin tone

## The workspace's own emoji
workspace-emoji-choose-title = Add an emoji
workspace-emoji-choose-accept = Add
workspace-emoji-image-files = Pictures
# Why a picture chosen in the emoji picker was not added, after
# emoji-chooser-add-failed's colon: short, it shares the picker's last line.
workspace-emoji-loading = the picture tools are still loading
workspace-emoji-not-a-picture = it isn't a PNG, JPEG or WebP
workspace-emoji-too-detailed = it's too detailed for an emoji
workspace-emoji-unavailable = this workspace's emoji aren't available
workspace-emoji-not-allowed = you may not add emoji here
# The conversation substrate: the delivery vocabulary and the attachment
# ladder, shared by every app that draws a conversation.
conversation-delivery-queued = Queued
conversation-delivery-sending = Sending
conversation-delivery-submitted = Submitted
conversation-delivery-sent = Sent
conversation-delivery-delivered = Delivered
conversation-delivery-read = Read
conversation-delivery-failed = Not sent
conversation-delivery-uncertain = Status uncertain
conversation-new-messages = New messages
conversation-day-new = { $day }: New messages
conversation-read-by = Read to here by { $names }
conversation-jump-unread = Jump to first unread
conversation-jump-present = Jump to present
conversation-earlier-unavailable = Earlier messages aren't on this device
conversation-attachment-open = Open
conversation-attachment-open-full-size = Open full size
conversation-attachment-save-as = Save as…
conversation-attachment-open-saved = Open saved copy
conversation-attachment-saved = a copy on this device
conversation-attachment-no-access = This file is not shared with you
conversation-attachment-unavailable = This file is not on this device
conversation-attachment-uploading = Uploading
conversation-attachment-cancel-upload = Cancel upload
conversation-attachment-image = Image
conversation-attachment-video = Video
conversation-attachment-file = File
conversation-draft = Draft
# Jobs: what is running. A job's row says what it is doing and how far.
jobs-starting = Starting
# $phase is what it is doing ("Downloading"), $done and $total are what it has
# got through and how much there is, each in the job's own unit.
jobs-phase-of = { $phase } { $done } of { $total }
# $phase is what it is doing, $seen is how much it has seen where there is no
# total to read.
jobs-phase-seen = { $phase } { $seen }
jobs-percent = { $percent }%
jobs-bar-of = { $done } of { $total }
jobs-failed = Couldn’t finish: { $reason }
jobs-stopped = Stopped
jobs-stop = Stop
jobs-stopping = Stopping
jobs-show-me = Show me
jobs-read = Read
# $count is how many jobs the band is not showing.
jobs-more = { $count } more
jobs-running-heading = In progress
jobs-finished-heading = Finished
jobs-none = Nothing is running

jobs-files-one = { $count } file
jobs-files = { $count } files
jobs-items-one = { $count } item
jobs-items = { $count } items
jobs-steps-one = { $count } step
jobs-steps = { $count } steps

# Needs you: what wants a person.
attention-blocking = Now
attention-waiting = Waiting
attention-noted = Worth knowing
attention-none = Nothing needs you
# $about is what it is about, $because is why it is waiting.
attention-because = { $about }, { $because }
attention-defer = Put aside for now
attention-deferred = { $count } put aside
attention-pill = { $count } waiting
action-document-replaced = This action did not run because its document was reopened or replaced. Try again in the current document.

# One-shot action proposals: one model call's answer, before it is written.
proposal-model-on-device = { $model }, running on this device
proposal-model-hosted = { $model }, running on another machine
proposal-context-selection = The selection
proposal-context-too-many = That is too much context to send at once. Choose less.
# $name is the part, $limit how many kilobytes one part may carry.
proposal-context-too-long = { $name } is longer than { $limit } KB. Choose less.
proposal-diff-more =
    { $count ->
        [one] One more line not shown
       *[other] { $count } more lines not shown
    }
proposal-accept = Apply
proposal-reject = Discard
proposal-decisions = Apply this change, or discard it

## Completion drawers
completion-in-conversation = In this conversation
completion-members = Members
completion-no-match = No matches for “{ $query }”
completion-results = { $count ->
    [one] 1 result
   *[other] { $count } results
}
emoji-picker-add-reaction = Add reaction
quote-dismiss = Stop replying
quote-open = Show what this replies to

toast-reply-placeholder = Reply…

## Window furniture and widget chrome, named where a screen reader reads it
floating-window-move-hint = Movable window. Hold Alt and use the arrow keys to move; hold Shift for larger steps.
widget-dialog-close = Close
widget-dialog-cancel = Cancel
widget-dismiss = Dismiss
widget-hand-to-agent = Hand to an agent
widget-more = More
widget-copy = Copy
widget-hosted-applications = Hosted applications
widget-searching = Searching…
widget-search-results = Search results
widget-loading = Loading
widget-timeline-name = Timeline position
widget-timeline-hint = Chronological collection. Arrow keys move, Page Up and Page Down move a screen, Home and End jump to the beginning or end; plus and minus change density.
widget-timeline-jump = Jump to { $marker }
widget-timeline-group = Chronological index
widget-timeline-group-hint = Date-indexed scrollbar
widget-shortcut-change = Change shortcut
widget-shortcut-unavailable = That shortcut is unavailable. Activate to choose another.
widget-chart-series = { $name } series
widget-step-of = Step { $step } of { $total }
widget-steps-count = { $count ->
    [one] { $count } step
   *[other] { $count } steps
  }
widget-page-of = Page { $page } of { $total }
widget-preview-named = { $label } preview
widget-type-to-confirm = Type { $phrase } to confirm
widget-color-name = Colour
widget-color-hex = Hex colour
widget-crop-name = Crop page
widget-crop-hint = From { $left }% across and { $top }% down to { $right }% across and { $bottom }% down
widget-document-switch-blocked = Document switch blocked
widget-qr-failed = Could not draw the QR code: { $error }
media-volume = Volume
media-position = Position
media-more-controls = More controls
media-play = Play
media-pause = Pause
media-previous = Previous track
media-next = Next track
media-mute = Mute
media-unmute = Unmute
media-like = Like
media-unlike = Unlike
media-lyrics = Lyrics
media-shuffle = Shuffle
media-repeat = Repeat
media-repeat-one = Repeat one
media-repeat-all = Repeat all
media-queue = Queue
media-player = Video player
media-no-video = No video loaded
media-player-hint = Space plays or pauses; Left and Right Arrow seek; Up and Down Arrow change volume; M mutes; F toggles fullscreen
media-key-play-pause = Space
media-position-value = { $position } of { $duration }
media-fullscreen-enter = Enter fullscreen
media-fullscreen-exit = Exit fullscreen
media-playback-settings = Playback settings
media-toggle-on = On
media-toggle-off = Off
floating-window-title = Floating window
now-playing-seek = Playback position
now-playing-seek-help = Use Left and Right Arrow to move five seconds

## What the records are called, and the actions on them
settings-text-assistance = Spelling and text
