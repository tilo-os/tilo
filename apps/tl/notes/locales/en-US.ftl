# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-lang-notes = Notes

d-mavvhka7 = Note
    .doc = A note: a title, a body, the note it stands under and the tags people gave it.
    .noun = { $count ->
        [one] note
       *[other] notes
    }
    .untitled = Untitled

d-dpj7scfe = Title

d-olsyp3aw = Body

d-pmjerv63 = Images
    .doc = The pictures the note shows, by the address of their content: the name each was taken in as.

d-ssu2tbnf = Parent

d-gfk6rqxp = Pinned

d-xcpasjg7 = Tags

d-erijxfd3 = Source
    .doc = The file the note was imported from, by the address of its content: importing the same file again opens this note rather than making a second one.

d-uquoltho = Origin
    .doc = The name that file had, path inside its archive included: an import that keeps a package's nesting finds the note a page stands under by it.

d-ngwafrjq = By
    .doc = Who wrote the page where it came from, when another service held it before this notebook: kept with the note so a page brought over still says whose words it is.

d-buuvovaf = Service
    .doc = The service that page came from, when it came from one (`Outline`).

d-jznzkj47 = Page
    .doc = Where that page is read there, when it came from one: importing the same page again opens this note rather than making a second one.

d-hchfoqqm = Created
    .doc = When the page was written there, and last changed there, as that service said it.

d-5wdceapz = Updated

d-mavvhka7-filter-pinned = Pinned

d-mavvhka7-filter-tags = Tags

# refined
d-v7cub7aw = Notes
    .doc = The workspace's notes.
    .noun = { $count ->
        [one] notebook
       *[other] notebooks
    }

d-tmqqtkeb = Notes

d-qymo5lg2 = New note
    .doc = Starts a note at the top of the tree, titled `title` and saying `body` (Markdown) when the caller knows them already.

d-qymo5lg2-title = Title

d-qymo5lg2-body = Body

d-37dxigeq = New sub-note
    .doc = Starts a note inside `n`'s own, after the notes it already holds.

d-37dxigeq-n = N

d-qhmjwgei = Pin
    .doc = Pins or unpins a note.
    .w-pin = Pin
    .w-unpin = Unpin

d-qhmjwgei-pinned = Pinned

d-7jhaq3a5 = Move to
    .doc = Moves a note to a place in the tree: at the end of `parent`'s own, or where `before` stands.
    .w-a-note-cannot-go-inside-itself = A note cannot go inside itself

d-7jhaq3a5-moved = Moved

d-7jhaq3a5-parent = Parent

d-7jhaq3a5-before = Before

d-ai4sxgvt = Move to
    .doc = Moves a note to just after `n` and every note under it: the place a drop after a row asks for, which names the row it lands after rather than the row after that one.
    .w-a-note-cannot-go-inside-itself = A note cannot go inside itself

d-ai4sxgvt-moved = Moved

d-ai4sxgvt-n = N

d-c34rzynx = Duplicate
    .doc = Copies a note, with every note under it, just after the one it copies.

d-c34rzynx-n = N

d-jo2tztsx = Sort by title
    .doc = Puts a note's own in the order of their titles, once, for a branch that grew out of order.
    .w-it-holds-no-notes = It holds no notes

d-jo2tztsx-n = N

d-nydyehwn = Delete
    .doc = Deletes a note and every note under it, as one change a person takes back at once.

d-nydyehwn-n = N

d-zvgl2tzt = Rename
    .doc = Renames a note.

d-zvgl2tzt-title = Title

d-6fwsh5ui = Add text
    .doc = Writes `text` (Markdown) at the end of a note, under what it says already: a line, a list or a paragraph added to it.
    .did = added to a note

d-6fwsh5ui-text = Text

d-4yaqj7z5 = Add to a note
    .doc = Writes `text` (Markdown) at the end of the note titled `title`, the case of its letters aside: how an agent or a script that knows a note by its name ("add this to Meeting notes") writes into it without first looking up which note that is.
    .did = added to a note
    .w-no-note-is-titled = No note is titled { $title }

