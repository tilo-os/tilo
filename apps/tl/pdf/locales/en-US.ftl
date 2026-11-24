# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-pdf-viewer = PDF Viewer

d-so-tilo-pdf-viewer-keywords = pdf, reader, document, annotate, highlight, sign, form, dokument, lesen, kommentieren, markieren, unterschreiben, formular, pdf-betrachter

d-typz3vku = Document
    .doc = A PDF the workspace reads: the pages it shows, and every mark, comment and signature its readers left on it.
    .noun = { $count ->
        [one] document
       *[other] documents
    }
    .untitled = Untitled document

d-peajfzyh = Source
    .doc = The address of the PDF's bytes, `tilo-blob://…`.

d-mxgrycmo = Source name
    .doc = What the file is called.

d-ebwmtj4p = Trouble
    .doc = Why the pages cannot be shown, in the engine's words; empty while they can.

d-2n6kpf64 = Imported source
    .doc = The source whose own mark-up was taken over, once.

d-zst2moht = Save trouble
    .doc = Why a copy could not be saved, in the engine's words; empty until one is tried.

d-vhujxgcb = Pages

d-zhtkbfeh = Pictures
    .doc = A page drawn at one width, by the address of the picture: what the reader shows.

d-22zxqkpo = Outlines
    .doc = The document's contents, as the engine reads them.

d-oushu5xx = Fields
    .doc = The form fields the file carries, where they stand and what they hold.

d-ilzd6zzn = Marks

d-7zfoarqk = Comments

d-s7stggro = Signatures

d-uwnyzedo = Form text
    .doc = What people filled into the form, by field name.

d-zoztpthf = Form choice

d-2waifcc4 = Form checked

d-kew5sddy = Form export

d-cvhgaqat = Adopted
    .doc = The mark-up the file itself carried, by the id each mark of it was given: the fingerprint it had when it was taken over, so a save keeps an unchanged one as the file's own object.

d-hflhpsk4 = Reading page
    .doc = The page the reader was on, how far down it, and the zoom in percent.

d-au5nrsiz = Reading fraction

d-fg6xtkn7 = Reading zoom

d-44bcedlj = Page
    .doc = One page of a document: which page of which bytes it shows, how big it is and how far it is turned.
    .noun = { $count ->
        [one] page
       *[other] pages
    }

d-oedigi3h = Source page
    .doc = The page of the file it comes from, counted from 1.

d-vphbhfba = Imported
    .doc = The address of the bytes of an imported page; empty for the document's own file.

d-5c4rqnpi = Kind
    .doc = What those bytes are, `pdf` or `image`; empty for the document's own file.

d-qxyy4u3k = Width
    .doc = Its width and height as the file draws it, in points.

d-bepcv7nh = Height

d-vlm7goob = Rotation
    .doc = A quarter turn added to the page's own.

d-fwoikm62 = Words
    .doc = Its words, as the engine reads them; empty until they are read.

d-kdrgc55v = Read
    .doc = Whether its words were read: a page of pictures has been read too, and holds none.

d-bgi4vhjd = Picture
    .doc = A page drawn once at one width, kept as the address of its picture: redrawing every page on every look costs the engine a render each, and a page is the same picture until its turn, its source or the width asked for changes.
    .noun = { $count ->
        [one] picture
       *[other] pictures
    }
    .untitled = Page picture

d-e5fx4rry = Page

d-lps7jwgj = Width
    .doc = The width it was drawn at, in pixels.

d-ht6kgmaq = Url

d-bgi4vhjd-filter-page = Page

d-bgi4vhjd-filter-width = Width

d-dsgjr5bj = Outline
    .doc = One entry of the document's contents, as the engine lists it, flattened.
    .noun = { $count ->
        [one] outline
       *[other] outlines
    }

d-qfml2pla = Title

d-34ukafjp = Page
    .doc = The page it leads to, counted from 1; 0 for an entry that leads nowhere.

d-v7hjvwyk = Level
    .doc = How deep it stands: 0 at the top.

d-dsgjr5bj-filter-page = Page

