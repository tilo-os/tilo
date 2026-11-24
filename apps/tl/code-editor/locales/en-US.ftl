# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-code-editor = Code Editor

d-so-tilo-code-editor-keywords = code editor, code, editor, source, ide, programming editor, code editor online, notepad, text editor, quelltext, code-editor, editor, programmieren, entwickeln

d-m2evo7on = Entry
    .doc = One file or folder of the project: its name, where it stands, the language it is written in, and — a file's — its text.
    .noun = { $count ->
        [one] entry
       *[other] entries
    }
    .untitled = untitled

d-jr3jrrmx = Kind

d-wlnikenq = Name

d-otwgp44c = Parent
    .doc = The folder it stands in; none at the top of the project.

d-fav4hm4j = Language
    .doc = The language it is coloured, checked and kept as, by the language's own name (`Rust`, `Plain Text`); empty for a folder.

d-zbluopai = Text
    .doc = The file's text.

d-n6s6du45 = Disk
    .doc = Where the file stands on this device; empty for a file saved nowhere yet.

d-lkyxb2f7 = Stamp
    .doc = The modified time of the bytes this project last read or wrote, in nanoseconds; 0 when the file is nowhere on this device.

d-wxaqrw5b = Stamp len
    .doc = The length of the bytes the project last read or wrote.

d-bvn4nnf5 = Saved
    .doc = The text as it stands on the device, so a reader sees what changed here and what is written already.

d-uhopusbk = Stale
    .doc = The file changed on the device since it was read here: saving over it asks first.

d-o3gbo56k = Problems
    .doc = What the last check of this file found.

d-jwc6eef6 = Checked
    .doc = What that check read: a file whose text no longer hashes to this has changed since.

d-jwkhracm = Source
    .doc = The checker that answered: the server's name, or `lint` while only the editor's own checks have run; empty before the file was checked.

d-m2evo7on-filter-kind = Kind

d-m2evo7on-filter-parent = Parent

d-m2evo7on-filter-stamp = Stamp

d-m2evo7on-filter-stamp_len = Stamp len

d-m2evo7on-filter-stale = Stale

d-vxqkc2n7 = Problem
    .doc = One problem a checker found in a file: where it stands and what it says.
    .noun = { $count ->
        [one] problem
       *[other] problems
    }

d-nbyucnyk = Severity

d-cg3ctzdq = Line
    .doc = The line, counted from 0.

d-f5kva2nw = Column
    .doc = The column, counted from 0.

d-e35xwjly = Message

d-vxqkc2n7-filter-severity = Severity

d-vxqkc2n7-filter-line = Line

d-vxqkc2n7-filter-column = Column

d-ymvbqmkh = Found
    .doc = One place in a file the words a person looked for stand: its line, counted from 0, the column, and the words of the line there.
    .noun = { $count ->
        [one] found
       *[other] founds
    }

d-hzzanee7 = Line

d-qt6b4p4a = Column

d-55vdsdve = Words

d-ymvbqmkh-filter-line = Line

d-ymvbqmkh-filter-column = Column

d-45c6x2nh = Item
    .doc = One line of a command's output that names something a person can go to.
    .noun = { $count ->
        [one] item
       *[other] items
    }

d-gvqsbkfl = Kind

d-6zcxi5vb = Text

d-effdgz7q = Path
    .doc = The file it names, when it names one.

d-62tkoolt = Line

d-3ritafbo = Column

d-45c6x2nh-filter-kind = Kind

d-45c6x2nh-filter-line = Line

d-45c6x2nh-filter-column = Column

d-xef44qec = Command
    .doc = One command run in the project.
    .noun = { $count ->
        [one] command
       *[other] commands
    }

d-c4ytwucq = Command

d-i3avwdem = Cwd
    .doc = The folder it ran in.

d-2xcd2xlu = Started

d-thtznwi2 = Ended

d-vjb7fevj = Exit
    .doc = What the command answered: 0 while it runs, its exit status once it is done.

d-bukegscc = Output
    .doc = Its output, both streams as one, as the terminal shows it.

d-xa7laxtf = Truncated
    .doc = The output grew past what a run keeps: what stands here is the start of it.

