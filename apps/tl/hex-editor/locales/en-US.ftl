# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-hex-editor = Hex Editor

d-so-tilo-hex-editor-keywords = hex editor, hex, bytes, binary, binary editor, editor, disk, byte, hexadezimal, binär

d-cjs7wfwl = Change
    .doc = One edit of the bytes: at `at`, `remove` bytes stand as `insert`'s hexadecimal ones.
    .noun = { $count ->
        [one] change
       *[other] changes
    }

d-b4ynsjfc = At

d-lkghkw33 = Remove

d-n55erru5 = Insert

d-cjs7wfwl-filter-at = At

d-cjs7wfwl-filter-remove = Remove

d-z7zzcrb6 = Row
    .doc = One row of the grid as the device read it: its bytes as hexadecimal pairs, and for each a `0` or `1` saying whether it differs from what the file holds.
    .noun = { $count ->
        [one] row
       *[other] rows
    }

d-rqqjo5b2 = Hex

d-i23efoqg = Changed

d-gd5t2ug6 = Finding
    .doc = One way the bytes at the cursor read: `u16` is 4660 there.
    .noun = { $count ->
        [one] finding
       *[other] findings
    }

d-j3rx5nr6 = Label

d-bqtm2sxd = Value

d-whqswnrc = Width

d-gd5t2ug6-filter-width = Width

d-fsjz7klv = Span
    .doc = A span of bytes, half open.
    .noun = { $count ->
        [one] span
       *[other] spans
    }

d-vjv7m35i = Start

d-6nu52vd6 = End

d-fsjz7klv-filter-start = Start

d-fsjz7klv-filter-end = End

d-s5feqeox = Desk
    .doc = One person's place in a document: their cursor, how they read the bytes, and what the device last read for their window.
    .noun = { $count ->
        [one] desk
       *[other] desks
    }

d-4pp4s3sf = Cursor

d-ta3nqlbq = Anchor

d-d6nzpsjf = Column
    .doc = `hex` or `ascii`: which column the keys type into.

d-mum5d7nq = Mode
    .doc = `overwrite` replaces the byte under the cursor; `insert` splices a new one in.

d-xmgji4li = High
    .doc = The high nibble typed in overwrite mode, which the next digit completes; none between bytes.

d-b23s2qvq = Endian
    .doc = `little` or `big`: the order the inspector reads numbers in.

d-ntd56cpd = First
    .doc = The first row the window shows.

d-d4rmfnak = Columns
    .doc = The bytes a row holds.

d-u54h5izd = Rows
    .doc = The rows the window shows, as the device read them: each its bytes as hexadecimal pairs.

d-iyf5x4du = Changed
    .doc = For each row shown, a `0` or `1` for each of its bytes: whether it differs from what the file holds.

d-nczhspum = Findings
    .doc = What the bytes at the cursor read as: a label and a value, a tab between them.

d-uswoxemn = Hits
    .doc = What the window's search found, each as `start:end`, and which of them the cursor is on.

d-nv3vcmev = At match

d-s777zrl4 = Truncated

d-gy4cijiy = Query
    .doc = The query last searched, and how it was read.

d-hfchq6ug = Kind

d-pzwozqbg = Match case

d-olbisfbn = Whole word

d-jko5an6z = Panel
    .doc = The strip open under the bar: `search`, `replace`, `goto` or `inspector`; empty for none.

d-n7ibv25j = Notice
    .doc = What the window last said: a result, or what went wrong.

d-jslwh26v = Notice kind

d-s5feqeox-filter-cursor = Cursor

d-s5feqeox-filter-anchor = Anchor

d-s5feqeox-filter-high = High

d-s5feqeox-filter-first = First

d-s5feqeox-filter-columns = Columns

d-s5feqeox-filter-at_match = At match

d-s5feqeox-filter-truncated = Truncated

d-s5feqeox-filter-match_case = Match case

