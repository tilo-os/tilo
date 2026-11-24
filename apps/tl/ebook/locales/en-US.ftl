# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-ebook = Books

d-so-tilo-ebook-keywords = book, books, ebook, ebooks, epub, read, reading, reader, novel, library, highlights, annotations, buch, buecher, bücher, lesen, roman, bibliothek

d-defhqkyd = Book
    .doc = A book on the shelf: what its package says about it, where the person is in it, its chapters in reading order, and what they marked in it.
    .noun = { $count ->
        [one] book
       *[other] books
    }
    .untitled = Untitled book

d-zzi6jbdk = Title

d-hzsemqyf = Author

d-ywdncocc = Subject

d-nczhbvr6 = Language
    .doc = The language the book is written in, for hyphenation.

d-uufjkgr3 = Source
    .doc = The address of the file's content: opening the same file again opens this book.

d-t7fdn6od = Cover
    .doc = The address of its cover's picture, when the package carries one.

d-sthxrdo4 = Added

d-s4t4ogtx = Chapters

d-kckryjj5 = Marks

d-y3qlwotc = Progress
    .doc = Where each member is in the book: one record per reader, changed as they read and kept with its history, so what a person read follows them across their devices and no one else's reading shows in theirs.

d-defhqkyd-filter-added = Added

d-ra6ct5pp = Chapter
    .doc = One chapter of a book: what the contents call it, and its words.
    .noun = { $count ->
        [one] chapter
       *[other] chapters
    }
    .untitled = Chapter

d-njsapppc = Title

d-hc6oa77y = Body

d-awonaulm = Book
    .doc = The book it is a chapter of: a search that finds a passage opens it with the book a step away.

d-ra6ct5pp-filter-book = Book

d-ujgcntpf = Mark
    .doc = A passage a person marked, as the anchor a page shows it at: the block it is in, its first and last character in that block's words, and the words themselves.
    .noun = { $count ->
        [one] mark
       *[other] marks
    }

d-nsx4szpu = Kind
    .doc = `bookmark`, `highlight`, `note` or `comment`.

d-eu36rgok = Chapter

d-kkwkq4sx = Block

d-c7gbrwjv = Start

d-gwydxwhz = End

d-pjwr2noa = Excerpt

d-jlprlqyv = Color

d-q6o2tid6 = Text
    .doc = What a note or a comment says.

d-qzbrg6g6 = Created

d-jt22d5yp = On
    .doc = The highlight a comment is about, by its id.

d-gjbrlgkj = Book
    .doc = The book the mark is in: a page of it stands one step from the passage.

d-vpgm2esg = Pieces
    .doc = The ranges the passage covers, in reading order, each `block, start, end` in threes: one block's words, or several when it crosses a block.

d-ujgcntpf-filter-highlights = Highlights

d-ujgcntpf-filter-notes = Notes

d-ujgcntpf-filter-bookmarks = Bookmarks

d-kfdvrrjc = Progress
    .doc = Where one member is in a book.
    .noun = { $count ->
        [one] progress
       *[other] progresses
    }

d-fw6cx373 = Who
    .doc = The reader this record is: each member's is their own.

d-hrlsiabz = Chapter
    .doc = The chapter open, counted from 0, the block its page begins at, the page in it, and how far down the column the continuous mode stands (`0` its top to `1` its foot).

d-dozgglst = Block

d-j7r2dpph = Page

d-t4pdkbb6 = Place

d-m5vjvjxn = Finished
    .doc = Whether they finished the book: what the shelf leaves out when it hides finished books.

d-vpcehkho = At
    .doc = When they last read on.

d-kfdvrrjc-filter-who = Who

d-kfdvrrjc-filter-chapter = Chapter

d-kfdvrrjc-filter-block = Block

d-kfdvrrjc-filter-page = Page

d-kfdvrrjc-filter-place = Place

d-kfdvrrjc-filter-finished = Finished

d-kfdvrrjc-filter-at = At

d-gtq4jwzo = Scheme
    .doc = A page palette a reader can pick: its paper, its ink and the colour a selection takes.
    .noun = { $count ->
        [one] scheme
       *[other] schemes
    }

d-yjaxgs5w = Name

d-zfe44ecr = Page

d-giw4wwwc = Ink

d-p6cg5shx = Accent

d-2xhwxzlq = Builtin
    .doc = A palette the reader ships with: the settings keep it and offer no way to delete it.

d-gtq4jwzo-filter-builtin = Builtin

d-5zqlbdnq = Found
    .doc = A book Project Gutenberg answered a search with, waiting to be downloaded.
    .noun = { $count ->
        [one] found
       *[other] founds
    }

