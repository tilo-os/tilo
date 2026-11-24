# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-typst-editor = Typst

d-so-tilo-typst-editor-keywords = typst, typesetting, typeset, document, paper, pdf, editor, markup, satz, dokument, schreiben, typst-editor

d-j7sx7zxi = Problem
    .doc = One problem the typesetter reported: what it says, and where in the source it stands, its lines and columns counted from 1 (0 where the compiler states none).
    .noun = { $count ->
        [one] problem
       *[other] problems
    }

d-yt5nyujj = Severity

d-i3zcji5u = Message

d-od6pdrjz = Line

d-xrgk5gr2 = Column

d-f7mcrdxf = End line

d-7j5cjuet = End column

d-u6wlx35y = Hints

d-ds45ukw2 = Page
    .doc = One page of the typeset document: how big it is, an identity for what it draws, and the pictures it is shown by — the page at the width the window asks for, and its thumbnail.
    .noun = { $count ->
        [one] page
       *[other] pages
    }
    .untitled = Page

d-cgrnobye = Width
    .doc = Its width and height as the document lays it out, in points.

d-r4wxummm = Height

d-6r2kgenm = Look
    .doc = What it draws, as the typesetter names it: a page whose identity stands keeps its picture through a typeset that leaves it as it was.

d-7ry6v2yt = Picture
    .doc = The address of its picture at the width the window asked for, `tilo-blob://…`; empty until it is drawn.

d-ycgenfy7 = Picture width
    .doc = The width its picture was drawn at, in pixels; 0 until it is drawn.

d-wfjgjbj4 = Thumb
    .doc = The address of its thumbnail; empty until it is drawn.

d-yusfx3a6 = Thumb width
    .doc = The width its thumbnail was drawn at, in pixels; 0 until it is drawn.

d-jnlperg3 = Heading
    .doc = One heading of the document, read from its source's syntax: its line, counted from 1, how deep it stands, and its words.
    .noun = { $count ->
        [one] heading
       *[other] headings
    }

d-ougql7d2 = Title

d-ucauvawo = Line

d-t2356zsr = Level

d-jnlperg3-filter-line = Line

d-jnlperg3-filter-level = Level

d-ks73ekfk = Document
    .doc = A Typst document: its source, the file it is saved as, what the typesetter said of it, and the pages it lays out.
    .noun = { $count ->
        [one] document
       *[other] documents
    }
    .untitled = Untitled.typ

d-ojyy5fog = Name
    .doc = What the document is called.

d-wigkyjqk = Source
    .doc = Its source: the Typst text, merged character by character.

d-zlwvg46c = Typeset
    .doc = The source the last typeset read; one whose source moved past this is typeset again.

d-s3mabp2s = Typesets
    .doc = Whether the last typeset found a document (false while it does not typeset).

d-d7qirnkw = Took ms
    .doc = How long the last typeset took, in milliseconds.

d-hk3xko3g = Trouble
    .doc = Why it does not typeset, in the typesetter's words; empty while it does.

d-rrdheos7 = Written
    .doc = The source that was last written to a file the person chose.

d-bxbra2li = Problems
    .doc = What the typesetter found, blocking problems first.

d-jsru5ywo = Pages
    .doc = What the document lays out, in its order.

d-cltpfy4y = Headings
    .doc = Its headings, read from the source's syntax.

d-xn4pkllr = Pdf
    .doc = The address of the PDF the last typeset wrote, and what it is called; empty until one did.

d-e3bwnsci = Pdf name

d-akp7jws5 = Shown width
    .doc = The width pages are drawn at, in pixels: the preview's zoom, 0 for the window's own fit.

d-sfn7naaa = Showing
    .doc = What the window shows: `split` (the source beside the page), `editor`, `preview`, `problems` or `outline`.

d-g6gxruuq = Reveal
    .doc = Where the editor goes to, written `line:column:count`, counted from 1; empty for nowhere.

d-xad4v4m3 = Reveals
    .doc = How many places were gone to: each ask counts, so the same place shows again.

d-fijcb57x = Reading page
    .doc = The page the person reads, so they come back to it.

d-tbdqtt5g = Stale
    .doc = Whether the pages shown are the last that typeset, while the source no longer does.

d-alhhxqdi = Library
    .doc = The workspace's Typst documents.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-jdegntgs = Documents

d-pzdvdyxx = Compile ask
    .doc = The ask a typeset makes.
    .noun = { $count ->
        [one] compile ask
       *[other] compile asks
    }

d-d2jzgpyx = Source

d-w5eejkzm = Page ask
    .doc = The ask one page's picture makes.
    .noun = { $count ->
        [one] page ask
       *[other] page asks
    }

d-a66w2zy6 = Source

d-6jynr6xk = Page

d-m7iybhvb = Width