d-s5feqeox-filter-whole_word = Whole word

d-jck5gzaa = Document
    .doc = One file's bytes: where they stand in the workspace's content store, and the edits made since they were read.
    .noun = { $count ->
        [one] document
       *[other] documents
    }
    .untitled = Untitled.bin

d-dbkdqzos = Name

d-teahiasd = Opened
    .doc = How many times the bytes were read afresh.

d-blhbkxjc = Base
    .doc = The address of the bytes as they were read or last saved; empty for an empty buffer.

d-wts2l2hg = Path
    .doc = Where the file stands on this device; empty for one handed over or saved nowhere.

d-nqk5afwc = Length
    .doc = The length of the bytes with the edits applied.

d-7u75gmtu = Edited
    .doc = How many bytes differ from what the file holds.

d-3tqozdp3 = Changes
    .doc = The edits made since the base was read, in order: `at:remove:insert`, the bytes to splice as hexadecimal pairs.

d-2fs4ukcj = Desks
    .doc = Each person's own place in the document.

d-jck5gzaa-filter-opened = Opened

d-jck5gzaa-filter-length = Length

d-jck5gzaa-filter-edited = Edited

d-v5bn27jt = Library
    .doc = The workspace's Hex Editor: the buffers it has open.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-fu57pivb = Documents

d-tehzgnkf = Open ask
    .noun = { $count ->
        [one] open ask
       *[other] open asks
    }

d-rruucmz4 = Path

d-ycca2usf = Opened answer
    .noun = { $count ->
        [one] opened answer
       *[other] opened answers
    }

d-cevle23p = Name

d-idpg6pxk = Address

d-cr3tncji = Length

d-ycca2usf-filter-length = Length

d-uu2y2bvc = Rows ask
    .noun = { $count ->
        [one] rows ask
       *[other] rows asks
    }

d-ldq6mtad = Address

d-2t4i36rg = Changes

d-52p24f3b = First

d-j3ztka6c = Count

d-jmwhucjc = Columns

d-tjv2yhp3 = Endian

d-oowz47v3 = Cursor

d-uu2y2bvc-filter-first = First

d-uu2y2bvc-filter-count = Count

d-uu2y2bvc-filter-columns = Columns

d-uu2y2bvc-filter-cursor = Cursor

d-rvfaddkj = Rows answer
    .noun = { $count ->
        [one] rows answer
       *[other] rows answers
    }

d-yjjtzhyt = Length

d-ao74hr7t = Edited

d-lgghdfh2 = Read

d-df32ywi4 = Error

d-jhbkwvr2 = Rows

d-3ugkwm42 = Changed

d-xhxdxsgb = Findings

d-rvfaddkj-filter-length = Length

d-rvfaddkj-filter-edited = Edited

d-rvfaddkj-filter-read = Read

d-jfroysuv = Find ask
    .noun = { $count ->
        [one] find ask
       *[other] find asks
    }

d-6grc7o75 = Address

d-5yumynmf = Changes

d-ljornfmm = Query

d-wyvxlbd4 = Kind

d-b4bt6mtj = Case

d-bxix7im4 = Word

d-qyar3ymw = Limit

d-jfroysuv-filter-case = Case

d-jfroysuv-filter-word = Word

d-jfroysuv-filter-limit = Limit

d-26cey6xc = Found answer
    .noun = { $count ->
        [one] found answer
       *[other] found answers
    }

d-t6d5sfoz = Hits

d-4fdr6kla = Truncated

d-26cey6xc-filter-truncated = Truncated

d-qpiqvvbd = Apply ask
    .noun = { $count ->
        [one] apply ask
       *[other] apply asks
    }

d-ba424tqt = Address

d-qtf35usu = Changes

d-r5xf3t6m = Op

d-z2ahkv3u = At

d-3kwri4q5 = Len

d-pk7jqula = Bytes

d-qpiqvvbd-filter-at = At