d-dsgjr5bj-filter-level = Level

d-drvz3law = Field
    .doc = One form field the file carries, where its widget stands on a page and what it holds.
    .noun = { $count ->
        [one] field
       *[other] fields
    }

d-5ghgpzwq = Name

d-4mifdrqw = Kind
    .doc = `text`, `checkbox`, `radio` or `choice`.

d-enpzi5ta = Value

d-f6evcr4f = Checked

d-jjifolfg = Export

d-ya7ugvvf = Options

d-h2kxabyf = Page
    .doc = The page its widget stands on, counted from 1; 0 for one with no widget.

d-ijs3t4gj = Rect
    .doc = Its widget's area in points: x, y, width, height.

d-eh3gs42n = Read only

d-lxh5hh6y = Required

d-drvz3law-filter-checked = Checked

d-drvz3law-filter-page = Page

d-drvz3law-filter-read_only = Read only

d-drvz3law-filter-required = Required

d-w2mt4umt = Mark
    .doc = One mark a reader left on a page.
    .noun = { $count ->
        [one] mark
       *[other] marks
    }
    .untitled = Mark

d-4x7a7juf = Page

d-n23wswru = Kind

d-ikltq7t2 = Colour
    .doc = The colour it is drawn in, `#rrggbb`.

d-35epyc2h = Style
    .doc = What a text mark draws: `highlight`, `underline`, `strikeout` or `squiggly`.

d-qtn6mlbl = Rects
    .doc = The rectangles it covers, `x, y, width, height` one after another, in points of the page as the file draws it: a text mark's lines, a text box's box.

d-crim3nye = X
    .doc = Where a sticky note stands, in points.

d-3rusqrrq = Y

d-agxxncq4 = Words
    .doc = A text box's or a note's words.

d-xt4alsjd = Size
    .doc = A text box's letters, in points.

d-fz2kryor = Strokes
    .doc = The strokes ink or a drawn signature was made of, as points x and y in turn in fractions of the page's box.

d-2a5limjf = Author
    .doc = Who left it, when the file says; empty for a mark made here, whose author is its change.

d-fkkbarxd = Adopted id
    .doc = The file's own mark this was taken over from, by the id the engine names it; empty for a mark made here.

d-w2mt4umt-filter-page = Page

d-w2mt4umt-filter-kind = Kind

d-w2mt4umt-filter-x = X

d-w2mt4umt-filter-y = Y

d-w2mt4umt-filter-size = Size

d-xfqcfhkv = Stroke
    .doc = One stroke of a drawing: its points, x and y in turn.
    .noun = { $count ->
        [one] stroke
       *[other] strokes
    }

d-4dzcakfh = Points

d-rpajrhcr = Comment
    .doc = What people said about a page, or about one of its marks.
    .noun = { $count ->
        [one] comment
       *[other] comments
    }

d-e5qxkctu = Mark
    .doc = The mark it is about; none for one about the page itself.

d-btdnd5pa = Page

d-ogqe2kdm = Words

d-hcfcbfso = Body

d-hgps2s5i = Resolved

d-thfbylkn = Replies

d-rpajrhcr-filter-open = Open

d-rpajrhcr-filter-resolved = Resolved

d-7jxpvur2 = Reply
    .doc = One answer in a comment's conversation.
    .noun = { $count ->
        [one] reply
       *[other] replies
    }

d-47zrerme = Words

d-exvjyq6h = Body

d-wd5gpwkf = Signature
    .doc = A signature placed on a page.
    .noun = { $count ->
        [one] signature
       *[other] signatures
    }

d-xudq4utx = Page

d-uyofemys = X

d-mai2dzre = Y

d-tvp2usjk = Width

d-iriyo77n = Height

d-ozrikra3 = Signer

d-k6r3zcgg = Date
    .doc = The day it was signed, as the signer wrote it.

d-5pbiosiz = Strokes
    .doc = The strokes of a drawn signature, in fractions of its box; none for a typed one, which is the signer's name set in the box.

d-wd5gpwkf-filter-page = Page

d-wd5gpwkf-filter-x = X

