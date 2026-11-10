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

d-ssu2tbnf = Parent

d-gfk6rqxp = Pinned

d-xcpasjg7 = Tags

d-erijxfd3 = Source
    .doc = The file the note was imported from, by the address of its content: importing the same file again opens this note rather than making a second one.

d-uquoltho = Origin
    .doc = The name that file had, path inside its archive included: an import that keeps a package's nesting finds the note a page stands under by it.

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
    .doc = Starts a note at the top of the tree.

d-37dxigeq = New sub-note
    .doc = Starts a note inside `n`'s own, after the notes it already holds.

d-qhmjwgei = Pin
    .doc = Pins or unpins a note.

d-7jhaq3a5 = Move to
    .doc = Moves a note to a place in the tree: at the end of `parent`'s own, or where `before` stands.

d-c34rzynx = Duplicate
    .doc = Copies a note, with every note under it, just after the one it copies.

d-jo2tztsx = Sort by title
    .doc = Puts a note's own in the order of their titles, once, for a branch that grew out of order.

d-nydyehwn = Delete
    .doc = Deletes a note and every note under it, as one change a person takes back at once.

d-zvgl2tzt = Rename
    .doc = Renames a note.

d-7ow26bju = Tag
    .doc = Gives a note a tag.

d-3u3bevec = Untag
    .doc = Untags a note.

d-4s5iqnfo = Insert image
    .doc = Puts a picture in a note, where its words end: the person picks the file, and the note keeps the address of its content rather than the picture.
    .did = added an image

d-xptsjaii = Export markdown
    .doc = A note as Markdown: its title, then its body.

d-cdjz7d5y = Export notebook
    .doc = The notebook as one Markdown file, in the tree's order: each note's title a heading one level under the note it stands in.

d-t4nxxtng = Import markdown
    .doc = Imports a Markdown file as a note at the top of the tree, or opens the note that file already made: a note keeps the address of the file it came from, so importing the same file twice makes one note.
    .did = imported a note

d-web4zfz5 = Import notion
    .doc = Imports a Notion export — the zip of a page's Markdown and its pictures — as notes under the notebook, a page under the page that held it, in one change: undo takes the whole import back.
    .did = imported a notebook

d-m6rqg3kf = Pin~present

d-cn43mwee = Move to~present

d-akcqjax3 = Notebook

d-qrsfr4a5 = Notebook menu
    .w-file = File
    .w-import-markdown = Import Markdown…
    .w-import-a-notion-export = Import a Notion export…
    .w-export-the-notebook = Export the notebook…

d-ojfhuiuf = Add child
    .w-new-sub-note = New sub-note

d-5y75taxm = Note node

d-xqxkoqbj = Note row

d-6wqou47w = Note editor

d-osyh3odz = Note page
    .w-write-something = Write something

d-x7x7sbtj = Note menu
    .w-file = File
    .w-insert-an-image = Insert an image…
    .w-export-as-markdown = Export as Markdown…