d-qpiqvvbd-filter-len = Len

d-l4ctb355 = Applied answer
    .noun = { $count ->
        [one] applied answer
       *[other] applied answers
    }

d-2w7szbth = Change

d-ll4ibsb5 = Length

d-l4ctb355-filter-length = Length

d-wdtj6n4g = Replace ask
    .noun = { $count ->
        [one] replace ask
       *[other] replace asks
    }

d-3riih5ln = Address

d-o4kbz7cm = Changes

d-ptnk64f6 = Spans

d-us73s4xt = Bytes

d-mj26u2qk = Mode

d-452gevoq = Replaced answer
    .noun = { $count ->
        [one] replaced answer
       *[other] replaced answers
    }

d-j3lt4zli = Changes

d-3kaouk56 = Count

d-4fce7y46 = Length

d-452gevoq-filter-count = Count

d-452gevoq-filter-length = Length

d-uodblmtb = Range ask
    .noun = { $count ->
        [one] range ask
       *[other] range asks
    }

d-3surtarm = Address

d-l7dzgaqe = Changes

d-3v66hb5v = Start

d-fsjcytnx = End

d-fyp52l2r = Kind

d-uodblmtb-filter-start = Start

d-uodblmtb-filter-end = End

d-xwmajagh = Range answer
    .noun = { $count ->
        [one] range answer
       *[other] range answers
    }

d-d7nxzpqz = Text

d-jzqk7v3k = Bytes ask
    .noun = { $count ->
        [one] bytes ask
       *[other] bytes asks
    }

d-66cpqgom = Address

d-4tyyvxqh = Changes

d-c4k3zrrj = Bytes answer
    .noun = { $count ->
        [one] bytes answer
       *[other] bytes answers
    }

d-smxqtshm = Bytes

d-yvz46vke = Save ask
    .noun = { $count ->
        [one] save ask
       *[other] save asks
    }

d-62acy6x2 = Address

d-2d44lw44 = Changes

d-sujs7lc2 = Path

d-evzo42ed = Saved answer
    .noun = { $count ->
        [one] saved answer
       *[other] saved answers
    }

d-fnprm7qy = Address

d-bnwrfq64 = Length

d-evzo42ed-filter-length = Length

d-lku5ldrh = Arrive
    .doc = A document is opened by whoever opens it: they get their own place in it, and the window reads the bytes it shows.

d-ib5i76y6 = Move cursor
    .doc = The cursor moved to `at`, kept inside the bytes; `extend` keeps where the selection started, and the window's first row follows it when it leaves the rows shown.

d-ib5i76y6-desk = Desk

d-ib5i76y6-at = At

d-ib5i76y6-extend = Extend

d-ykxrxick = Say
    .doc = Say something in the window: a result, or what went wrong.

d-ykxrxick-words = Words

d-ykxrxick-kind = Kind

d-ryi5rumk = Show
    .doc = Read the window's rows and the bytes at its cursor from the device again.

d-vojndsoo = Edit
    .doc = Apply one edit of the bytes: the device checks it and answers the change to keep.

d-vojndsoo-op = Op

d-vojndsoo-at = At

d-vojndsoo-len = Len

d-vojndsoo-bytes = Bytes

d-topfzl2k = Open path
    .doc = Opens the file at `path` on this device as its own buffer.

d-topfzl2k-path = Path

d-lsmhjlew = Refused buffer

d-lsmhjlew-path = Path

d-lsmhjlew-why = Why

d-ki2kthjn = Open a file
    .doc = Opens a file a person chose, or one another app handed over, as its own buffer.
    .did = opened a file

d-ki2kthjn-file = File

d-gdczru47 = Opened buffer
    .doc = The buffer of the bytes at `address`: the one already open on them, read afresh, or a new one.

d-gdczru47-name = Name

d-gdczru47-address = Address

d-gdczru47-path = Path

