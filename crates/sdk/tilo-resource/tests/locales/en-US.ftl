# The words of the declarations in this crate's tests.

## crates/sdk/tilo-resource/src/concurrent_text_tests.rs
test-pad = Pad
test-pad-add = Add an item

## crates/sdk/tilo-resource/src/reactions_tests.rs
test-board = Board
test-board-add = Add a card

## crates/sdk/tilo-resource/tests/apply_cache.rs
test-cached-checklist = Checklist
test-cached-checklist-draft = Type into the draft
test-cached-checklist-add = Add a task
test-cached-checklist-toggle = Tick or untick a task
test-cached-checklist-retitle = Retitle a task, and note why
test-cached-checklist-move-task = Move a task
test-cached-checklist-remove = Delete a task
test-cached-checklist-flip = Switch between list and board
test-cached-checklist-tag = Tag the list
test-cached-checklist-refuse = Refuse, having changed the record first
test-merged-texts = Texts
test-merged-texts-keep-text = Keep a text under a key, the first time
test-merged-texts-bump = Count once more
test-cached-table = Table
test-cached-table-add-row = Add a row
test-cached-table-add-column = Add a column
test-cached-table-write = Write a cell, or clear it with no text
test-cached-table-drop-row = Remove a row
test-cached-table-rename = Rename the table

## crates/sdk/tilo-resource/tests/by_app.rs
test-visits = Visits
test-visits-note = Note a visit
test-visits-star = Star a page

## crates/sdk/tilo-resource/tests/data_enum.rs
test-card = Card
test-card-draw-rect = Draw a box `width` units wide
test-card-draw-text = Draw text wrapping at `width` units, or not wrapping
test-card-widen = Make the box `width` units wide, when the figure is a box
test-card-set-wrap = Wrap the text at `width` units, when the figure is text
test-card-text = Make the text, saying `value`
test-card-count = Make the count, counting `value`
test-rules = Rules
test-rules-match-host = Match a host
test-rules-match-path = Match a path
test-rules-rename-host = Rename the host matched
test-rules-set-port = Change the port matched

## crates/sdk/tilo-resource/tests/dates.rs
test-diary = Diary
test-diary-plan = Plan a moment

## crates/sdk/tilo-resource/tests/evolution.rs
test-contact = Contact
test-contact-rename = Rename the contact
test-contact-invite = Invite the contact

## crates/sdk/tilo-resource/tests/fields.rs
test-board-view-look = Look at part of the board
test-board-clear = Remove every card
test-board-restyle = Retitle the board and set its card count
test-board-shout = Retitle the board loudly: the title is resolved once, where invoked
test-board-retitle = Retitle the board, with an optional suffix
test-board-clear-note = Clear a note's lines, answering what it cleared
test-board-add-card-for = Note who a card was added for, and by what
test-board-add-card = Add a card, signed by whoever adds it
test-lock = Lock
test-screen = Screen
test-pair = Pair
test-pair-move-left = Move the left side
test-deck = Deck
test-deck-nudge = Nudge an element along
test-comfort = Comfort
test-comfort-scale-text = Draw text at `scale`
test-swatch = Swatch
test-swatch-clear = Clear the colour
test-frame = Frame
test-frame-untitle = Clear the title

## crates/sdk/tilo-resource/tests/footprint.rs
test-review = Review
test-review-start-thread = Start a thread: writes a thread's unit, so it says so
test-review-sneak-thread = Start a thread without saying so
test-review-reply = Reply in a thread, and touch the record
test-review-summarize = Summarise from a thread, writing a unit it does not declare

## crates/sdk/tilo-resource/tests/golden_path.rs
test-board-rename = Rename the board
test-board-advance = Advance a card to the next stage
test-board-move-card = Move a card to a position
test-board-delete-card = Delete a card
test-channel = Channel
test-channel-set-topic = Set the topic
test-channel-mark-read = Mark the channel read

## crates/sdk/tilo-resource/tests/housekeeping_tag.rs
test-channel-post = Post a message

## crates/sdk/tilo-resource/tests/incoming_message.rs
test-inbox = Inbox
test-inbox-receive = Receive a message
test-inbox-note = Note something down