d-cuehsj4a = Title

d-5z23zsgs = Author

d-5kd3qulh = Url
    .doc = The address its EPUB reads at.

d-rgyfsse6 = Language

d-fhfwqqv3 = Query
    .doc = The words it was found for, so a new search takes the old answers away.

d-in25mwb6 = Watched
    .doc = A folder on a device the library watches: every EPUB in it, now or later, becomes a book.
    .noun = { $count ->
        [one] watched
       *[other] watcheds
    }

d-zfzcnyj7 = Path

d-l4rfzd5q = Token
    .doc = The watch this device keeps, so a change to the folder runs the library's scan.

d-vs5vpxpc = Given
    .doc = The paths it already gave books for, so a scan reads only what is new.

d-dafaqxgm = Added

d-in25mwb6-filter-added = Added

d-srrcatfl = Library
    .doc = The workspace's library: the books on the shelf, the books an online search found, the page palettes, and how a page is set for the people reading here.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-rjqqjeka = Books

d-gark455v = Found

d-b2rhddat = Schemes

d-sndaeoib = Text size
    .doc = The text's size as a multiple of the reading base, between 0.8 and 2.

d-3fnv4dcf = Line spacing
    .doc = The line spacing as a multiple of the face's own, between 0.95 and 1.6.

d-5umaklng = Face
    .doc = The face the page is set in.

d-4nrbbruw = Justified
    .doc = Whether paragraphs are justified like a book's rather than set ragged.

d-q2xeecyu = Scheme
    .doc = The palette chosen, by name; the sepia page where it names none.

d-7iepiv5z = Draft scheme
    .doc = The name a palette of the person's own is made under, and its paper and ink colours.

d-wc5z47pj = Custom page

d-yiozk22e = Custom ink

d-dww4lfs6 = Draft folder
    .doc = The folder the settings' field names: what "Watch it" watches.

d-trpwrvrr = Folders
    .doc = The folders this device watches for books.

d-rmk5y2dl = Sort by
    .doc = How the shelf orders its books: `title`, `author` or `added`, the last added first.

d-iw7jv3y3 = Hide finished
    .doc = Whether the shelf leaves out the books a person finished.

d-srrcatfl-filter-text_size = Text size

d-srrcatfl-filter-line_spacing = Line spacing

d-srrcatfl-filter-justified = Justified

d-srrcatfl-filter-hide_finished = Hide finished

d-jya2xpym = Highlight
    .doc = A highlight colour, by the name it is chosen by and the colour it paints.
    .noun = { $count ->
        [one] highlight
       *[other] highlights
    }

d-2mud3bws = Name

d-ztqdbzwg = Value

d-4xgt6ef3 = Fill
    .doc = What a new library starts with: the page palettes every reader knows, and the sepia page chosen.

d-euyhlcqi = Open book
    .doc = Adds the EPUB `file` to the shelf and opens it.
    .did = added a book

d-euyhlcqi-file = File

d-umxlbp4p = Drop books
    .doc = Adds the EPUB files a person dropped on the shelf, each as `open_book` adds it.

d-umxlbp4p-files = Files

d-vmft5yuq = Watch folder
    .doc = Watches the folder `path` for books: each EPUB it holds, and each that arrives, becomes a book, keeping the person's place as one they opened themselves does.
    .did = watched a folder for books
    .w-a-folder-is-watched-by-its = A folder is watched by its name

d-vmft5yuq-path = Path

d-ap57yp5t = Scan folder
    .doc = Reads the folder `path`'s EPUBs, giving a book to each file the library does not hold yet.

d-ap57yp5t-path = Path

d-3gvqo77t = Foldered
    .doc = Reads every watched folder again: what the platform runs when one of them changes.

d-mjbtpsjr = Remove book
    .doc = Takes a book off the shelf, its chapters and marks with it.
    .did = removed a book

d-xiofub57 = Record at
    .doc = Where the person reading is in `b`: their own record, changed in place so its history says where they read and when.

d-xiofub57-who = Who

d-xiofub57-chapter = Chapter

d-xiofub57-block = Block

d-xiofub57-page = Page

d-xiofub57-place = Place

d-liwol635 = Read chapter
    .doc = Opens `c` in `b`: the contents and where the person is are kept together.

d-liwol635-who = Who

d-liwol635-c = C

d-5vlcfzhe = Show page
    .doc = Shows page `page` of the book's open chapter, counted from 0 (a spread in the two-page mode).

d-5vlcfzhe-who = Who