d-gdczru47-length = Length

d-wydoopxa = New empty file
    .doc = An empty buffer to type bytes into, saved wherever the person says.
    .did = started an empty buffer

d-eziyendc = Empty buffer

d-eziyendc-name = Name

d-5ptrab3k = Open a file
    .doc = The action the person's file chooser fills.

d-5ptrab3k-file = File

d-wucjou7l = Save
    .doc = Saves the buffer where it came from, or asks where when it stands nowhere yet.
    .did = saved the file
    .w-this-file-stands-nowhere-on-this = This file stands nowhere on this device yet: use Save as

d-o7q63qsf = Save bytes
    .doc = The bytes as a file the person places with the system's save chooser, which the window shows.

d-ounak5zh = Rebased
    .doc = What was saved is the base: no edit stands against it any more.

d-ounak5zh-address = Address

d-ounak5zh-length = Length

d-e3uvry6i = Close the file
    .doc = Forgets a buffer, its edits with it.

d-e3uvry6i-d = D

d-sjgncfl3 = Press a key
    .doc = The keys pressed on the grid, as the person's keyboard names them.

d-sjgncfl3-key = Key

d-tky2mwod = Chord
    .doc = The Ctrl chords typed while the bytes have the keyboard.

d-tky2mwod-desk = Desk

d-tky2mwod-key = Key

d-53hjuxzd = Moved
    .doc = The cursor moved to `at`: the rows and the inspector read there.

d-53hjuxzd-desk = Desk

d-53hjuxzd-at = At

d-53hjuxzd-extend = Extend

d-oxhf3mvc = Type key

d-oxhf3mvc-desk = Desk

d-oxhf3mvc-one = One

d-cddfr4of = Type hex
    .doc = Type one hexadecimal digit: in overwrite mode the first sets the high nibble and the second completes the byte; in insert mode each completed byte is spliced in.

d-cddfr4of-desk = Desk

d-cddfr4of-value = Value

d-i4voe4c3 = Type ascii
    .doc = Type a character into the ASCII column, then advance.

d-i4voe4c3-desk = Desk

d-i4voe4c3-one = One

d-d4uh3stf = Backspace
    .doc = In insert mode the byte before the cursor goes; in overwrite mode the cursor only steps back.

d-d4uh3stf-desk = Desk

d-5jv54u5l = Delete at cursor
    .doc = The selection goes, or else the byte under the cursor.

d-5jv54u5l-desk = Desk

d-wenh6lnl = Select all

d-wenh6lnl-desk = Desk

d-mr6fuewa = Copy
    .doc = Copies the selected bytes in the column's form: `50 4e 47 44`, or `PNGD`.

d-y5wcfls4 = Cut
    .doc = Copies the selection and deletes it.

d-t63az7ij = Paste
    .doc = Pastes the clipboard at the selection, or at the cursor, in the focus mode.

d-bvziotqd = Replaced
    .doc = Replaces each span with the same bytes, as one step.

d-bvziotqd-desk = Desk

d-bvziotqd-spans = Spans

d-bvziotqd-bytes = Bytes

d-lcexraol = Find bytes
    .doc = Opens the search strip.

d-tcw7lqtk = Find and replace
    .doc = Opens the replace strip.

d-s5cyut7x = Go to offset
    .doc = Opens the go-to-offset strip.

d-7kxwiqbq = Show or hide the inspector
    .doc = Shows or hides the inspector.

d-fsxslum7 = Close panel
    .doc = Closes the strip under the bar.

d-qr5huw5z = Search the bytes
    .doc = Finds `query` in the bytes: a hexadecimal pattern, or text under its rules.

d-qr5huw5z-query = Query

d-an5w227j = Reveal
    .doc = Puts the cursor on the match at `index`.

d-an5w227j-index = Index

d-elqaxiiv = Replace all
    .doc = Replaces every match the search found with `with`.