d-4yaqj7z5-title = Title

d-4yaqj7z5-text = Text

d-gc34cbhl = File a note
    .doc = Files a note titled `title` and tagged `tag` in one step: what another app writes down of what it did (Today's morning) without waiting on the note it made to name it.

d-gc34cbhl-title = Title

d-gc34cbhl-tag = Tag

d-7ow26bju = Tag
    .doc = Gives a note a tag.

d-7ow26bju-t = T

d-3u3bevec = Untag
    .doc = Untags a note.

d-3u3bevec-t = T

d-4s5iqnfo = Insert image
    .doc = Puts a picture in a note, where its words end: the person picks the file, and the note keeps the address of its content rather than the picture, and the name it came in as.
    .did = added an image

d-4s5iqnfo-image = Image

d-zfjavdpr = Keep image
    .doc = Keeps a picture the editor wrote into a note where the person pasted or dropped it: the note holds the address of its bytes, and the name it came in as, which an export writes beside it.
    .did = added an image

d-zfjavdpr-picture = Picture

d-xptsjaii = Export markdown
    .doc = A note as Markdown: its title, then its body, each picture it shows written beside it as `<note>/images/<name>`, a path another reader of the file follows and this app reads again as the address of the picture's bytes.

d-2e5dloct = Export note
    .doc = A note as a file: its Markdown, and the pictures it shows beside it.

d-cdjz7d5y = Export notebook
    .doc = The notebook as a zip, one Markdown file for each note: a note's own under the folder of the note it stands in, and the pictures it shows in its own `images` folder beside it.

d-t4nxxtng = Import markdown
    .doc = Imports a Markdown file as a note, or opens the note that file already made: a note keeps the address of the file it came from, so importing the same file twice makes one note.
    .did = imported a note

d-t4nxxtng-file = File

d-n75ltziu = Import a page from a service
    .doc = Imports a page another service held as a note, keeping whose words it is and where it stood there: what `tilo migrate` calls for each page it brings over, and what a person's own import of a file does without a service's words.
    .did = imported a note

d-n75ltziu-file = File

d-n75ltziu-by = By

d-n75ltziu-service = Service

d-n75ltziu-page = Page

d-n75ltziu-created = Created

d-n75ltziu-updated = Updated

d-h3zczsen = Import archive
    .doc = Imports a zip of pages as notes under the notebook, a page under the page that held it, in one change: undo takes the whole import back.
    .did = imported a notebook

d-h3zczsen-file = File

d-hy2uefpy = Import files
    .doc = Files dropped on the notebook: a Markdown file comes in as a note, an archive as the pages it holds — the notebook's own written out, or another app's export — and each file of a drop in turn.
    .did = imported a file

d-hy2uefpy-files = Files

d-ttq2d5ah = File name
    .w-note = Note

d-m6rqg3kf = Pin~present

d-cn43mwee = Move to~present

d-7in2x6ql = Move after~present

d-dimkzymo = Append to note~present

d-akcqjax3 = Notebook

d-x4im3xs5 = Notebook head

d-qrsfr4a5 = Notebook menu
    .w-file = File
    .w-import-markdown = Import Markdown…
    .w-import-a-notion-export-or-a = Import a Notion export or a zip of Markdown…
    .w-export-the-notebook = Export the notebook…
    .w-insert-an-image = Insert an image…
    .w-export-this-note = Export this note…

d-ojfhuiuf = Add child
    .w-new-sub-note = New sub-note

d-5y75taxm = Note node

d-kdl43aw7 = Notes
    .w-no-notes-yet = No notes yet

d-xqxkoqbj = Note row

d-6wqou47w = Note editor

d-osyh3odz = Note page
    .w-write-something = Write something

d-nki6assy = Note details
    .w-from = From { $service }
    .w-from-by = From { $service } by { $by }
    .w-created = Created
    .w-edited = Edited
    .w-1-word = 1 word
    .w-words = { $count } words