d-dgchvwcx = Job
    .doc = The engine's job while it runs; empty once it has stopped.

d-ufboumgh = State
    .doc = `running`, `done`, `stopped` or `failed`.

d-cck6yows = Items
    .doc = The lines of its output that name a problem, a place or a test.

d-xef44qec-filter-started = Started

d-xef44qec-filter-ended = Ended

d-xef44qec-filter-exit = Exit

d-xef44qec-filter-truncated = Truncated

d-hokczcgb = Hit
    .doc = One place a project search found the words it looked for.
    .noun = { $count ->
        [one] hit
       *[other] hits
    }

d-bcptqatu = Path
    .doc = The file's path on this device.

d-zwn4bgwt = Rel
    .doc = Its path in the project, as the results show it.

d-bgpnyksl = Line
    .doc = The line, counted from 0.

d-hbulfnxb = Column

d-uk7foyau = Len

d-hl3ypi66 = Preview
    .doc = The line's words, trimmed to the place the match stands.

d-hokczcgb-filter-line = Line

d-hokczcgb-filter-column = Column

d-hokczcgb-filter-len = Len

d-a3vtjwxq = Completion
    .doc = One suggestion a language server offers at the caret.
    .noun = { $count ->
        [one] completion
       *[other] completions
    }

d-qg4m5uwr = Label

d-xm4myjqj = Detail
    .doc = What the suggestion stands for, as the server says it (`fn(u64) -> Vec<u64>`).

d-kwj5llbq = Insert
    .doc = What is written where it is accepted.

d-brvl6h6g = Location
    .doc = One place a declaration stands: what go-to-definition answers.
    .noun = { $count ->
        [one] location
       *[other] locations
    }

d-trhzxars = Path

d-pu4qmppj = Line

d-e2bphgrg = Column

d-brvl6h6g-filter-line = Line

d-brvl6h6g-filter-column = Column

d-z5hlonow = Loaded
    .doc = The device's answers, as this app reads them.
    .noun = { $count ->
        [one] loaded
       *[other] loadeds
    }

d-pisboohb = Text

d-wcz4xfj7 = Mtime

d-jqqoxnpy = Len

d-z5hlonow-filter-mtime = Mtime

d-z5hlonow-filter-len = Len

d-s2gjwjhz = Stamped
    .noun = { $count ->
        [one] stamped
       *[other] stampeds
    }

d-fj7hqru4 = Mtime

d-qw5jig3a = Len

d-s2gjwjhz-filter-mtime = Mtime

d-s2gjwjhz-filter-len = Len

d-jslhrmnm = Exists
    .noun = { $count ->
        [one] exist
       *[other] exists
    }

d-uvwwyzvs = Exists

d-7n6rmzco = Mtime

d-pwjqsxzz = Len

d-jslhrmnm-filter-exists = Exists

d-jslhrmnm-filter-mtime = Mtime

d-jslhrmnm-filter-len = Len

d-44dpm5a4 = Disk ask
    .noun = { $count ->
        [one] disk ask
       *[other] disk asks
    }

d-kn6q47ka = Path

d-vc6ysavm = Disk save
    .noun = { $count ->
        [one] disk save
       *[other] disk saves
    }

d-haepmky3 = Path

d-pwykv2y3 = Text

d-zetzlvco = Mtime

d-evmprhon = Len

d-om5axsdm = Force

d-vc6ysavm-filter-mtime = Mtime

d-vc6ysavm-filter-len = Len

d-vc6ysavm-filter-force = Force

d-opcoyw4j = Disk entry
    .noun = { $count ->
        [one] disk entry
       *[other] disk entries
    }

d-bfnbknrx = Name

d-wv4a7ixx = Path

d-7z4nle5w = Folder

d-pp4ozwjd = Hidden

d-opcoyw4j-filter-folder = Folder

d-opcoyw4j-filter-hidden = Hidden

d-t43et6gp = Listing
    .noun = { $count ->
        [one] listing
       *[other] listings
    }

d-pd7egi2y = Entries

d-bon4ylpi = Search ask
    .noun = { $count ->
        [one] search ask
       *[other] search asks
    }

