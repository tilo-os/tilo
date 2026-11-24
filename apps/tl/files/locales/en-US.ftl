# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-file-explorer = Files

d-so-tilo-file-explorer-keywords = files, file manager, folder, folders, browse, explorer, documents, downloads, trash, dateien, ordner, durchsuchen

d-4pm4hu0z = Entry
    .doc = One entry of a folder: what its disk shows, kept where an action read it, since a view is drawn again from what it is shown and never reads a disk itself.
    .noun = { $count ->
        [one] entry
       *[other] entries
    }

d-lm429k0c = Name
    .doc = What it is called, as its folder shows it.

d-sq7vzl81 = Path
    .doc = The path that reaches it.

d-7x0zd867 = Folder
    .doc = Whether it is a folder.

d-tnbzoc5e = Size
    .doc = Its size in bytes; 0 for a folder.

d-ora1368t = Modified
    .doc = When its content last changed.

d-qttidvz5 = Hidden
    .doc = Whether its name starts with a dot.

d-iqh17pvk = Original
    .doc = Where it stood before it was trashed; empty for one that never was.

d-4pm4hu0z-filter-folder = Folder

d-4pm4hu0z-filter-size = Size

d-4pm4hu0z-filter-modified = Modified

d-4pm4hu0z-filter-hidden = Hidden

d-hy1nsbs0 = Listing
    .doc = A folder as one action read it: its path, what it held and what went wrong, so the window draws a listing a keystroke old at worst and says the reason a folder did not open.
    .noun = { $count ->
        [one] listing
       *[other] listings
    }

d-uzztpcqp = Path
    .doc = The place it lists: a path, or `trash` for this device's trash.

d-9ilrk28j = Entries

d-qwo5qxtp = Error
    .doc = Why it could not be read; empty when it was.

d-yflohkj0 = Read
    .doc = Whether an action has read it at all.

d-fzgm7n2x = Watch
    .doc = The watch this window keeps on the folder, so a change on the disk shows at once.

d-hy1nsbs0-filter-read = Read

d-khab557i = Place
    .doc = A place the sidebar offers, kept as a resource so the system's search finds it by name too.
    .noun = { $count ->
        [one] place
       *[other] places
    }

d-kcy1i0qo = Label
    .doc = What the place is called.

d-z0dyhfw9 = Path
    .doc = Where it goes: a path, `~`, or `trash`, `recent` or `applications`.

d-2v9tcdo2 = Group
    .doc = The heading over it in the sidebar.

d-ogdvbz3p = Favorite
    .doc = A place the person marked in the sidebar.
    .noun = { $count ->
        [one] favorite
       *[other] favorites
    }

d-m5mpzvqt = Place

d-hyjz9xk8 = Recent
    .doc = A file recently opened, so it is one press away again.
    .noun = { $count ->
        [one] recent
       *[other] recents
    }

d-v8w5s5s2 = Name

d-ddeyygde = Path

d-flcdg3ss = Opened

d-hyjz9xk8-filter-opened = Opened

d-rb1nrssu = Session
    .doc = One person's window on this device's files: where they are, how they look, what they marked, what they carry, and the folders this window has read.
    .noun = { $count ->
        [one] session
       *[other] sessions
    }

d-zxu7qmx7 = Here
    .doc = The place shown: a path, `~` for the home folder, or `trash`, `recent` or `applications`.

d-roylouv1 = Sort

d-hm8xkfti = Ascending

d-gkgwx8bk = Hidden
    .doc = Whether names starting with a dot show.

d-31bypi4c = Grid
    .doc = Whether the listing draws as a grid.

d-9cdifavr = Went
    .doc = The places gone before and put away, so back and forward walk the way the person came.

d-4tiugype = Ahead

d-6twkbcct = Carrying
    .doc = What the person copied or cut here, and whether a paste moves it.

d-5y3gacnc = Cutting

d-9psytdam = Listings
    .doc = The folders this window has read.

d-8kzpf3es = Preview
    .doc = The picture a chosen file shows, when it is one, and the text a chosen file holds.

