# The words of the declarations in this crate's tests.

## crates/sdk/tilo-app/src/framework/bar_actions_tests.rs
test-bar-counter-view-zoom-in = Zoom in
test-bar-counter = Counter
test-bar-counter-add = Add one
test-bar-counter-clear = Clear the count

## crates/sdk/tilo-app/src/framework/children.rs
test-workspace = Workspace
test-workspace-touch = Rename nothing
test-topic = Topics
test-topic-jot = Note

## crates/sdk/tilo-app/src/framework/effect/tests.rs
test-probe-effects-count = Count until cancelled
test-probe-effects-ping = Answer at once
test-probe = Probe
test-probe-set = Set the count

## crates/sdk/tilo-app/src/framework/preview.rs
test-page = Page
test-page-set-heading = Set the heading
test-shelf = Shelf
test-shelf-rename = Rename the shelf
test-shelf-add = Add a book
test-shelf-finish = Mark a book read
test-shelf-remove = Remove a book
test-shelf-annotate = Annotate the shelf

## crates/sdk/tilo-app/src/framework/root/search_tests.rs
test-mailbox = Mailbox
test-mailbox-set-subject = Set the subject
test-shelf-retitle = Retitle the book
test-notebook = Notebook
test-notebook-add = Add a note

## crates/sdk/tilo-app/src/framework/root/shared_tree.rs
test-shared-board = Board
test-shared-board-draft = Set the draft
test-shared-board-add = Add a task at `index`
test-shared-board-set-done = Mark a task done or open
test-shared-board-rename = Retitle a task
test-shared-board-remove = Remove a task
test-shared-board-move-task = Move a task to `index`
test-shared-board-jot = Add a note
test-shared-board-drop-note = Drop a note
test-tagged = Tagged
test-tagged-write = Write the draft
test-tagged-tag = File the draft under `tag`
test-tagged-untag = Take the tag `tag` off

## crates/sdk/tilo-app/src/framework/saves.rs
test-save-status-draft = Draft
test-save-status-draft-write = Set the text

## crates/sdk/tilo-app/src/framework/window_bar_tests.rs
test-window-bar-tally-view-list = Show the list
test-window-bar-tally-view-board = Show the board
test-window-bar-tally-view-sidebar = Show the tally's sidebar
test-window-bar-tally = Tally
test-window-bar-tally-add = Add one

## crates/sdk/tilo-app/tests/action_groups.rs
test-shopping = Shopping list
test-shopping-rename = Rename the list
test-shopping-add = Add an entry
test-shopping-clear = Remove every entry

## crates/sdk/tilo-app/tests/action_proposal.rs
test-note = Note

## crates/sdk/tilo-app/tests/attached_view.rs
test-bench = Bench
test-bench-sit = Sit down
test-ledger = Ledger
test-ledger-ask = Ask for an entry
test-ledger-answer = Record what looking an entry up answered

## crates/sdk/tilo-app/tests/chain_list.rs
test-chain-log = Log
test-chain-log-rename = Rename the log
test-chain-log-part = Lines
test-chain-log-part-write = Write a line

## crates/sdk/tilo-app/tests/child_records.rs
test-child-workspace = Workspace
test-child-workspace-touch = Rename nothing
test-child-topic = Topics
test-child-topic-rename = Rename the topic

## crates/sdk/tilo-app/tests/confirmation.rs
test-notes = Notes
test-notes-add = Add a note
test-notes-remove = Remove a note
test-notes-clear = Remove every note
test-notes-forget = Forget every note, with what each device kept beside the document:

## crates/sdk/tilo-app/tests/direct_records.rs
test-direct = Direct conversation
test-direct-say = Say something
test-direct-mark-read = Mark how far the person has read

## crates/sdk/tilo-app/tests/edit_answers.rs
test-deck-effects-add-card = Add a card
test-deck-effects-add-two = Add two cards
test-deck = Deck