d-xkrqnga5 = Root

d-5dkbrhtv = Needle

d-cdk5sbcj = Case sensitive

d-b7eojwjq = Whole word

d-bon4ylpi-filter-case_sensitive = Case sensitive

d-bon4ylpi-filter-whole_word = Whole word

d-vkas4mky = Run ask
    .noun = { $count ->
        [one] run ask
       *[other] run asks
    }

d-qhqdbt2f = Command

d-vqlkdbpf = Cwd

d-33v7dktu = Job answer
    .noun = { $count ->
        [one] job answer
       *[other] job answers
    }

d-o7kaxwxn = Job

d-mjpwq7cd = Search state
    .noun = { $count ->
        [one] search state
       *[other] search states
    }

d-zby3g4ps = State

d-aqlgsja7 = Hits

d-vp2ht5bq = Files

d-izoyjzkp = Truncated

d-mjpwq7cd-filter-files = Files

d-mjpwq7cd-filter-truncated = Truncated

d-mmfil2sb = Run state
    .noun = { $count ->
        [one] run state
       *[other] run states
    }

d-coxzozlk = State

d-swqx5bgj = Exit

d-wsbgo4u6 = Output

d-vlearfbg = Truncated

d-iqbxloe7 = Failure

d-mmfil2sb-filter-exit = Exit

d-mmfil2sb-filter-truncated = Truncated

d-oghkbdye = Server ask
    .noun = { $count ->
        [one] server ask
       *[other] server asks
    }

d-dwxem6ty = Language

d-fegqdch5 = Root

d-qzi3lurf = Document ask
    .noun = { $count ->
        [one] document ask
       *[other] document asks
    }

d-jmzsddw6 = Job

d-cimxyukl = Path

d-u2ozcojk = Language

d-kxtscxdf = Text

d-xbl32ey4 = Version

d-qzi3lurf-filter-version = Version

d-3ayhgfzm = Ask ask
    .noun = { $count ->
        [one] ask ask
       *[other] ask asks
    }

d-gxbdmbnd = Job

d-ysxutqlg = Path

d-p4m2am32 = Line

d-q3inki66 = Column

d-l45wtegn = What

d-3ayhgfzm-filter-line = Line

d-3ayhgfzm-filter-column = Column

d-qsowlyv4 = Progress
    .noun = { $count ->
        [one] progress
       *[other] progresses
    }

d-agn2f2db = Title

d-g7hzlu2h = Detail

d-whcglspc = Percent

d-qsowlyv4-filter-percent = Percent

d-7nbsbvje = Diag
    .noun = { $count ->
        [one] diag
       *[other] diags
    }

d-ioyqdqpm = Severity

d-beb3ijea = Line

d-mev4fmk5 = Column

d-iwar3uyn = Message

d-7nbsbvje-filter-line = Line

d-7nbsbvje-filter-column = Column

d-7rcunm5q = Diagnostics
    .noun = { $count ->
        [one] diagnostic
       *[other] diagnostics
    }

d-pfdqfft6 = Diagnostics
    .doc = Each file's problems, by the path it stands at.

d-xa3s7x43 = Completion answer
    .noun = { $count ->
        [one] completion answer
       *[other] completion answers
    }

d-z5gkzdtb = What

d-xm2tpahh = Items

d-kpy2dgw2 = Hover answer
    .noun = { $count ->
        [one] hover answer
       *[other] hover answers
    }

d-jchhxxfm = What

d-m4blp7e7 = Text

d-t2uoek4h = Definition answer
    .noun = { $count ->
        [one] definition answer
       *[other] definition answers
    }

d-qb2osbdn = What

d-u57x3dub = Locations

d-2sfpuocd = Format edit
    .noun = { $count ->
        [one] format edit
       *[other] format edits
    }

d-7flc4fke = Start

d-g2cgs6ok = Start column

d-wfsos5vr = End

d-jhfxuyi3 = End column

d-yj73ybbe = Text

d-2sfpuocd-filter-start = Start

d-2sfpuocd-filter-start_column = Start column

d-2sfpuocd-filter-end = End

d-2sfpuocd-filter-end_column = End column