d-omzv6cq5 = Previewed

d-l1bciwln = Words

d-zgzedmq4 = Selected
    .doc = The entry the person chose, by its path; empty while nothing is chosen.

d-kljwn54u = Templates
    .doc = The templates the person keeps in `~/Templates`, offered when a new file is named.

d-uswipaet = Opening
    .doc = The path of the entry the person asked to open with, and the apps that open it.

d-v4v6h4ut = Openers

d-fpb2xkum = Notice
    .doc = The last thing that went wrong, for the window to say; empty while nothing did.

d-rb1nrssu-filter-ascending = Ascending

d-rb1nrssu-filter-hidden = Hidden

d-rb1nrssu-filter-grid = Grid

d-rb1nrssu-filter-cutting = Cutting

d-rbuvwnu3 = Files
    .doc = The workspace's Files: one window per person, the places they marked, and what they opened.
    .noun = { $count ->
        [one] file
       *[other] files
    }

d-7dfrn16h = Sessions

d-fuihvmu6 = Places

d-j9xycg06 = Favorites

d-60ufpbms = Recent

d-5kl9f8yy = Crumb
    .doc = One step of a path, drawn in the bar: what it is called and the path that reaches it.
    .noun = { $count ->
        [one] crumb
       *[other] crumbs
    }

d-lbeg1rhd = Label

d-sv3ucdrd = Path

d-pllo08gs = Sort by
    .doc = How a folder's entries read.

d-0rya7m08 = By what they are called
    .doc = By what they are called.

d-ba832r20 = By how much they hold
    .doc = By how much they hold.

d-3ce1vizu = By when they last changed
    .doc = By when they last changed.

d-czgp48en = By the kind of file they are
    .doc = By the kind of file they are.

d-hcs5ia4f = Standard places
    .doc = A new Files starts with the places its sidebar offers.

d-dr8h4hvl = Arrive
    .doc = Opening Files reads the folder the person is in, unless this window has read it already.

d-yvcgroaj = Read listing
    .doc = Reads the place `place` into this window's listing for it: a folder's entries, this device's trash, or nothing for the places this app keeps itself.

d-yvcgroaj-place = Place

d-q3fr2sid = Changed
    .doc = Runs when a watched folder changes: reads the folder shown again.

d-mrkltxcn = Go
    .doc = Goes to the place `place`, reading it: a folder, the trash, the recent files or the apps.
    .w-a-place-is-named-by-its = A place is named by its path

d-mrkltxcn-place = Place

d-cj9hqb9z = Back
    .doc = Goes back to the place the person came from.
    .w-there-is-nowhere-to-go-back = There is nowhere to go back to

d-z87yr3dn = Forward
    .doc = Goes forward again to the place the person left.
    .w-there-is-nowhere-to-go-forward = There is nowhere to go forward to

d-52nrdgom = Up
    .doc = Goes to the folder above the one shown.
    .w-this-is-already-the-top-folder = This is already the top folder

d-mam33h3b = Send nearby
    .doc = Sends the entry the person chose to the nearby device `nearby`, as Nearby Send offers it.
    .w-choose-a-file-to-send-first = Choose a file to send first

d-mam33h3b-nearby = Nearby

d-v7sec4gx = Dismiss notice
    .doc = Dismisses what the window last said went wrong.

d-qix4firi = Refresh
    .doc = Reads the place shown again, so what changed on the disk shows.

d-poz095x9 = Sort by
    .doc = Sorts the folder shown by `by`, the way `ascending` says.

d-poz095x9-by = By

d-bnbnc8s7 = Ascending
    .doc = Sorts the folder shown oldest or smallest first, or the other way.

d-bnbnc8s7-first = First

d-4a6l65gk = Show grid
    .doc = Draws the folder as a grid of cards, or as rows.

d-4a6l65gk-grid = Grid

d-jxhgmocg = Choose view
    .doc = Draws the folder the way the segment at place `i` says: the list at 0, the grid at 1.

d-jxhgmocg-i = I