## crates/sdk/tilo-app/tests/effects.rs
test-mailbox-effects-count = Count the delivered messages
test-mailbox-effects-fetch = Fetch new mail
test-mailbox-effects-connect = Connect to the server
test-mailbox-effects-stall = Promise an answer without starting anything
test-mailbox-effects-receipt = Settle an external receipt as the original caller
test-mailbox-effects-read-receipt = A read receipt cannot authorize a document edit
test-mailbox-effects-note = Note what was read, as an edit
test-mailbox-effects-show-sent = Show the sent folder
test-tally-effects-tally = Tally
test-tally-effects-count = Count a row
test-counter-effects-count = Count
test-counter-effects-counted = Read what the window counted
test-hosted-counter-effects-count-later = Count off the UI thread
test-mounting-effects-open-child = Mount the child window
test-mounting-effects-close-child = Let the child window go
test-mailbox-files-import = Import a message from a file
test-mailbox-files-export = Export what was fetched to a file
test-mailbox-files-sync-device = Sync with a device that is busy
test-mailbox-device-choose-folder = Choose a mail folder on this computer
test-mailbox-device-empty-local-folder = Empty the mail folder on this computer
test-mailbox-disks-erase = Erase a disk
test-mailbox-send = Queue a message for delivery
test-mailbox-delivered = Record that a queued message was delivered
test-mailbox-fetched = Record what a fetch brought

## crates/sdk/tilo-app/tests/forms.rs
test-bookmarks = Bookmarks
test-bookmarks-add = Add a bookmark

## crates/sdk/tilo-app/tests/framework.rs
test-checklist = Checklist
test-checklist-rename = Rename the checklist
test-checklist-add = Add an item
test-checklist-set-done = Check or uncheck an item
test-checklist-clear-done = Remove every checked item

## crates/sdk/tilo-app/tests/gestures.rs
test-mixer = Mixer

## crates/sdk/tilo-app/tests/history_actions.rs
test-board = Board
test-board-rename = Rename the board
test-board-add = Add a card
test-board-reword = Reword a card

## crates/sdk/tilo-app/tests/host_context.rs
test-runs = Runs
test-runs-run-on = Record a run on `device`

## crates/sdk/tilo-app/tests/identity.rs
test-board-claim = Make whoever invokes this the board's owner
test-board-post = Post a note, signed by whoever posts it
test-board-show-headline = Show the headline a refresh fetched
test-board-set-level = Set the level

## crates/sdk/tilo-app/tests/incremental.rs
test-library = Library
test-library-add-many = Add books
test-library-mark = Mark a book read
test-library-retitle = Retitle a book

## crates/sdk/tilo-app/tests/item_actions.rs
test-todos = Todos
test-todos-add = Add a todo
test-todos-set-done = Check or uncheck a todo
test-todos-remove = Remove a todo

## crates/sdk/tilo-app/tests/item_effects.rs
test-queries-effect-run = Run a saved query
test-queries = Queries
test-queries-save = Save a query

## crates/sdk/tilo-app/tests/item_navigation.rs
test-navigation = Pages
test-navigation-add = Add a page

## crates/sdk/tilo-app/tests/item_offers.rs
test-channels = Channels
test-channels-add = Add a channel

## crates/sdk/tilo-app/tests/kinds.rs
test-tally = Tally
test-tally-rename = Rename the tally
test-tally-bump = Count one more

## crates/sdk/tilo-app/tests/landings.rs
test-script-effects-mark = Mark a line
test-script = Script
test-script-append = Append a line

## crates/sdk/tilo-app/tests/launch.rs
test-gallery-effects-show = Show a picture
test-sketch = Sketch
test-sketch-trace = Trace over an image
test-sketch-note = Add a note

## crates/sdk/tilo-app/tests/lazy_rows.rs
test-lazy-library-shelf-open = Open the book
test-lazy-library = Library
test-lazy-library-add-many = Add books