d-o7aifpfb = Format answer
    .noun = { $count ->
        [one] format answer
       *[other] format answers
    }

d-um2x3bkg = What

d-wikls7kq = Edits

d-zke66gso = Error

d-uaoqlskg = Server state
    .noun = { $count ->
        [one] server state
       *[other] server states
    }

d-mxvtycpj = State

d-kqcbbx2v = Server

d-66ixtrna = Failure

d-sqfl7jdi = Progress

d-s4bc4e6s = Diagnostics

d-h4pgltlo = Answer

d-uaoqlskg-filter-progress = Progress

d-uaoqlskg-filter-diagnostics = Diagnostics

d-a34gxw47 = Project
    .doc = A project: its files and folders, the commands run in it, and what the device answers about it.
    .noun = { $count ->
        [one] project
       *[other] projects
    }
    .untitled = Untitled project

d-evbsknez = Files

d-inupl3md = Commands
    .doc = The commands run in the project, newest last.

d-ubr6i22h = Folder
    .doc = The folder on this device the project was opened from; empty for a project that lives in the workspace alone.

d-7sbddtl7 = Server job
    .doc = The engine's job for the language server, while one runs for the open file's language.

d-w3s6k4wz = Server language
    .doc = The language the running server answers for.

d-tz2cc24s = Server name
    .doc = The server's name (`rust-analyzer`), as its state line says it.

d-wajnagjm = Server state
    .doc = `starting`, `ready`, `busy`, `failed` or `stopped`; empty before a server was wanted.

d-mkyngogi = Server trouble
    .doc = What is wrong with the server, in a person's words.

d-jwtdk5zn = Server progress
    .doc = The progress of the work the server is doing (`Indexing 42%`).

d-yukh3x5s = Language servers
    .doc = Language servers answer for this project (the personal preference, kept with it).

d-5qk4qm7f = Format on save
    .doc = A file is formatted by its server each time it is saved.

d-eolbog7w = Answer what
    .doc = The line a person reads code help about, and the words it answers: what a completion list, a hover or a definition put on the page.

d-luh6yto7 = Answer text

d-5vjxpshd = Answer line

d-w3z62her = Answer column

d-uix4xjnx = Reveal line
    .doc = The place a search result, a problem or a symbol opened: what the page shows, apart from where code help was asked.

d-bi7d7kdu = Reveal column

d-p4n4bpno = Answer items

d-thlj3m5d = Answer locations

d-t4nipezs = Search job
    .doc = The project search the person started, as it stands.

d-nbqioo6i = Search query

d-5z5b5fqg = Search text

d-7tocyk5d = Search case

d-t6mgiinw = Search word

d-of2jzwnc = Search state

d-nmqjtfyh = Search hits

d-3gaova7k = Search files

d-w3quhvl4 = Search truncated

d-b5q44glb = Found
    .doc = What the find row of the open file last found.

d-vwrqicgc = Trouble
    .doc = What the last thing the app did came to, where a person reads it.

d-a34gxw47-filter-language_servers = Language servers

d-a34gxw47-filter-format_on_save = Format on save

d-a34gxw47-filter-answer_line = Answer line

d-a34gxw47-filter-answer_column = Answer column

d-a34gxw47-filter-reveal_line = Reveal line

d-a34gxw47-filter-reveal_column = Reveal column

d-a34gxw47-filter-search_case = Search case

d-a34gxw47-filter-search_word = Search word

d-a34gxw47-filter-search_files = Search files

d-a34gxw47-filter-search_truncated = Search truncated

d-5zxrwqiw = Entry kind
    .doc = What an entry of the project is: code that is written, or a place that holds entries.

d-a2nctzaf = File

d-prdvqlwb = Folder

d-oei5dbt7 = Severity
    .doc = How much a problem matters, as the editor shows it.

d-x6gee6kv = Error

d-2eccreml = Warning

d-5dll5rqh = Info

d-t4vqqdlh = Hint

d-3ksl3xtm = Item kind
    .doc = What a line of a command's output is: a problem a tool found, a place it named, a test, or a change.

d-vh5xnbgh = Error