d-5vlcfzhe-chapter = Chapter

d-5vlcfzhe-block = Block

d-5vlcfzhe-page = Page

d-hx37iaus = Show passage
    .doc = Shows the first page of the chapter `chapter` in `b`: a link to a passage lands on it.
    .w-a-passage-stands-at-a-block = A passage stands at a block

d-hx37iaus-who = Who

d-hx37iaus-chapter = Chapter

d-hx37iaus-block = Block

d-algei7uu = Show place
    .doc = How far down the column the person scrolled.

d-algei7uu-who = Who

d-algei7uu-chapter = Chapter

d-algei7uu-block = Block

d-algei7uu-page = Page

d-algei7uu-place = Place

d-o6ubqh53 = Saw blocks
    .doc = Notes the block the page begins at as the person scrolls the column, so a bookmark lands where they are.

d-o6ubqh53-who = Who

d-o6ubqh53-blocks = Blocks

d-oyxehe6h = Set text size
    .doc = Sets the page's text size, within what a page can be read at.
    .did = changed the text size

d-oyxehe6h-size = Size

d-x6jkfidp = Set line spacing
    .doc = Sets the page's line spacing.
    .did = changed the line spacing

d-x6jkfidp-spacing = Spacing

d-x5djz5gg = Set face
    .doc = Sets the face the page is set in.
    .did = chose the reading face

d-x5djz5gg-face = Face

d-wfzyizr4 = Set justified
    .doc = Whether the page's paragraphs are justified.
    .did = changed the paragraph fitting

d-wfzyizr4-on = On

d-qazz5dbn = Set shelf sort
    .doc = Orders the shelf by title, by author, or by the books added last.
    .did = sorted the shelf
    .w-a-shelf-is-sorted-by-title = A shelf is sorted by title, author or added

d-qazz5dbn-order = Order

d-7c3mgmnh = Set hide finished
    .doc = Leaves the books a person finished off the shelf, or shows them again.
    .did = changed whether finished books show

d-7c3mgmnh-hidden = Hidden

d-4wmsw3oj = Set finished
    .doc = Marks `b` finished for the person reading it, or starts it again.
    .did = changed whether a book is finished

d-4wmsw3oj-who = Who

d-4wmsw3oj-finished = Finished

d-iex2qhcv = Cycle scheme
    .doc = Steps to the next palette the reader ships with: its bar's quick switch.
    .did = changed the page palette

d-uuny6qzl = Set scheme
    .doc = The palette the page is set in, by name.
    .did = chose the page palette

d-uuny6qzl-name = Name

d-msgz7llr = Add scheme
    .doc = A palette of the person's own, from a paper and an ink colour.
    .did = made a page palette
    .w-a-palette-is-made-under-a = A palette is made under a name

d-msgz7llr-name = Name

d-msgz7llr-page = Page

d-msgz7llr-ink = Ink

d-g5klupmx = Delete scheme
    .doc = Deletes a palette of the person's own.
    .did = deleted a page palette
    .w-the-reader-s-own-palettes-stay = The reader's own palettes stay

d-g5klupmx-scheme = Scheme

d-km4hezzj = Highlight
    .doc = Highlights the passage of `b`'s open chapter: its ranges, each `block, start, end`.
    .did = highlighted a passage

d-km4hezzj-chapter = Chapter

d-km4hezzj-ranges = Ranges

d-km4hezzj-color = Color

d-l2my2cwb = Add note
    .doc = Writes a note on the passage of `b`'s open chapter.
    .did = wrote a note on a passage

d-l2my2cwb-chapter = Chapter

d-l2my2cwb-ranges = Ranges

d-l2my2cwb-color = Color

d-l2my2cwb-text = Text

d-nvonvbnv = Toggle bookmark
    .doc = Bookmarks where the page `block` begins in `b`'s open chapter, or takes the bookmark off again.
    .did = bookmarked a page, or took its bookmark off

d-nvonvbnv-chapter = Chapter

d-nvonvbnv-block = Block

d-y7xiu6cu = Comment
    .doc = Comments on the highlight `m`, in the book `b`.
    .did = commented on a highlight

d-y7xiu6cu-m = M

d-y7xiu6cu-text = Text

d-kyszv557 = Change color
    .doc = Recolours a highlight, and the notes on it.
    .did = recoloured a highlight

d-kyszv557-m = M

d-kyszv557-color = Color

d-rhst3sds = Edit a note
    .doc = Rewrites what a note or a comment says.
    .w-only-its-author-can-edit-a = Only its author can edit a note
    .w-write-a-note-first = Write a note first