d-wd5gpwkf-filter-y = Y

d-wd5gpwkf-filter-width = Width

d-wd5gpwkf-filter-height = Height

d-a5isswfb = Library
    .doc = The workspace's PDFs, and the signature a person reuses on every document they sign.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-bqmqo56y = Documents

d-u2q7brpp = Signer
    .doc = The name a typed signature carries; empty until a person sets one.

d-bny56wzk = Strokes
    .doc = The strokes of a drawn signature, so the next document is signed with one press; empty until a person draws one.

d-p5eyqgoi = Page row
    .doc = A page as the engine answers it: its number and the size it stands at.
    .noun = { $count ->
        [one] page row
       *[other] page rows
    }

d-2xoqlfit = Page

d-hsgsnasu = Width

d-vmsxv3ma = Height

d-p5eyqgoi-filter-page = Page

d-p5eyqgoi-filter-width = Width

d-p5eyqgoi-filter-height = Height

d-tkibkym4 = Glyph row
    .doc = One character of a page's text, in the page's display space.
    .noun = { $count ->
        [one] glyph row
       *[other] glyph rows
    }

d-e35nwhcd = Text

d-owthh7c4 = Line

d-at7kirp3 = X

d-w53k2oqb = Y

d-7ufljz44 = W

d-showgoq3 = H

d-5u27ffde = Placed

d-tkibkym4-filter-line = Line

d-tkibkym4-filter-x = X

d-tkibkym4-filter-y = Y

d-tkibkym4-filter-w = W

d-tkibkym4-filter-h = H

d-tkibkym4-filter-placed = Placed

d-ybwcg2lj = Import row
    .doc = What a file holds as pages.
    .noun = { $count ->
        [one] import row
       *[other] import rows
    }

d-b35hfv2r = Kind

d-m2v5uip7 = Pages

d-u36zis4z = Outline row
    .doc = One entry of a document's contents as the engine lists it.
    .noun = { $count ->
        [one] outline row
       *[other] outline rows
    }

d-rl5gr6nc = Title

d-mtayx6nb = Page

d-ewudh3r2 = Level

d-u36zis4z-filter-page = Page

d-u36zis4z-filter-level = Level

d-rqrdcezf = Mark row
    .doc = One mark the file's own bytes carry.
    .noun = { $count ->
        [one] mark row
       *[other] mark rows
    }

d-s3btzxsu = Page

d-5x3rjw33 = Index

d-l6esysss = Name

d-i3qsks26 = Kind

d-qca4t246 = Color

d-5ddf4ghi = Contents

d-ejgwsaae = Author

d-4chyadse = Created

d-sw7xlda5 = Modified

d-xsmehh7m = Rect

d-v7q3ceeu = Quads

d-pv225oqp = Strokes

d-yrxpeq4o = Size

d-xkarfjs4 = In reply to

d-rwsq3hzc = Reply

d-udw6i6fp = Hidden

d-nuljshqh = Readable

d-rqrdcezf-filter-page = Page

d-rqrdcezf-filter-index = Index

d-rqrdcezf-filter-size = Size

d-rqrdcezf-filter-in_reply_to = In reply to

d-rqrdcezf-filter-reply = Reply

d-rqrdcezf-filter-hidden = Hidden

d-rqrdcezf-filter-readable = Readable

d-dtgx4nex = Markup row
    .doc = The mark-up and the form a file carries.
    .noun = { $count ->
        [one] markup row
       *[other] markup rows
    }

d-6cyewmj4 = Marks

d-dk6bxvq5 = Fields

d-ec3hou66 = Field row
    .doc = One form field of a file.
    .noun = { $count ->
        [one] field row
       *[other] field rows
    }

d-havty2vp = Name

d-d4cnpeq3 = Kind

d-br4tfro7 = Value

d-jyx27y2l = Checked

d-tfvicybt = Export

d-66vdxlss = Options

d-puoybohy = Page

d-ip3ujn6w = Rect

d-mneasolx = Read only

d-i46gksbq = Required

d-ec3hou66-filter-checked = Checked