d-pfjpo75e = Warning

d-ecad2xkw = Location

d-ceeeookj = Test passed

d-y7qerkxo = Test failed

d-xdnyowon = Test ignored

d-4ob7cofy = Change

d-zamjq72l = New file
    .doc = Starts a file called `name` in `folder`, holding `text`: what a person types in the project, what a drop makes, and where an import lands.
    .w-a-file-s-name-cannot-hold = A file's name cannot hold a slash and cannot be empty
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-zamjq72l-name = Name

d-zamjq72l-folder = Folder

d-zamjq72l-text = Text

d-7t3vkzl7 = New folder
    .doc = Starts a folder called `name` in `folder`.
    .w-a-folder-s-name-cannot-hold = A folder's name cannot hold a slash and cannot be empty
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-7t3vkzl7-name = Name

d-7t3vkzl7-folder = Folder

d-xo5gmi2s = Rename
    .doc = Renames `entry` to `name`, keeping the names in its folder apart.
    .w-a-name-cannot-hold-a-slash = A name cannot hold a slash and cannot be empty
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-xo5gmi2s-entry = Entry

d-xo5gmi2s-name = Name

d-avhnccjo = Move
    .doc = Moves `entry` into `folder` (the top of the project when none), just before `before` — or after every entry there when none names one.
    .w-an-entry-moves-into-a-folder = An entry moves into a folder
    .w-a-folder-cannot-hold-itself = A folder cannot hold itself
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-avhnccjo-entry = Entry

d-avhnccjo-folder = Folder

d-avhnccjo-before = Before

d-5ml2ymon = Move
    .doc = Moves `entry` just after `after`, as a drop on the row below asks.
    .w-an-entry-cannot-stand-after-itself = An entry cannot stand after itself

d-5ml2ymon-entry = Entry

d-5ml2ymon-after = After

d-b6hozlbl = Delete
    .doc = Deletes `entry`, and every entry under it: a folder empties with its entries, as one change a person takes back at once.

d-b6hozlbl-entry = Entry

d-xmgwozuc = Language
    .doc = Sets the language `entry` is coloured and checked as, by the name of a language the editor knows.
    .w-is-not-a-language-the-editor = `{ $language }` is not a language the editor knows

d-xmgwozuc-language = Language

d-thmoupjg = Replace the text
    .doc = Writes `text` as the whole file, for an agent or a script that holds a file's new text.
    .did = wrote a file
    .w-a-folder-has-no-text = A folder has no text

d-thmoupjg-text = Text

d-54h7hn2x = Replace a range
    .doc = Replaces `count` characters from `at` with `text`: a range or a caret in one file, counted in characters from its start.
    .did = wrote a file
    .w-a-folder-has-no-text = A folder has no text
    .w-the-range-lies-outside-the-file = The range lies outside the file

d-54h7hn2x-at = At

d-54h7hn2x-count = Count

d-54h7hn2x-text = Text

d-hkywvnmq = Duplicate line
    .doc = Copies a line of the file to the line under it.
    .w-the-line-is-outside-the-file = The line is outside the file

d-hkywvnmq-line = Line

d-u4tf42bi = Move line
    .doc = Moves a line over the one above it (`up`) or under the one below it.
    .w-the-line-is-outside-the-file = The line is outside the file
    .w-the-line-is-at-the-file = The line is at the file's edge

d-u4tf42bi-line = Line

d-u4tf42bi-up = Up

d-eczcslbz = Delete line
    .doc = Deletes a line of the file.
    .w-the-line-is-outside-the-file = The line is outside the file

d-eczcslbz-line = Line

d-pggfkkcf = Toggle comment
    .doc = Comments out a line, or takes its comment away when the line stands commented already: the marker is the language's own, at the line's own indentation.
    .w-a-file-has-no-line-comment = A { $language } file has no line comment
    .w-the-line-is-outside-the-file = The line is outside the file

d-pggfkkcf-line = Line

d-cocpiuyx = Indent
    .doc = Adds one indentation step to a line.
    .w-the-line-is-outside-the-file = The line is outside the file

d-cocpiuyx-line = Line