d-rhst3sds-m = M

d-rhst3sds-text = Text

d-t53zqwaz = Copy the passage
    .doc = Copies a passage, so it can be pasted anywhere: what a selection's Copy does.

d-t53zqwaz-text = Text

d-3c4owmt6 = Delete mark
    .doc = Deletes a mark, and the comments on it.
    .did = deleted a mark

d-3c4owmt6-m = M

d-enyvtx2q = Find books
    .doc = Searches Project Gutenberg for books matching `query`, taking the answers an earlier search left behind.
    .did = searched for books

d-enyvtx2q-query = Query

d-sq35zdfy = Download found
    .doc = Downloads the book an online search found and opens it: the window opens the book the action returns, as it opens what any primary action makes.
    .did = downloaded a book

d-sq35zdfy-f = F

d-ob6iyicm = Clear found
    .doc = Takes an online search's answers away.
    .did = cleared the books found online

d-oxd7kaud = Watch folder~present

d-w4cgekcm = Show passage~present

d-om3e7iv4 = Set shelf sort~present

d-vwfvgnvc = Add scheme~present

d-f2pjguwz = Delete scheme~present

d-7n672gfq = Edit mark text~present

d-yseykhty = Library
    .w-books = Books
    .w-add-a-book = Add a book
    .w-sort = Sort
    .w-by-title = By title
    .w-by-author = By author
    .w-added-last = Added last
    .w-hide-finished = Hide finished
    .w-what-the-library-shows = What the library shows
    .w-shelf = Shelf
    .w-settings = Settings
    .w-found-online = Found online
    .w-no-books-yet = No books yet
    .w-open-an-epub-file-and-it = Open an EPUB file and it stands here, keeping your place as you read.

d-cmr5ibgv = Library settings
    .w-reading-settings = Reading settings
    .w-text = Text
    .w-text-size = Text size
    .w-line-spacing = Line spacing
    .w-face = Face
    .w-the-reader-s-serif = The reader's serif
    .w-justified-paragraphs = Justified paragraphs
    .w-watched-folders = Watched folders
    .w-new-epubs-here-become-books = New EPUBs here become books
    .w-a-folder-on-this-device = A folder on this device
    .w-watch-it = Watch it
    .w-page-palettes = Page palettes
    .w-paper-ink = Paper { $page }, ink { $ink }
    .w-delete = Delete { $name }
    .w-palette-name = Palette name
    .w-paper = Paper
    .w-ink = Ink
    .w-use-these-colours = Use these colours

d-jnthvtdd = Found books
    .w-find-a-book-online = Find a book online
    .w-title-author-or-subject = Title, author or subject
    .w-search = Search
    .w-clear = Clear
    .w-nothing-found-yet = Nothing found yet
    .w-search-project-gutenberg-for-a-free = Search Project Gutenberg for a free book; it stands here until you download it.

d-tntic6tx = Book page
    .w-contents = Contents
    .w-justified = Justified
    .w-reading-settings = Reading settings
    .w-bookmarks = Bookmarks
    .w-chapter = Chapter { $value }
    .w-notes = Notes
    .w-highlight = Highlight
    .w-copy = Copy
    .w-note = Note
    .w-bookmark = Bookmark
    .w-dismiss = Dismiss
    .w-write-a-note-on-the-passage = Write a note on the passage
    .w-save-note = Save note
    .w-previous = Previous
    .w-page-of = Page { $value } of { $pages }
    .w-next = Next
    .w-smaller-text = Smaller text
    .w-larger-text = Larger text
    .w-change-the-page-palette = Change the page palette
    .w-pages = Pages
    .w-one-page = One page
    .w-two-pages = Two pages
    .w-scrolled = Scrolled
    .w-bookmark-this-page = Bookmark this page
    .w-finished = Finished
    .w-how-far-through-this-chapter = How far through this chapter
    .w-this-chapter-s-marks = This chapter's marks
    .w-comment = Comment
    .w-edit = Edit
    .w-delete = Delete
    .w-edit-the-note = Edit the note
    .w-save = Save
    .w-cancel = Cancel
    .w-comment-on-the-highlight = Comment on the highlight
    .w-send-comment = Send comment

d-uxr7ymrf = Chapter page
    .w-read-in = Read in { $title }
    .w-continuous = continuous

d-qzus6seo = Mark page
    .w-a-bookmark = A bookmark
    .w-chapter = Chapter { $value }
    .w-show-it-in = Show it in { $title }

d-hjpohooo = Book row

d-4dzykgkv = Mark row