d-ec3hou66-filter-page = Page

d-ec3hou66-filter-read_only = Read only

d-ec3hou66-filter-required = Required

d-c2khvmu2 = Page out
    .doc = One page of a copy, as a save names it.
    .noun = { $count ->
        [one] page out
       *[other] page outs
    }

d-d56ntmtj = Source

d-5y2litv2 = Page

d-hgb4qbox = Rotation

d-oaf2yyke = Primary

d-c2khvmu2-filter-source = Source

d-c2khvmu2-filter-page = Page

d-c2khvmu2-filter-rotation = Rotation

d-c2khvmu2-filter-primary = Primary

d-aa3m6nmt = Mark out
    .doc = One mark of a copy, in the output page's display space.
    .noun = { $count ->
        [one] mark out
       *[other] mark outs
    }

d-qlypcvpd = Page

d-jfdwouoh = Kind

d-4aptjhj3 = Color

d-5vi7wvna = Author

d-wt44cnbi = Contents

d-cv6deozk = Name

d-uzy5hqqe = Created ms

d-qnpoauzu = Rects

d-ilro56v5 = Strokes

d-slisyq2r = Rect

d-u3zrbin4 = Size

d-aa3m6nmt-filter-page = Page

d-aa3m6nmt-filter-created_ms = Created ms

d-aa3m6nmt-filter-size = Size

d-a5kt32h3 = Reply out
    .doc = One answer written with the annotation it answers.
    .noun = { $count ->
        [one] reply out
       *[other] reply outs
    }

d-7ckypqja = Page

d-ksvfd5se = X

d-g6nqwvtx = Y

d-qs6kxe3w = Author

d-vqovoaba = Body

d-kcbfphy7 = Name

d-qdwom5mm = Answers

d-a5kt32h3-filter-page = Page

d-a5kt32h3-filter-x = X

d-a5kt32h3-filter-y = Y

d-suymio4x = Value out
    .doc = One form value of a copy.
    .noun = { $count ->
        [one] value out
       *[other] value outs
    }

d-4uvljgge = Name

d-tlqdrzox = Kind

d-fp6projh = Text

d-oquhk6ja = On

d-mlfgloi5 = Export

d-suymio4x-filter-on = On

d-h2aiklu2 = Adopted out
    .doc = One mark of the file's own a copy keeps, replaces or removes.
    .noun = { $count ->
        [one] adopted out
       *[other] adopted outs
    }

d-yzggzvtt = Page

d-jxgttint = Index

d-k3tlktwg = Name

d-ua2gidzw = Kind

d-jlsyhy65 = Unchanged

d-h2aiklu2-filter-page = Page

d-h2aiklu2-filter-index = Index

d-h2aiklu2-filter-unchanged = Unchanged

d-j6fkbfbv = Save request
    .doc = Everything a save writes.
    .noun = { $count ->
        [one] save request
       *[other] save requests
    }

d-l5cfhg2u = Pages

d-egrqnnmk = Marks

d-fy7mbgs7 = Replies

d-q2jlnwmi = Values

d-igdolmln = Adopted

d-c7m3hmjb = Mark colour
    .doc = A marker colour, by the name it is chosen by and the colour it draws.
    .noun = { $count ->
        [one] mark colour
       *[other] mark colours
    }

d-yr5hwqqr = Name

d-fmr3zdmh = Value

d-k7x4fj7s = Mark kind
    .doc = What a mark does on a page.

d-p5ikccc5 = Text marked with a highlighter, an underline, a strikethrough or a squiggle
    .doc = Text marked with a highlighter, an underline, a strikethrough or a squiggle.

d-6jhaxxim = Free-hand ink
    .doc = Free-hand ink.

d-dfpdvuf4 = A sticky note
    .doc = A sticky note.

d-3kjya37k = A box of words
    .doc = A box of words.

d-sb3liwex = Open PDF
    .doc = A PDF opens in the reader: Files, the person's own file system and the system's search hand it over as the file it is, and it opens on the document those bytes already are.
    .did = opened a PDF