## crates/sdk/tilo-resource/tests/incremental.rs
test-notebook = Notebook
test-notebook-rename = Rename
test-notebook-theme = Set the theme
test-notebook-add = Add a note
test-notebook-edit = Edit a note's text
test-notebook-decorate = Pin and colour a note, or unpin it with no colour
test-notebook-move-note = Move a note
test-notebook-delete = Delete a note
test-notebook-tag = Count a tag
test-sheet = Sheet
test-sheet-add-row = Add a row
test-sheet-add-column = Add a column
test-sheet-write = Write a cell, or clear it with no text
test-sheet-drop-row = Remove a row
test-sheet-drop-column = Remove a column
test-board-edit-card = Edit a card's text, wherever it is
test-board-archive-card = Archive a card
test-board-stack-card = Stack a card at a place, the top when past it
test-board-unstack = Drop the card at the bottom of the stack
test-board-see = Mark a word seen, or unseen
test-track = Track
test-track-add-cue = Add a cue at a place
test-shelf = Shelf
test-shelf-retitle = Rename
test-shelf-mark = Record where a book is read to
test-shelf-forget = Forget a book

## crates/sdk/tilo-resource/tests/keyed.rs
test-timeline = Timeline
test-timeline-add-generated = Add a clip with an id of its own
test-timeline-add-clip = Add a clip

## crates/sdk/tilo-resource/tests/outbox.rs
test-desk = Desk
test-desk-add-note = Add a note
test-desk-remove-note = Remove a note
test-desk-trash = Move a file to the trash
test-desk-revert = Put back what a file operation changed
test-desk-found-gone = Record that the device found a file already gone
test-planner = Planner
test-planner-add-event = Add an event
test-planner-import-events = Import a calendar's events, one undo step
test-planner-ingest = Take the events a server sent
test-runs = Runs
test-runs-run = Run a command

## crates/sdk/tilo-resource/tests/paths.rs
test-library = Library
test-library-new-snippet = Create a snippet: a unit of its own
test-library-record-use = Record a use
test-library-star = Star the library
test-library-share = Share the library with someone
test-shelf-shelve = Shelve a book
test-misdeclared = Misdeclared
test-misdeclared-rename = Rename it

## crates/sdk/tilo-resource/tests/upsert.rs
test-party = Party

## crates/sdk/tilo-resource/tests/verbs.rs
test-shelf-view-type-here = Type at the invoker's caret
test-shelf-view-top = Scroll to the top
test-shelf-cards-show = Show a card
test-shelf-press-print = Print the shelf
test-shelf-press-count = Count the cards
test-shelf-press-export = Export the shelf
test-shelf-wipe-wipe = Wipe a drive. Offered on each drive
test-shelf-wipe-rescan = Rescan drives
test-shelf-add = Add a card
test-shelf-archive-card = Archive a card
test-shelf-restore = Restore an archived card
test-shelf-place-picture = Place a picture on the shelf
test-shelf-caption-picture = Caption a picture: content another action produced, beside words
test-shelf-mail-to = Mail the shelf to someone
test-book = Book
test-book-add = Add a card
test-book-remove = Remove a card
test-book-rename = Change a card's first name
test-pins = Pins
test-pins-pin = Pin a place
test-reports = Reports
test-reports-file = File a report
test-pages = Pages
test-pages-add-page = Add a page
test-private-import = Private import
test-private-import-import = Receive a private snapshot
test-private-import-optional-import = Receive an optional private snapshot

## Actions bound to what the person is looking at
test-board-retitle-sprint = Call it Sprint
test-timeline-add-clip-here = Add a clip here
test-effects-reload = Reload everything
test-effects-rename-thing = Rename a thing
test-effects-wipe = Wipe
test-effects-copy-key = Copy a thing's key
test-effects-paint = Paint a thing
test-effects-widen = Widen a column
test-transforms-shrink = Shrink images
test-transforms-copy = Copy an image
test-rows-open = Open an item
test-rows-connect = Connect to an item
test-rows-search = Search
test-exports-export-book = Export the address book
test-secrets-pair = Pair a device
test-files-save-copy = Save a copy
test-readers-open = Open documents