d-cbzbgevx = Outdent
    .doc = Takes one indentation step off a line.
    .w-the-line-is-outside-the-file = The line is outside the file

d-cbzbgevx-line = Line

d-rgh5qh4q = Find in file
    .doc = Finds the words `text` in a file and keeps what it found, for the page's find row.

d-rgh5qh4q-entry = Entry

d-rgh5qh4q-text = Text

d-rgh5qh4q-case = Case

d-rgh5qh4q-word = Word

d-pyrg5gaw = Find in file
    .doc = Finds the words `text` in a file, the case of their letters aside unless `case` says otherwise; `word` matches only where they stand alone as a word.
    .w-type-something-to-find = Type something to find

d-pyrg5gaw-text = Text

d-pyrg5gaw-case = Case

d-pyrg5gaw-word = Word

d-qeqmb2pr = Replace in file
    .doc = Replaces every place the words `text` stand in a file, answering how many changed.
    .did = replaced words
    .w-type-something-to-replace = Type something to replace

d-qeqmb2pr-text = Text

d-qeqmb2pr-with = With

d-qeqmb2pr-case = Case

d-qeqmb2pr-word = Word

d-hedpejsd = Check the file
    .doc = Checks a file and keeps what it found: the device's server's problems when one answers for the file, the editor's own otherwise.
    .w-a-folder-holds-no-text = A folder holds no text

d-hedpejsd-entry = Entry

d-6jlaqqyx = Open a project folder
    .doc = Opens the folder `folder` as the project: each file and folder it holds, read as entries, and each file's bytes as its text.

d-6jlaqqyx-folder = Folder

d-vmmfvt6f = Read into
    .doc = Records the entries of `listing`, standing in `folder`, and reads a file's own bytes: the tree is the project's, so every file the project shows is one a save can check.

d-vmmfvt6f-parent = Parent

d-vmmfvt6f-listing = Listing

d-thpwxqjh = Read folder into

d-thpwxqjh-folder = Folder

d-thpwxqjh-path = Path

d-og2uvscy = Read text
    .doc = Reads a file's bytes as its text, keeping the stamp a save checks against; a file that is not text, or is too large, is an entry with nothing in it that says why when it is opened.

d-og2uvscy-entry = Entry

d-ip74dfdt = Open a file
    .doc = Opens a file a person chose, or one Files or another app handed over: `file` is the file it is, and its words become an entry of the project.
    .did = opened a file

d-ip74dfdt-file = File

d-pgtuoai2 = Save
    .doc = Writes a file's text to the device, checking first that the bytes there are the ones this project read: a file something else changed is not written over without the person saying so.
    .w-a-folder-has-no-text = A folder has no text
    .w-this-file-is-nowhere-on-the = This file is nowhere on the device yet: save it as one

d-pgtuoai2-entry = Entry

d-5kbwbngv = Write

d-5kbwbngv-entry = Entry

d-5kbwbngv-force = Force

d-m6yumndr = Save over it
    .doc = Saves a file over the device's change, as a person chose after reading what changed.

d-m6yumndr-entry = Entry

d-hkv2gfvd = Reload from disk
    .doc = Reads the file again from the device, dropping what changed here.

d-hkv2gfvd-entry = Entry

d-z6zgrduy = Save all
    .doc = Writes every file that changed here to the device.

d-yuwykmf6 = Save into
    .doc = Writes `entry` into the folder `files` names, under its own name.
    .w-choose-a-folder-to-save-the = Choose a folder to save the file in

d-yuwykmf6-entry = Entry

d-yuwykmf6-files = Files

d-nwpwungx = Move on disk
    .doc = Moves a file into the folder `to` of the project, on the device as in the tree.
    .w-this-file-is-nowhere-on-the = This file is nowhere on the device

d-nwpwungx-entry = Entry

d-nwpwungx-to = To

d-w5jbknvi = Import files
    .doc = Imports the files a person dropped on the window: a folder becomes a folder of the project, a text file an entry with its words.

d-w5jbknvi-files = Files

d-u2s46hxm = Run
    .doc = Runs `command` in the project's folder, as the terminal does: its output and its exit status are kept with the command, and a person reads them beside the code.
    .w-type-a-command-to-run = Type a command to run