d-sb3liwex-file = File

d-q6ctwqv7 = Open a PDF from the workspace's content
    .doc = Opens the PDF whose bytes the workspace keeps at `address`: the document those bytes are if one holds them, else a new one.
    .did = opened a PDF
    .w-a-document-is-opened-from-the = A document is opened from the address of its bytes

d-q6ctwqv7-address = Address

d-q6ctwqv7-name = Name

d-svkv5kmu = Lay out the pages
    .doc = Lays a document out on its own bytes: one page for each page of the file, in their order, at the size the file draws it, its contents and its form.
    .did = laid out the pages

d-47sfve6u = Read the contents
    .doc = Reads the document's contents, so the review panel lists them without asking the engine each time it is shown.

d-3lai6j3z = Read the form
    .doc = Reads the document's form fields, so their widgets are drawn over the pages and filled in place.

d-ejxksddn = Read a page's words
    .doc = Reads the page's words once, so the workspace's search finds the page by them.

d-ejxksddn-p = P

d-g5hnjkuf = Draw a page
    .doc = Draws one page as a picture at `width` pixels, kept so a person looking at it again, or another member of the workspace, does not wait for the engine.

d-g5hnjkuf-p = P

d-g5hnjkuf-width = Width

d-hztfw3kc = Read the next page's words
    .doc = Reads the words of the next page nobody read.

d-4sqh76et = Draw the next page
    .doc = Draws the next page nobody drew at the reader's zoom.

d-nlhwr2fh = Rotate page
    .doc = Turns a page a quarter clockwise.

d-nlhwr2fh-p = P

d-73dv3j6a = Move page
    .doc = Puts a page after another one, or first.
    .w-a-page-cannot-go-after-itself = A page cannot go after itself

d-73dv3j6a-p = P

d-73dv3j6a-after = After

d-7ibbaapq = Duplicate page
    .doc = Copies a page right after itself.

d-7ibbaapq-p = P

d-rjejnje3 = Delete page
    .doc = Takes a page out of the document; a document keeps at least one.
    .w-a-document-keeps-at-least-one = A document keeps at least one page

d-rjejnje3-p = P

d-newzwxq5 = Import pages
    .doc = Takes the pages of another PDF or of a picture into the document, after a page (first when none is named).
    .did = imported pages

d-newzwxq5-after = After

d-newzwxq5-file = File

d-ocfsjb5a = Mark text
    .doc = Marks text on a page: a highlight, an underline, a strikethrough or a squiggle over the lines a selection covers, in the reader's colour.
    .did = marked text
    .w-a-mark-covers-something = A mark covers something

d-ocfsjb5a-p = P

d-ocfsjb5a-style = Style

d-ocfsjb5a-colour = Colour

d-ocfsjb5a-rects = Rects

d-kc6nzzkh = Draw
    .doc = Draws ink on a page: one list of points a stroke, its points as x and y in turn in fractions of the page's box.
    .did = drew on a page

d-kc6nzzkh-p = P

d-kc6nzzkh-colour = Colour

d-kc6nzzkh-strokes = Strokes

d-vug2naba = Add note
    .doc = Puts a sticky note on a page where the person pressed, with nothing written in it yet: what it says is written in the conversation it starts.
    .did = added a note

d-vug2naba-p = P

d-vug2naba-x = X

d-vug2naba-y = Y

d-vug2naba-colour = Colour

d-vnlqtooa = Add note
    .doc = Adds a note with these words at the top of a page: for the palette, a script and an agent.
    .did = added a note

d-vnlqtooa-p = P

d-vnlqtooa-words = Words

d-cuzfsube = Add a text box
    .doc = Puts a box of words on a page.
    .did = added a text box

d-cuzfsube-p = P

d-cuzfsube-x = X

d-cuzfsube-y = Y

d-cuzfsube-width = Width

d-cuzfsube-height = Height

d-3g3j2j7f = Edit a mark
    .doc = Changes what a mark says, and its box when one is named.
    .did = edited a mark

d-3g3j2j7f-m = M

d-3g3j2j7f-words = Words

