# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-notes = Notes

d-so-tilo-notes-keywords = note, memo, memos, notebook, journal, markdown, notepad, textedit, apple notes, google keep, onenote, obsidian, notiz, notizbuch, aufzeichnungen, zettel, merkzettel, tagebuch, notizen machen

d-mavvhka7 = Note
    .doc = A note: a title, a body, the note it stands under, the tags people gave it and the reactions they left on it.
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

d-ddxl7oum = Reactions
    .doc = Who reacted to the note, by emoji: the person and agent each pill in its header stands for, and whose own reaction a press on it takes back.

d-t6igvr56 = Reacted
    .doc = The reactions it had in the native Notes app, each emoji with how many reacted with it: who reacted there is not carried, since this app names people only as its own members.

d-erijxfd3 = Source
    .doc = The file the note was imported from, by the address of its content: importing the same file again opens this note rather than making a second one.

d-uquoltho = Origin
    .doc = The name that file had, path inside its archive included: an import that keeps a package's nesting finds the note a page stands under by it.

d-eo4enwxh = By
    .doc = Who wrote the page where it came from, when another service held it before this notebook: kept with the note so a page brought over still says whose words it is.

d-yzessjhr = Service
    .doc = The service that page came from, when it came from one (`Outline`).

d-zex2vawx = Page
    .doc = Where that page is read there, when it came from one: importing the same page again opens this note rather than making a second one.

d-rnhwkv6z = Created
    .doc = When the page was written there, and last changed there, as that service said it.

d-2od7mwir = Updated

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

d-qgq6mcy7 = Set aside
    .doc = Notes carried over from the native Notes app that it no longer showed: a page whose parent was deleted, kept here rather than dropped, out of the tree.

d-dq2mnb22 = Tags
    .doc = The tags the notebook's notes carry, one a name notes hold by: a note keeps its tags as words, and the notebook keeps the list a person names, renames and deletes them from.

d-qj3f52qw = Tag
    .doc = A tag of the notebook: a name notes carry, listed once so a person reads, renames and deletes it in one place, and every note that carries it follows.
    .noun = { $count ->
        [one] tag
       *[other] tags
    }

d-itadoh74 = Name

d-cvwcei36 = Found
    .doc = One place in a note the words a person looked for stand: the paragraph they are in, counted from the note's first, and its words.
    .noun = { $count ->
        [one] found
       *[other] founds
    }

d-dxxdlopq = At
    .doc = The paragraph, counted from the note's first (1).

d-2vlaqp45 = Words
    .doc = The paragraph's words.

d-cvwcei36-filter-at = At

d-qymo5lg2 = New note
    .doc = Starts a note titled `title` and saying `body` (Markdown) when the caller knows them already.

d-qymo5lg2-title = Title

d-qymo5lg2-body = Body

d-qymo5lg2-parent = Parent

d-qymo5lg2-after = After

d-37dxigeq = New sub-note
    .doc = Starts a note inside `n`'s own, after the notes it already holds, titled `words` when the caller knows them already (a `/page` in one of `n`'s blocks names the page it makes with the words that stood there): the note the window has open too, so a person writing one starts the note it stands under without finding its row.

d-37dxigeq-words = Words

d-qvjedq2v = Carry native notebook
    .doc = The notebook the native Notes app kept before this app took its id over, carried into this one once: each page a note, under the note its page stood under and in the order its pages stood there, with its title, its body, its tags and the id its page had.

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

d-pjxiode5 = Indent
    .doc = Puts a note under the note it stands over, with the notes under it, as `Tab` does in the tree; with none named, it goes under the note above it.
    .w-nothing-over-it-to-move-under = Nothing over it to move under

d-pjxiode5-above = Above

d-bosv7337 = Outdent
    .doc = Puts a note beside the note it stands in, just after it and the notes under it, as `Shift-Tab` does in the tree.
    .w-it-is-at-the-top-level = It is at the top level already