d-w5eejkzm-filter-page = Page

d-w5eejkzm-filter-width = Width

d-iyeymqh4 = Text ask
    .doc = The ask a page's words make.
    .noun = { $count ->
        [one] text ask
       *[other] text asks
    }

d-rydvb6fi = Source

d-rqodn5jw = Page

d-iyeymqh4-filter-page = Page

d-3naxgrbf = Outline ask
    .doc = The ask the outline makes.
    .noun = { $count ->
        [one] outline ask
       *[other] outline asks
    }

d-wevbiksf = Source

d-zjs45sex = Page row
    .doc = What the typesetter answered one page with.
    .noun = { $count ->
        [one] page row
       *[other] page rows
    }

d-mtlxa3ps = Width

d-ay4kqc7o = Height

d-ehra7vlr = Look

d-zjs45sex-filter-width = Width

d-zjs45sex-filter-height = Height

d-36735ayx = Problem row
    .doc = What the typesetter answered a problem with.
    .noun = { $count ->
        [one] problem row
       *[other] problem rows
    }

d-7vzeic6n = Kind

d-55dyzse3 = Message

d-bzomsyem = Line

d-vwara56h = Column

d-lewaebu2 = End line

d-unj2zbun = End column

d-wakty2b2 = Hints

d-36735ayx-filter-line = Line

d-36735ayx-filter-column = Column

d-36735ayx-filter-end_line = End line

d-36735ayx-filter-end_column = End column

d-kc7rlieo = Compiled row
    .doc = What the typesetter answered a typeset with.
    .noun = { $count ->
        [one] compiled row
       *[other] compiled rows
    }

d-ffvtdeuv = Ok

d-t5b6wta3 = Elapsed ms

d-siiy7bea = Pdf url
    .doc = The address of the PDF the typeset wrote, and what it is called; none while it wrote none.

d-htehc63o = Pdf name

d-63pe7jqy = Pdf size

d-uryvrrzv = Pdf trouble

d-kc7rlieo-filter-ok = Ok

d-kc7rlieo-filter-elapsed_ms = Elapsed ms

d-kc7rlieo-filter-pdf_size = Pdf size

d-ymzwzbqi = Heading row
    .doc = What the typesetter answered the outline with.
    .noun = { $count ->
        [one] heading row
       *[other] heading rows
    }

d-3olgjoh6 = Line

d-lcdznshm = Level

d-pq3mu5rf = Title

d-ymzwzbqi-filter-line = Line

d-ymzwzbqi-filter-level = Level

d-ewmnrcj6 = Cell row
    .doc = One placed glyph of a page.
    .noun = { $count ->
        [one] cell row
       *[other] cell rows
    }

d-dsvn4b2o = Text

d-erwowgdw = X0

d-txaxsk54 = X1

d-vobo3z4e = Top

d-w6ydh2nv = Bottom

d-73rf4vlf = Line

d-ewmnrcj6-filter-x0 = X0

d-ewmnrcj6-filter-x1 = X1

d-ewmnrcj6-filter-top = Top

d-ewmnrcj6-filter-bottom = Bottom

d-ewmnrcj6-filter-line = Line

d-5ox5sdk3 = Severity
    .doc = How much a problem the typesetter found matters.

d-6c2uqjwj = Error

d-gj5raevk = Warning

d-6wkf2pdj = Open a Typst document
    .doc = A Typst file opens in the editor: Files, the person's own file system and the system's search hand it over as the file it is, and it opens on the document those words are.
    .did = opened a Typst document

d-6wkf2pdj-file = File

d-5ipxgvaw = New Typst document
    .doc = A new, empty document.
    .did = made a Typst document

d-5ipxgvaw-name = Name

d-5ipxgvaw-source = Source

d-jdb3eb6g = Close the document
    .doc = A document leaves the workspace.
    .did = closed a document

d-jdb3eb6g-d = D

d-gxzucf5i = Typeset the document
    .doc = Typesets the document, as the window and the background do.
    .did = typeset a document

d-tqoyg4ac = Typeset the document when it changes
    .doc = Typesets the document when its source moved past the last typeset.

d-6eqv5vrp = Take problems
    .doc = Lays the problems a typeset reported over the ones the document held, blocking ones first.

d-6eqv5vrp-rows = Rows

d-sk6tqdpi = Take pages
    .doc = Lays the typeset pages over the ones the document held: a page that draws what it drew keeps its picture, and one that draws something else is drawn again.

d-sk6tqdpi-rows = Rows

d-cxvgocg6 = Read outline
    .doc = Reads the document's headings from its source's syntax.

d-2pimchjc = Draw a page
    .doc = Draws one page at `width` pixels wide, and its thumbnail.

d-2pimchjc-page = Page

d-2pimchjc-width = Width