d-3g3j2j7f-rects = Rects

d-ohknsjyv = Move a note
    .doc = Puts a note where the person moved it.
    .did = moved a note

d-ohknsjyv-m = M

d-ohknsjyv-x = X

d-ohknsjyv-y = Y

d-fnztmqkv = Change a mark's colour
    .doc = Gives a mark another colour.
    .did = recoloured a mark

d-fnztmqkv-m = M

d-fnztmqkv-colour = Colour

d-hs5adic7 = Delete mark
    .doc = Takes a mark away, and what was said about it with it.

d-hs5adic7-m = M

d-aoszgtah = Comment
    .doc = Says something about a page, or about one of its marks.
    .did = commented

d-aoszgtah-p = P

d-aoszgtah-m = M

d-aoszgtah-words = Words

d-tvtgg3pl = Reply
    .doc = Answers a comment.
    .did = replied

d-tvtgg3pl-c = C

d-tvtgg3pl-words = Words

d-exeplvmp = Resolve or reopen a comment
    .doc = Marks a conversation as resolved, or opens it again.
    .did = changed a comment's state

d-exeplvmp-c = C

d-exeplvmp-resolved = Resolved

d-4mrvct5b = Edit comment
    .doc = Rewrites what a comment says.
    .did = edited a comment

d-4mrvct5b-c = C

d-4mrvct5b-words = Words

d-ubslpwtp = Delete comment
    .doc = Takes a conversation away; a conversation about a sticky note takes the note with it when it is the last one about it.

d-ubslpwtp-c = C

d-xgbmalca = Delete reply
    .doc = Deletes one answer of a conversation.

d-xgbmalca-c = C

d-xgbmalca-r = R

d-bjtdqwle = Sign as
    .doc = The name typed signatures carry from now on.
    .did = set the signing name

d-bjtdqwle-name = Name

d-bxrvpnea = Draw a signature
    .doc = Keeps the signature a person drew, so the next document is signed with one press: the strokes drawn on the pad, x and y in turn in fractions of the signature's box.
    .did = drew their signature

d-bxrvpnea-strokes = Strokes

d-5it2mdqs = Sign
    .doc = Signs a page where the person pressed: the signature they drew, else their name set in the signature's box.
    .did = signed a page
    .w-type-the-name-to-sign-as = Type the name to sign as, or draw a signature, first

d-5it2mdqs-p = P

d-5it2mdqs-x = X

d-5it2mdqs-y = Y

d-5it2mdqs-width = Width

d-5it2mdqs-height = Height

d-7672bnqp = Sign with name
    .doc = Signs a page with a typed name, in its lower right corner: for the palette, a script and an agent.
    .did = signed a page
    .w-a-signature-names-who-signed = A signature names who signed

d-7672bnqp-p = P

d-7672bnqp-signer = Signer

d-to7kq5we = Delete signature
    .doc = Takes a signature off the page.

d-to7kq5we-s = S

d-agvv6awe = Fill in a field
    .doc = Fills a form field with words.
    .did = filled in the form

d-agvv6awe-name = Name

d-agvv6awe-text = Text

d-l5ttiwdu = Choose a field's option
    .doc = Chooses an option of a form's choice.
    .did = filled in the form

d-l5ttiwdu-name = Name

d-l5ttiwdu-choice = Choice

d-uohk6ns6 = Fill in a checkbox
    .doc = Turns a form's box on or off.
    .did = filled in the form

d-uohk6ns6-name = Name

d-uohk6ns6-on = On

d-uohk6ns6-export = Export

d-t5erm5jf = Remember the reading position
    .doc = Remembers where the reader left off.
    .w-a-reading-position-is-a-share = A reading position is a share of the page

d-t5erm5jf-p = P

d-t5erm5jf-fraction = Fraction

d-t5erm5jf-zoom = Zoom

d-g4qzysfp = Take over the PDF's own mark-up
    .doc = Takes over the mark-up the PDF's own bytes carry: its highlights, notes with their answers, free text and ink become this document's, once per source.
    .did = took over the PDF's mark-up