d-6rs9uy5i = Show hidden files
    .doc = Shows or hides the names that start with a dot.

d-6rs9uy5i-hidden = Hidden

d-7di40kuf = Open entry
    .doc = Opens the entry `e`: a folder in this window, a file in the app that opens its kind.

d-7di40kuf-e = E

d-4j3db57u = Find openers
    .doc = Finds the apps that open the entry at `path`, for the person to choose one.

d-4j3db57u-path = Path

d-al4xcvb7 = Open with
    .doc = Opens the entry the person asked to open with in `chosen`, one of the apps offered.

d-al4xcvb7-chosen = Chosen

d-msrzddf6 = Close openers
    .doc = Puts away the apps offered for the entry the person asked to open with.

d-hdabwwa2 = Open terminal
    .doc = Opens a terminal in the folder at `folder`.

d-hdabwwa2-folder = Folder

d-vzs2pkap = Compress entry
    .doc = Packs the entry at `path` into a new archive beside it, named after it: `zip` picks a ZIP, and TAR.ZST otherwise.

d-vzs2pkap-path = Path

d-vzs2pkap-zip = Zip

d-bjqwycd6 = Extract entry
    .doc = Extracts the archive at `path` into a new folder beside it, named after it.

d-bjqwycd6-path = Path

d-f2p3xu5w = Open recent
    .doc = Opens a file from Recent again, as the entry it was.

d-f2p3xu5w-r = R

d-0ibyyurl = Touch entry
    .doc = A press on an entry: a folder opens as a person presses it once, and a file is read into the inspector beside the list.

d-0ibyyurl-e = E

d-3nuuco40 = Note recent
    .doc = Notes that the file `path` was opened, so it stands under Recent.

d-3nuuco40-name = Name

d-3nuuco40-path = Path

d-tjvt3ycq = New folder
    .doc = Makes a folder named `name` in the place shown.

d-tjvt3ycq-name = Name

d-2p5d6k2y = Start new file
    .doc = Reads the person's `~/Templates`, so naming a new file can offer them.

d-xc7thuvo = New from template
    .doc = Copies the template `template` into the place shown under `typed`'s name, or the template's.

d-xc7thuvo-template = Template

d-xc7thuvo-typed = Typed

d-o4nc1mt2 = New file
    .doc = Makes a text file named `name` in the place shown, for the person to fill.

d-o4nc1mt2-name = Name

d-2b6epu9v = Rename entry
    .doc = Renames the entry at `path` to `name` in its own folder.
    .w-a-name-cannot-be-empty = A name cannot be empty

d-2b6epu9v-path = Path

d-2b6epu9v-name = Name

d-8anojqje = Move to Trash
    .doc = Moves the entries at `paths` to this device's trash.
    .w-select-files-first = Select files first

d-8anojqje-paths = Paths

d-qi6xdvcy = Restore
    .doc = Restores the trashed entry `name` to where it came from.

d-qi6xdvcy-name = Name

d-s1wqgtx8 = Delete permanently
    .doc = Removes the entry at `path` for good.

d-s1wqgtx8-path = Path

d-crjdf1pc = Empty Trash
    .doc = Empties this device's trash: every entry, and the receipt that would put it back.

d-tfg0v0s6 = Carry
    .doc = Remembers the entries at `paths` as what a paste copies; `cut` makes it a move.

d-tfg0v0s6-paths = Paths

d-tfg0v0s6-cut = Cut

d-241p3hv1 = Paste
    .doc = Pastes what the person carries into the place shown.

d-q2qhukz6 = Key on
    .doc = A key pressed on the row `focused` while the entries `paths` are chosen: what Files does with a copy, a cut or a delete.

d-q2qhukz6-paths = Paths

d-q2qhukz6-focused = Focused

d-q2qhukz6-key = Key

d-a5nwzp2a = Add to Favorites
    .doc = Marks the place `place` in the sidebar.
    .w-it-already-stands-in-favorites = It already stands in Favorites

d-a5nwzp2a-place = Place