## crates/sdk/tilo-app/tests/live_permissions.rs
test-memo = Memo
test-memo-retitle = Retitle the memo
test-memo-add-line = Add a line
test-memo-remark = Remark on the memo: someone who may only comment may
test-memo-configure = Configure the memo

## crates/sdk/tilo-app/tests/live_rows.rs
test-tally-jobs-stop = Stop the job
test-tally-jobs-inspect = Inspect the total
test-tally-jobs-reveal = Reveal the total

## crates/sdk/tilo-app/tests/other_records.rs
test-board-retitle = Retitle the board
test-display = Display
test-display-reset-scale = Draw everything at its natural size
test-playback = Playback
test-playback-mute = Mute a channel
test-playback-unmute-all = Unmute every channel
test-lock = Lock

## crates/sdk/tilo-app/tests/outbox.rs
test-post = Post
test-post-send = Send a message through the account on `device`
test-post-recall = Take a sent message back
test-post-write = Write a note
test-agenda = Agenda
test-agenda-reschedule = Move the meeting and rename it at once

## crates/sdk/tilo-app/tests/person_records.rs
test-queue = Queue
test-queue-add = Queue a video
test-watched = Watch history
test-watched-watch = Remember that a video was watched
test-saved = Saved
test-saved-save = Save a video for later

## crates/sdk/tilo-app/tests/private_windows.rs
test-tabs = Tabs
test-tabs-open = Open a tab
test-visits = Visits
test-visits-visit = Remember a visit

## crates/sdk/tilo-app/tests/reactivity.rs
test-shelf-add-many = Add books
test-shelf-mark = Mark a book read

## crates/sdk/tilo-app/tests/record_reactions.rs
test-poster = Poster
test-poster-retitle = Retitle the poster

## crates/sdk/tilo-app/tests/references.rs
test-reference-notes = Notes

## crates/sdk/tilo-app/tests/render_anchor.rs
test-notebook-write = Write a page

## crates/sdk/tilo-app/tests/render_view.rs
test-sketch-rename = Rename the sketch

## crates/sdk/tilo-app/tests/scenarios.rs
test-playlist = Playlist
test-playlist-add = Add a song

## crates/sdk/tilo-app/tests/settings.rs
test-log = Log
test-log-retitle = Retitle the log

## crates/sdk/tilo-app/tests/share_links.rs
test-notebook-tag = Add a tag

## crates/sdk/tilo-app/tests/support/checklist.rs
test-checklist-draft = Set the next task's title
test-checklist-seed = Add `count` tasks at the end
test-checklist-seed-long = Add `count` tasks whose titles wrap in a row, every third done

## crates/sdk/tilo-app/tests/surface_report.rs
test-surface-report-effects-count = Count what the record holds
test-surface-report-effects-zoom-in = Zoom the window in
test-counter = Counter
test-counter-bump = Count one more
test-counter-prefs = Counter preferences

## crates/sdk/tilo-app/tests/surface_semantics.rs
test-ballot = Ballot
test-ballot-approve = Count a vote for
test-ballot-reject = Count a vote against

## crates/sdk/tilo-app/tests/type_history.rs
test-blob = File
test-blob-rename = Keep the bytes under a new name

## crates/sdk/tilo-app/tests/unreadable.rs
test-memo-clear = Clear the title
test-learned = Learned words
test-learned-forget = Forget everything the keyboard learned

## crates/sdk/tilo-app/tests/window_content.rs
test-album = Album
test-album-attach = Keep `text` as a picture of the album

## crates/sdk/tilo-app/tests/workspace_arrival.rs
test-workspace-arrival-board = Board
test-workspace-arrival-board-add = Add a card

## crates/sdk/tilo-app/tests/workspace_records.rs
test-note-write = Write the note
test-review-replay = test.review-replay
test-blob-store = Keep the bytes a file holds