d-frgxlsls = Take mark
    .doc = Writes one mark the file carried into the document.

d-frgxlsls-page = Page

d-frgxlsls-row = Row

d-frgxlsls-id = Id

d-fuzv2bmi = Save annotated PDF
    .doc = Saves a copy of the PDF with the reader's mark-up in it: its pages in the reader's order, its marks, what was said about them, the form's values and the file's own mark-up kept where nobody changed it.
    .did = saved a copy with its mark-up

d-7662zeux = Turn to a page
    .doc = Remembers that the reader turned to the page at `at`.

d-7662zeux-at = At

d-7662zeux-zoom = Zoom

d-zpwpewsv = Move a page
    .doc = Puts the page at `index` where the rail dropped it: after the page at `at`.
    .did = moved a page

d-zpwpewsv-index = Index

d-zpwpewsv-at = At

d-n5rcgbh4 = Import pages
    .doc = Imports pages after `p`: what the rail's add asks the person for.

d-n5rcgbh4-p = P

d-n5rcgbh4-file = File

d-rrjfjggg = Press a page
    .doc = What pressing on a page does, by the tool the window holds: places a note, a text box or a signature where the person pressed.

d-rrjfjggg-p = P

d-rrjfjggg-at = At

d-rrjfjggg-tool = Tool

d-rrjfjggg-colour = Colour

d-2uq5pptz = Release a page
    .doc = What letting go does: a marking tool marks the box dragged over, and ink keeps the stroke.

d-2uq5pptz-p = P

d-2uq5pptz-tool = Tool

d-2uq5pptz-colour = Colour

d-2uq5pptz-start = Start

d-2uq5pptz-end = End

d-hpdtxwj5 = Comment
    .doc = Says something about the page the window shows.

d-hpdtxwj5-words = Words

d-e5pfej3h = Open source~present

d-4qnr3itw = Move page~present

d-3d6agjuu = Delete page~present

d-gzdwyydw = Mark text~present

d-5i3rvbg2 = Place signature~present

d-xljvuoko = Sign with name~present

d-o3qptln6 = Remember reading~present

d-ihyvwez3 = Library
    .w-no-pdfs-yet = No PDFs yet
    .w-open-one-from-this-device-or = Open one from this device, or from the workspace's files.
    .w-open-pdf = Open PDF

d-agcyt7eg = Document row

d-uh2bcexu = Library head
    .w-open-pdf = Open PDF

d-ibtlwzis = Reader
    .w-this-pdf-cannot-be-opened = This PDF cannot be opened
    .w-laying-out-the-pages = Laying out the pages
    .w-find-in-this-pdf = Find in this PDF
    .w-found = { $hits } found
    .w-previous = Previous
    .w-next = Next
    .w-pages = Pages
    .w-import = Import…
    .w-page = Page { $source_page }
    .w-page-its-marks = Page { $source_page }, its marks
    .w-rotate = Rotate
    .w-more = More
    .w-review = Review
    .w-comments = Comments
    .w-contents = Contents
    .w-signing = Signing
    .w-this-pdf-has-no-contents = This PDF has no contents.
    .w-page-2 = Page { $page }

d-lfkj2ymr = Field row
    .w-choose = Choose

d-zcxne7qg = Comments panel
    .w-nothing-is-said-about-this-pdf = Nothing is said about this PDF yet.
    .w-say-something-about-this-pdf = Say something about this PDF

d-obtjfw3f = Comment card
    .w-a-note = A note
    .w-resolved = Resolved
    .w-more = More
    .w-reopen = Reopen
    .w-resolve = Resolve
    .w-delete = Delete
    .w-page = Page { $source_page }
    .w-reply = Reply

d-ioakkrvn = Signing panel
    .w-sign-as = Sign as
    .w-your-name = Your name
    .w-draw-a-signature = Draw a signature
    .w-sign-here = Sign here
    .w-keep-this-signature = Keep this signature
    .w-clear = Clear
    .w-place-it = Place it
    .w-choose-sign-then-press-the-page = Choose Sign, then press the page where the signature goes.