d-r9tivfaz = Remove from Favorites
    .doc = Takes the favorite `favorite` out of the sidebar.

d-r9tivfaz-favorite = Favorite

d-0nodt7ql = Inspect
    .doc = Reads what the chosen entry is, so the inspector says its size, its kind and its path, and shows a picture file's picture or a text file's first words.

d-0nodt7ql-e = E

d-86n0vq62 = Name it
    .doc = Does what the name asked of the person is for: makes the folder or the file, or renames the entry.

d-86n0vq62-naming = Naming

d-86n0vq62-typed = Typed

d-npbyxhoy = Go~present

d-tnjafyxk = Back~present

d-kkf4qwdb = Forward~present

d-nuftacw2 = Up~present

d-drltsrm6 = Send nearby~present

d-ed3fdxi3 = Rename entry~present

d-x6q5wlkw = Trash entries~present

d-dkeycb6f = Add favorite~present

d-n9jkqy4o = Files
    .w-add-the-folder-shown-to-favorites = Add the folder shown to Favorites
    .w-new-folder = New folder
    .w-how-the-folder-shows = How the folder shows
    .w-list = List
    .w-grid = Grid
    .w-sort = Sort
    .w-sort-by-name = Sort by name
    .w-sort-by-size = Sort by size
    .w-sort-by-kind = Sort by kind
    .w-sort-by-date = Sort by date
    .w-ascending = Ascending
    .w-descending = Descending
    .w-view-options = View options
    .w-hide-hidden-files = Hide hidden files
    .w-show-hidden-files = Show hidden files
    .w-new-file = New file
    .w-refresh = Refresh
    .w-copy = Copy
    .w-cut = Cut
    .w-paste = Paste
    .w-move-to-trash = Move to Trash
    .w-empty-trash = Empty Trash
    .w-type-a-path-e-g-documents = Type a path, e.g. ~/Documents
    .w-back = Back
    .w-forward = Forward
    .w-up-one-folder = Up one folder
    .w-home = Home
    .w-dismiss = Dismiss
    .w-name-of-the-new-folder = Name of the new folder
    .w-name-of-the-new-file = Name of the new file
    .w-new-name = New name
    .w-from-a-template = From a template
    .w-no-apps-yet = No apps yet
    .w-the-apps-on-this-device-stand = The apps on this device stand here once one is installed.
    .w-couldn-t-open = Couldn’t open “{ $name_of }”
    .w-try-again = Try again
    .w-reading = Reading “{ $name_of }”…
    .w-this-folder-is-empty = This folder is empty
    .w-nothing-here-matches = Nothing here matches “{ $found }”
    .w-files = Files
    .w-ctrl-c = Ctrl+C
    .w-ctrl-x = Ctrl+X
    .w-delete = Delete
    .w-open = Open
    .w-get-info = Get info
    .w-restore = Restore
    .w-delete-permanently = Delete permanently
    .w-open-in-terminal = Open in Terminal
    .w-extract = Extract
    .w-open-with = Open with…
    .w-compress-to-zip = Compress to ZIP
    .w-compress-to-tar-zst = Compress to TAR.ZST
    .w-rename = Rename
    .w-files-is-starting = Files is starting…

d-z6cheebf = Places
    .w-places = Places
    .w-workspace = Workspace
    .w-favorites = Favorites
    .w-folders = Folders
    .w-this-computer = This computer
    .w-nearby = Nearby

d-4faq5fwx = Openers row
    .w-open-with = Open “{ $name_of }” with
    .w-cancel = Cancel

d-jwdlcu14 = Entry grid
    .w-files = Files

d-kovpbjhk = Entry row

d-wsk8uwkt = Recent files
    .w-no-recent-files-yet = No recent files yet
    .w-files-you-open-stand-here-one = Files you open stand here, one press away.
    .w-recent-files = Recent files

d-qfj1tkgq = Inspected
    .w-type = Type
    .w-size = Size
    .w-modified = Modified
    .w-location = Location
    .w-came-from = Came from
    .w-select-a-file-to-see-what = Select a file to see what it is.