d-elqaxiiv-with = With

d-pq6chsex = Replace spans

d-pq6chsex-desk = Desk

d-pq6chsex-spans = Spans

d-pq6chsex-with = With

d-5ri4qufq = Search kind
    .doc = Reads the search strip's choices: text or hexadecimal, letter case and whole words.

d-5ri4qufq-i = I

d-uzubdaxi = Search case

d-uzubdaxi-on = On

d-qea6cfig = Search word

d-qea6cfig-on = On

d-fvwif6qs = Go to
    .doc = Moves the cursor to the offset `query` names.

d-fvwif6qs-query = Query

d-2cks257j = Set endian

d-2cks257j-i = I

d-hffonsed = Set mode

d-hffonsed-i = I

d-fgy2d4ey = Set column

d-fgy2d4ey-i = I

d-wilggcvm = Dismiss

d-yyd33pnt = Pressed
    .doc = A press on the grid: the byte at `at` gets the cursor, the column pressed the focus, and `"shift"` keeps where the selection started.

d-yyd33pnt-at = At

d-z3a3o4ap = Scrolled
    .doc = The wheel or the bar moved the window by `rows`.

d-z3a3o4ap-rows = Rows

d-ttzgh32m = Insert bytes
    .doc = Inserts `bytes` (hexadecimal, like `de ad be ef`) at `offset`.

d-ttzgh32m-offset = Offset

d-ttzgh32m-bytes = Bytes

d-4idnkxx2 = Replace bytes
    .doc = Replaces the bytes at `offset` with `bytes` (hexadecimal), never growing the file.

d-4idnkxx2-offset = Offset

d-4idnkxx2-bytes = Bytes

d-cgsqzkz4 = Delete bytes
    .doc = Removes `len` bytes at `offset`.

d-cgsqzkz4-offset = Offset

d-cgsqzkz4-len = Len

d-umksjjan = Save~present

d-it2igfh4 = Library
    .w-hex-editor = Hex Editor
    .w-open-file = Open file
    .w-new-empty-file = New empty file
    .w-no-file-open = No file open
    .w-open-a-file-to-read-its = Open a file to read its bytes, or start an empty one and type bytes in.
    .w-open-files = Open files
    .w-close = Close
    .w-changed = { $size_text } · { $edited } changed
    .w-not-on-disk = Not on disk
    .w-saved = Saved

d-hecirnqi = Document
    .w-dismiss = Dismiss
    .w-de-ad-be-ef = de ad be ef
    .w-words-in-the-bytes = Words in the bytes
    .w-read-as = Read as
    .w-hex = Hex
    .w-text = Text
    .w-match-case = Match case
    .w-whole-word = Whole word
    .w-close = Close
    .w-what-to-put-there = What to put there
    .w-replace-all = Replace all
    .w-first-matches = First { $len } matches
    .w-matches = { $len } matches
    .w-0x1f-or-31 = 0x1F or 31
    .w-reading-the-file = Reading the file…

d-c3gyftot = Toolbar
    .w-save = Save
    .w-save-as = Save as
    .w-edit-mode = Edit mode
    .w-overwrite = Overwrite
    .w-insert = Insert
    .w-keys-type = Keys type
    .w-hex = Hex
    .w-text = Text
    .w-edit = Edit
    .w-find-bytes = Find bytes
    .w-find-and-replace = Find and replace
    .w-go-to-offset = Go to offset
    .w-copy = Copy
    .w-cut = Cut
    .w-paste = Paste
    .w-select-all = Select all
    .w-inspector = Inspector

d-em4nrjtr = Inspector
    .w-inspector = Inspector
    .w-byte-order = Byte order
    .w-little = Little
    .w-big = Big

d-jdsqkjn7 = Status
    .w-offset-0x = Offset 0x{ $cursor_hex }
    .w-selected = { $byte_count } selected
    .w-edited = { $byte_count } edited