d-u2s46hxm-command = Command

d-u2s46hxm-cwd = Cwd

d-5axs3ogs = Stop
    .doc = Stops the command that runs in the project now.
    .w-nothing-runs-in-the-project-now = Nothing runs in the project now

d-v6rwio5m = Run again
    .doc = Runs a command of the project's terminal again, as a person does from its history.

d-v6rwio5m-run = Run

d-nfvb5ymp = Run the test again
    .doc = Runs one test of a run again: the command with the test's name after it, run as that run's tool ran it.

d-nfvb5ymp-run = Run

d-nfvb5ymp-test = Test

d-dmtobwu4 = Run

d-dmtobwu4-cwd = Cwd

d-dmtobwu4-command = Command

d-ndblrsrx = Clear the terminal
    .doc = Erases the terminal's history.

d-juebimhb = Open a terminal
    .doc = Opens the project's folder in the system's terminal: the shell a person runs by hand.

d-7weouyp2 = Poll
    .doc = Asks how far the running commands and the project search have got, once a second while anything runs: the one place the device's answers are folded into what the window shows.

d-mqipns37 = Go to it
    .doc = Brings the caret to the place an item of a command's output names: the file is read if the project does not hold it yet, and the page shows it.
    .w-that-line-names-no-file = That line names no file

d-mqipns37-item = Item

d-7ecrxle7 = Seen line

d-7ecrxle7-line = Line

d-a7tfyjpe = Search the project
    .doc = Searches every file of the project for `needle`, the case of its letters aside unless `case` says otherwise.
    .w-type-something-to-find = Type something to find

d-a7tfyjpe-needle = Needle

d-a7tfyjpe-case = Case

d-a7tfyjpe-word = Word

d-5lsivooa = Stop searching
    .doc = Stops the project search.

d-ivfwweyt = Open the place
    .doc = Shows the place a search found: the file is read from the device when the project does not hold it yet, and the page opens it.

d-ivfwweyt-hit = Hit

d-kcnxibkp = Ensure server
    .doc = Starts the language server for a file's language, if none runs for it yet, and tells it about the file.

d-kcnxibkp-entry = Entry

d-34fbxyr2 = Announce
    .doc = Tells the server that `entry` is open, with the text it holds now, so its answers are about what a person reads.

d-34fbxyr2-entry = Entry

d-hxjncwl5 = Poll server
    .doc = Reads what the server answered: its state and progress, its problems for each file, and any answer to a code-help ask the page made.

d-63l6fhcb = Read answer
    .doc = Reads the answer the server gave to the page's last ask: a completion list, the words a hover shows, the places a declaration stands at, or the edits a format makes.

d-63l6fhcb-answer = Answer

d-2p3qz2ll = Suggest
    .doc = Asks the server for completions at a place in a file.

d-2p3qz2ll-entry = Entry

d-2p3qz2ll-line = Line

d-2p3qz2ll-column = Column

d-3cpxvmxh = What stands here
    .doc = Asks the server what stands at a place in a file.

d-3cpxvmxh-entry = Entry

d-3cpxvmxh-line = Line

d-3cpxvmxh-column = Column

d-nkwsrijh = Go to definition
    .doc = Asks the server where the declaration at a place is written.

d-nkwsrijh-entry = Entry

d-nkwsrijh-line = Line

d-nkwsrijh-column = Column

d-pnged4ux = Write the suggestion
    .doc = Writes the completion a person chose where the caret stands.

d-pnged4ux-entry = Entry

d-pnged4ux-item = Item

d-fu3y2v7i = Format the file
    .doc = Reformats a file as its language's formatter does, as one change.
    .w-a-folder-holds-no-text = A folder holds no text

d-fu3y2v7i-entry = Entry

d-cph2enjs = Language servers
    .doc = Turns language servers on or off, for the project.

d-cph2enjs-servers = Servers

d-m5tkeabl = Format on save
    .doc = Turns formatting a file on save on or off.

d-m5tkeabl-format = Format

d-m3cwqb2r = Try again
    .doc = Stops the language server, as a person pressing "Try again" after a failure does.