d-gsiqqwwd = Move up
    .doc = Puts a note just over the note above it, with the notes under it, as `Alt-Up` does in the tree; with none named, over the note just above it.
    .w-nothing-over-it-to-move-it = Nothing over it to move it over

d-gsiqqwwd-above = Above

d-qi2ysxan = Move down
    .doc = Puts a note just under the note below it and the notes under that one, with its own notes after it, as `Alt-Down` does in the tree.

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

d-s4ewx4fo = Add to a note
    .doc = Writes `text` (Markdown) at the end of the note titled `title`, the case of its letters aside: how an agent or a script that knows a note by its name ("add this to Meeting notes") writes into it without first looking up which note that is.
    .did = added to a note
    .w-no-note-is-titled = No note is titled { $title }

d-s4ewx4fo-title = Title

d-s4ewx4fo-text = Text

d-gc34cbhl = File a note
    .doc = Files a note titled `title` and tagged `tag` in one step: what another app writes down of what it did (Today's morning) without waiting on the note it made to name it.

d-gc34cbhl-title = Title

d-gc34cbhl-tag = Tag

d-j33acdci = New tag
    .doc = Makes a tag of the notebook named `name`, or gives the one already named that: the name is what people, agents and scripts know a tag by.
    .w-a-tag-needs-a-name = A tag needs a name

d-j33acdci-name = Name

d-7ow26bju = Tag
    .doc = Gives a note the tag named `name`, and lists the tag in the notebook when it lists none of that name yet: a name is how a person, an agent and a script know a tag.
    .w-a-tag-needs-a-name = A tag needs a name

d-7ow26bju-name = Name

d-3u3bevec = Untag
    .doc = Takes the tag named `name` off a note.

d-3u3bevec-name = Name

d-ja7xx6nf = Rename
    .doc = Names a tag of the notebook: every note that carries the old name carries the new one.
    .w-a-tag-needs-a-name = A tag needs a name
    .w-the-notebook-has-that-tag = The notebook has that tag

d-ja7xx6nf-tag = Tag

d-ja7xx6nf-wanted = Wanted

d-6hqlshe7 = Delete
    .doc = Deletes a tag of the notebook: no note carries the name any more, and the notebook stops listing it.

d-6hqlshe7-tag = Tag

d-eec4pye2 = Carry word tags
    .doc = Lists the tags the notebook's notes carry, once each: what a notebook written before the notebook listed its tags needs, and what a start that wrote tags straight onto notes left.

d-3ngyieqa = Find in note
    .doc = Finds the words `text` in one note, the case of their letters aside, and answers each paragraph holding them with its place in the note and its words: what the window's find bar shows, reachable by an agent or the palette that knows the note but not where in it to look.
    .w-type-something-to-find = Type something to find

d-3ngyieqa-text = Text

d-jkqp27g7 = React
    .doc = Adds your reaction with an emoji to a note, or takes it back when you had reacted with it: a press on one of its pills, or the pick of the one its add button opened.

d-jkqp27g7-emoji = Emoji

d-4s5iqnfo = Insert image
    .doc = Puts a picture in a note, where its words end, described by `alt` for a reader who cannot see it: the person picks the file, and the note keeps the address of its content rather than the picture, and the name it came in as.
    .did = added an image

d-4s5iqnfo-image = Image

d-4s5iqnfo-alt = Alt

d-tmlnqj7q = Keep image
    .doc = Keeps a picture the editor wrote into a note where the person pasted or dropped it: the note holds the address of its bytes, and the name it came in as, which an export writes beside it.
    .did = added an image

d-tmlnqj7q-picture = Picture

d-xptsjaii = Export markdown
    .doc = A note as Markdown: its title, then its body, each picture it shows written beside it as `<note>/images/<name>`, a path another reader of the file follows and this app reads again as the address of the picture's bytes.

d-2e5dloct = Export note
    .doc = A note as a file: its Markdown, and the pictures it shows beside it.

d-cdjz7d5y = Export notebook
    .doc = The notebook as a zip, one Markdown file for each note: a note's own under the folder of the note it stands in, and the pictures it shows in its own `images` folder beside it.

d-hjwb45bx = Export notebook markdown
    .doc = The notebook as one Markdown document: each note a heading one level deeper than the note it stands under (six at the deepest), its body beneath, and the notes under it after it — the notebook read as one text, in the order the tree keeps it.

d-t4nxxtng = Import markdown
    .doc = Imports a Markdown file as a note, or opens the note that file already made: a note keeps the address of the file it came from, so importing the same file twice makes one note.
    .did = imported a note

d-t4nxxtng-file = File

d-l23y3cjf = Import a page from a service
    .doc = Imports a page another service held as a note, keeping whose words it is and where it stood there: what `tilo migrate` calls for each page it brings over, and what a person's own import of a file does without a service's words.
    .did = imported a note

d-l23y3cjf-file = File

d-l23y3cjf-by = By

d-l23y3cjf-service = Service

d-l23y3cjf-page = Page

d-l23y3cjf-created = Created

d-l23y3cjf-updated = Updated

d-fxhtpbwf = Import a note as text
    .doc = Imports a note given as words rather than a file: its title, its body as Markdown, the pictures it shows and the notes its links lead to.
    .did = imported a note

d-fxhtpbwf-title = Title

d-fxhtpbwf-markdown = Markdown

d-fxhtpbwf-pictures = Pictures

d-fxhtpbwf-links = Links

d-h3zczsen = Import archive
    .doc = Imports a notebook another tool wrote: a zip of pages, or the JSON a Notion API page listing answers.
    .did = imported a notebook

d-h3zczsen-file = File

d-hy2uefpy = Import files
    .doc = Files dropped on the notebook come in one at a time.
    .did = imported a file

d-hy2uefpy-files = Files

d-ttq2d5ah = File name
    .w-note = Note

d-m6rqg3kf = Pin~present

d-cn43mwee = Move to~present

d-7in2x6ql = Move after~present

d-i653dg7r = Append to note~present

d-ureyllbi = New tag~present

d-mapbyngl = Tag~present

d-2f5tmgbl = Rename tag~present

d-akcqjax3 = Notebook
    .w-outline = Outline
    .w-tags = Tags
    .w-no-tags-yet = No tags yet
    .w-new-tag = New tag
    .w-reacted-in-the-native-notes = Reacted in the native Notes: { $reacted }
    .w-write-something = Write something
    .w-image = image
    .w-page = page

d-aikvtwt5 = Notes tagged
    .w-nothing-carries-yet = Nothing carries { $name } yet

d-x4im3xs5 = Notebook head

d-qrsfr4a5 = Notebook menu
    .w-file = File
    .w-import-markdown = Import Markdown…
    .w-import-a-notion-export-or-a = Import a Notion export or a zip of Markdown…
    .w-export-the-notebook = Export the notebook…
    .w-export-the-notebook-as-one-markdown = Export the notebook as one Markdown document…
    .w-new-sub-note = New sub-note
    .w-insert-an-image = Insert an image…
    .w-export-this-note = Export this note…

d-ojfhuiuf = Add child
    .w-new-sub-note = New sub-note

d-5y75taxm = Note node

d-kdl43aw7 = Notes
    .w-no-notes-yet = No notes yet

d-xqxkoqbj = Note row

d-6wqou47w = Note editor
    .w-reacted-in-the-native-notes = Reacted in the native Notes: { $reacted }
    .w-write-something = Write something
    .w-image = image
    .w-page = page

d-nki6assy = Note details
    .w-created = Created
    .w-moved-here = Moved here
    .w-from = From { $service }
    .w-from-by = From { $service } by { $by }
    .w-edited = Edited
    .w-1-word = 1 word
    .w-words = { $count } words