d-ebsbeihs = Draw the next page
    .doc = Draws the next page the window's width asks for, one a second: a long document fills in page by page, without anyone waiting for it.

d-63azmqnp = Show the source beside the page
    .doc = Shows the source beside the page.

d-2hjahx4h = Show the source
    .doc = Shows the source alone.

d-ghqidq45 = Show the page
    .doc = Shows the page alone.

d-iv4urjhq = Problems
    .doc = Shows what the typesetter found, or goes back to the page.

d-qpw3jv76 = Outline
    .doc = Shows the document's headings, or goes back to the page.

d-qwut2qdz = Zoom in
    .doc = Draws the pages a step wider.

d-abulifuu = Zoom out
    .doc = Draws the pages a step narrower.

d-p2dixf6c = Fit the page's width
    .doc = Draws the pages as wide as the room they are shown in.

d-p2dixf6c-width = Width

d-nsttu26n = Turn to a page
    .doc = Turns to a page, so the person comes back to it.

d-nsttu26n-page = Page

d-2jljvrjq = Go to the problem
    .doc = Goes to the place a problem names.

d-2jljvrjq-problem = Problem

d-ftnuj3xb = Go to the heading
    .doc = Goes to the line a heading stands on.

d-ftnuj3xb-heading = Heading

d-pl3ixlbr = Go to
    .doc = Puts the caret at `line`:`column` and shows it, counting each ask so the same place shows again.

d-pl3ixlbr-line = Line

d-pl3ixlbr-column = Column

d-knbolpey = Save the document
    .doc = The document as the file it is saved as, marked as written.
    .did = saved a document

d-ix3mfzzu = Export the PDF
    .doc = The PDF the last typeset wrote, as the file a person saves.

d-uty64k76 = Copy the selected words
    .doc = Copies the words a person selected on a page.

d-uty64k76-page_at = Page at

d-uty64k76-from = From

d-uty64k76-to = To

d-jtqxb525 = Copy the page's words
    .doc = Copies all of a page's words.

d-jtqxb525-page_at = Page at

d-s3ah6rfa = Page text
    .doc = All of a page's words, as the typesetter read them.

d-s3ah6rfa-page_at = Page at

d-cczhygw4 = Selection text
    .doc = The words of a page inside the box from `start` to `end`.

d-cczhygw4-page_at = Page at

d-cczhygw4-start = Start

d-cczhygw4-end = End

d-f2a66q6a = Page words
    .doc = The placed glyphs of one page, as the typesetter read them.

d-f2a66q6a-page_at = Page at

d-gog7heib = Library
    .w-no-typst-documents-yet = No Typst documents yet
    .w-open-one-from-this-device-or = Open one from this device, or start a new one.
    .w-new-document = New document

d-5dt2n5ip = Document row
    .w-not-typeset-yet = Not typeset yet

d-ntb4stnt = Library head
    .w-open = Open
    .w-new-document = New document

d-zapqqrte = Editor
    .w-the-page-shown-is-from-before = The page shown is from before this change
    .w-this-document-does-not-typeset = This document does not typeset

d-32mzmsd6 = Editor head
    .w-open = Open
    .w-save = Save
    .w-export-pdf = Export PDF
    .w-typeset = Typeset
    .w-problems = Problems
    .w-outline = Outline
    .w-the-source-beside-the-page = The source beside the page
    .w-the-source = The source
    .w-the-page = The page
    .w-zoom-out = Zoom out
    .w-zoom-in = Zoom in

d-cuwdvehn = Source pane
    .w-write-a-typst-document = Write a Typst document

d-rn3rpf4r = Status line
    .w-typeset-in-ms = typeset in { $took_ms } ms
    .w-lines = { $len } lines
    .w-pdf-ready = PDF ready

d-rwylofzj = Preview pane
    .w-page-of = Page { $value } of { $len }
    .w-zoom-out = Zoom out
    .w-zoom-in = Zoom in
    .w-fit-width = Fit width
    .w-the-pages-thumbnails = The pages' thumbnails
    .w-typesetting-the-document = Typesetting the document
    .w-this-document-lays-out-no-pages = This document lays out no pages
    .w-pages = Pages
    .w-page = Page { $index }
    .w-page-of-2 = Page { $index } of { $len }
    .w-page-its-words = Page { $index }, its words
    .w-words-selected = Words selected
    .w-copy = Copy
    .w-copy-the-page = Copy the page
    .w-clear = Clear

d-p2hycvhx = Problems panel
    .w-typesetting-the-document = Typesetting the document
    .w-no-problems = No problems
    .w-the-document-typesets-cleanly = The document typesets cleanly.
    .w-line = Line { $line }:{ $column }

d-7loyjosy = Outline panel
    .w-no-headings = No headings
    .w-a-heading-starts-with = A heading starts with `=`.
    .w-line = Line { $line }