d-l7lwb6q5 = Add child
    .doc = Starts a file inside the folder `e`: a row's "+", pressed.

d-baaey4sk = New file~present

d-2wzefmvk = New folder~present

d-p3dsnwpy = Rename~present

d-eqkywfls = Move entry~present

d-cogoamzq = Move after~present

d-j4nd4j3a = Set language~present

d-samgh7oc = Replace text~present

d-24fjpwqk = Replace range~present

d-3wawneyr = Duplicate line~present

d-ijkczbmv = Move line~present

d-u2y5ffw6 = Delete line~present

d-6r5pq3fu = Toggle comment~present

d-75tpm4zs = Indent line~present

d-se2ooaqw = Outdent line~present

d-lmrtkaxd = Check~present

d-jxe2rmpm = Save~present

d-33j2f7gw = Save into~present

d-fpjxwhph = Move on disk~present

d-qfnk3hny = Run~present

d-enlvxjgl = Stop~present

d-unzcdvgw = Reveal item~present

d-pxdbowga = Format file~present

d-yspw5xt2 = File row

d-liefjfl2 = Language menu
    .w-language = Language

d-zw7y7jba = Add inside
    .w-new-file = New file

d-qe5s5jlf = Project head
    .w-project = Project
    .w-open-a-folder = Open a folder…
    .w-new-file = New file
    .w-new-folder = New folder
    .w-save-all = Save all
    .w-search-the-project = Search the project…
    .w-open-a-terminal = Open a terminal

d-744hb56m = Project
    .w-no-files-yet-a-project-starts = No files yet: a project starts with its first file
    .w-unsaved = Unsaved
    .w-changed-on-disk = Changed on disk
    .w-file = File
    .w-save = Save
    .w-save-as = Save as…
    .w-reload-from-disk = Reload from disk
    .w-format = Format
    .w-duplicate-line = Duplicate line
    .w-delete-line = Delete line
    .w-toggle-comment = Toggle comment
    .w-check-the-file = Check the file
    .w-suggest = Suggest
    .w-what-stands-here = What stands here
    .w-go-to-definition = Go to definition
    .w-find-in-the-file = Find in the file
    .w-find = Find
    .w-replace-all = Replace all
    .w-panel = Panel
    .w-problems = Problems
    .w-terminal = Terminal
    .w-search-results = Search results
    .w-write-the-file = Write the file
    .w-found = Found
    .w-nothing-in-this-file-holds = Nothing in this file holds “{ $finding }”
    .w-line = line { $value }
    .w-unsaved-changes = Unsaved changes
    .w-1-problem = 1 problem
    .w-problems-2 = { $len } problems
    .w-try-again = Try again
    .w-ln-col = Ln { $value }, Col { $value2 }
    .w-suggestions = Suggestions
    .w-definitions = Definitions
    .w-choose-a-file-to-read-it = Choose a file to read it

d-6jwzz6bh = Explorer footer
    .w-a-project-in-the-workspace-alone = A project in the workspace alone

d-7yzd7qdh = Problems panel
    .w-this-file-has-not-been-checked = This file has not been checked yet
    .w-check-the-file = Check the file
    .w-by = { $problems_said }, by { $source }
    .w-line-column = line { $value }, column { $value2 }

d-zl3bj7gd = Terminal panel
    .w-run-a-command-in-the-project = Run a command in the project
    .w-run = Run
    .w-stop = Stop
    .w-clear = Clear
    .w-no-command-has-run-in-the = No command has run in the project
    .w-run-again = Run again
    .w-terminal = Terminal
    .w-open-a-terminal = Open a terminal
    .w-clear-the-terminal = Clear the terminal

d-2lxnmz5v = Search panel
    .w-find-in-every-file = Find in every file
    .w-search = Search
    .w-stop = Stop

d-bbheeez5 = File view
    .w-write-the-file = Write the file

d-seqze3hc = Files
    .w-no-files-yet = No files yet

d-erf5uzfi = Commands
    .w-no-command-has-run-in-the = No command has run in the project

d-ynyb6yyr = Command view
    .w-ran-in = Ran in
    .w-state = State
