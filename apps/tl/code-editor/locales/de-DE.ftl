# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-code-editor = Code-Editor

d-so-tilo-code-editor-keywords = code editor, code, editor, source, ide, programming editor, code editor online, notepad, text editor, quelltext, code-editor, editor, programmieren, entwickeln

d-m2evo7on = Entry
    .doc = Eine Datei oder ein Ordner des Projekts: its name, where it stands, the language it is written in, and — a file's — its text.
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }
    .untitled = untitled

d-jr3jrrmx = Art

d-wlnikenq = Name

d-otwgp44c = Übergeordnet
    .doc = The folder it stands in; none at the top of the project.

d-fav4hm4j = Sprache
    .doc = The language it is coloured, checked and kept as, by the language's own name (`Rust`, `Plain Text`); empty for a folder.

d-zbluopai = Text
    .doc = The file's text.

d-n6s6du45 = Disk
    .doc = Where the file stands on this device; empty for a file saved nowhere yet.

d-lkyxb2f7 = Stamp
    .doc = The modified time of the bytes this project last read or wrote, in nanoseconds; 0 when the file is nowhere on this device.

d-wxaqrw5b = Stamp len
    .doc = The length of the bytes the project last read or wrote.

d-bvn4nnf5 = Speichernd
    .doc = The text as it stands on the device, so a reader sees what changed here and what is written already.

d-uhopusbk = Stale
    .doc = The file changed on the device since it was read here: saving over it asks first.

d-o3gbo56k = Probleme
    .doc = What the last check of this file found.

d-jwc6eef6 = Checked
    .doc = What that check read: a file whose text no longer hashes to this has changed since.

d-jwkhracm = Source
    .doc = The checker that answered: the server's name, or `lint` while only the editor's own checks have run; empty before the file was checked.

d-m2evo7on-filter-kind = Art

d-m2evo7on-filter-parent = Übergeordnet

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

d-cg3ctzdq = Zeile
    .doc = The line, counted from 0.

d-f5kva2nw = Column
    .doc = The column, counted from 0.

d-e35xwjly = Message

d-vxqkc2n7-filter-severity = Severity

d-vxqkc2n7-filter-line = Zeile

d-vxqkc2n7-filter-column = Column

d-ymvbqmkh = Found
    .doc = One place in a file the words a person looked for stand: its line, counted from 0, the column, and the words of the line there.
    .noun = { $count ->
        [one] found
       *[other] founds
    }

d-hzzanee7 = Zeile

d-qt6b4p4a = Column

d-55vdsdve = Words

d-ymvbqmkh-filter-line = Zeile

d-ymvbqmkh-filter-column = Column

d-45c6x2nh = Item
    .doc = One line of a command's output that names something a person can go to.
    .noun = { $count ->
        [one] item
       *[other] items
    }

d-gvqsbkfl = Art

d-6zcxi5vb = Text

d-effdgz7q = Path
    .doc = The file it names, when it names one.

d-62tkoolt = Zeile

d-3ritafbo = Column

d-45c6x2nh-filter-kind = Art

d-45c6x2nh-filter-line = Zeile

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

d-bgpnyksl = Zeile
    .doc = The line, counted from 0.

d-hbulfnxb = Column

d-uk7foyau = Len

d-hl3ypi66 = Preview
    .doc = The line's words, trimmed to the place the match stands.

d-hokczcgb-filter-line = Zeile

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

d-pu4qmppj = Zeile

d-e2bphgrg = Column

d-brvl6h6g-filter-line = Zeile

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

d-bon4ylpi = Suchen ask
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

d-vkas4mky = Ausführen ask
    .noun = { $count ->
        [one] run ask
       *[other] run asks
    }

d-qhqdbt2f = Command

d-vqlkdbpf = Cwd

d-mjpwq7cd = Suchen state
    .noun = { $count ->
        [one] search state
       *[other] search states
    }

d-zby3g4ps = State

d-aqlgsja7 = Hits

d-vp2ht5bq = Dateis

d-izoyjzkp = Truncated

d-mjpwq7cd-filter-files = Dateis

d-mjpwq7cd-filter-truncated = Truncated

d-mmfil2sb = Ausführen state
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

d-dwxem6ty = Sprache

d-fegqdch5 = Root

d-qzi3lurf = Document ask
    .noun = { $count ->
        [one] document ask
       *[other] document asks
    }

d-jmzsddw6 = Job

d-cimxyukl = Path

d-u2ozcojk = Sprache

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

d-p4m2am32 = Zeile

d-q3inki66 = Column

d-l45wtegn = What

d-3ayhgfzm-filter-line = Zeile

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

d-beb3ijea = Zeile

d-mev4fmk5 = Column

d-iwar3uyn = Message

d-7nbsbvje-filter-line = Zeile

d-7nbsbvje-filter-column = Column

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

d-2sfpuocd = Formatieren edit
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

d-o7aifpfb = Formatieren answer
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
    .doc = Die Arbeit, die der Server gerade tut (`Indexing 42%`), wenn er welche tut.

d-s4bc4e6s = Diagnostics
    .doc = Die Probleme jeder Datei, nach dem Pfad, unter dem sie steht.

d-h4pgltlo = Answer

d-a34gxw47 = Projekt
    .doc = A project: its files and folders, the commands run in it, and what the device answers about it.
    .noun = { $count ->
        [one] project
       *[other] projects
    }
    .untitled = Unbenannt project

d-evbsknez = Dateis

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

d-yukh3x5s = Sprache servers
    .doc = Sprache servers answer for this project (the personal preference, kept with it).

d-5qk4qm7f = Formatieren on save
    .doc = A file is formatted by its server each time it is saved.

d-eolbog7w = Answer what
    .doc = The line a person reads code help about, and the words it answers: what a completion list, a hover or a definition put on the page.

d-luh6yto7 = Answer text

d-5vjxpshd = Answer line

d-w3z62her = Answer column

d-p4n4bpno = Answer items

d-thlj3m5d = Answer locations

d-t4nipezs = Suchen job
    .doc = The project search the person started, as it stands.

d-nbqioo6i = Suchen query

d-5z5b5fqg = Suchen text

d-7tocyk5d = Suchen case

d-t6mgiinw = Suchen word

d-of2jzwnc = Suchen state

d-nmqjtfyh = Suchen hits

d-3gaova7k = Suchen files

d-w3quhvl4 = Suchen truncated

d-b5q44glb = Found
    .doc = What the find row of the open file last found.

d-vwrqicgc = Trouble
    .doc = What the last thing the app did came to, where a person reads it.

d-a34gxw47-filter-language_servers = Sprache servers

d-a34gxw47-filter-format_on_save = Formatieren on save

d-a34gxw47-filter-answer_line = Answer line

d-a34gxw47-filter-answer_column = Answer column

d-a34gxw47-filter-search_case = Suchen case

d-a34gxw47-filter-search_word = Suchen word

d-a34gxw47-filter-search_files = Suchen files

d-a34gxw47-filter-search_truncated = Suchen truncated

d-5zxrwqiw = Entry kind
    .doc = What an entry of the project is: code that is written, or a place that holds entries.

d-a2nctzaf = Datei

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

d-zamjq72l = Neue Datei
    .doc = Starts a file called `name` in `folder`, holding `text`: what a person types in the project, what a drop makes, and where an import lands.
    .w-a-file-s-name-cannot-hold = A file's name cannot hold a slash and cannot be empty
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-zamjq72l-name = Name

d-zamjq72l-folder = Folder

d-zamjq72l-text = Text

d-7t3vkzl7 = Neuer Ordner
    .doc = Starts a folder called `name` in `folder`.
    .w-a-folder-s-name-cannot-hold = A folder's name cannot hold a slash and cannot be empty
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-7t3vkzl7-name = Name

d-7t3vkzl7-folder = Folder

d-xo5gmi2s = Umbenennen
    .doc = Umbenennens `entry` to `name`, keeping the names in its folder apart.
    .w-a-name-cannot-hold-a-slash = A name cannot hold a slash and cannot be empty
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-xo5gmi2s-entry = Entry

d-xo5gmi2s-name = Name

d-avhnccjo = Verschieben
    .doc = Verschiebens `entry` into `folder` (the top of the project when none), just before `before` — or after every entry there when none names one.
    .w-an-entry-moves-into-a-folder = An entry moves into a folder
    .w-a-folder-cannot-hold-itself = A folder cannot hold itself
    .w-an-entry-of-that-name-is = An entry of that name is in the folder already

d-avhnccjo-entry = Entry

d-avhnccjo-folder = Folder

d-avhnccjo-before = Before

d-5ml2ymon = Verschieben
    .doc = Verschiebens `entry` just after `after`, as a drop on the row below asks.
    .w-an-entry-cannot-stand-after-itself = An entry cannot stand after itself

d-5ml2ymon-entry = Entry

d-5ml2ymon-after = After

d-b6hozlbl = Löschen
    .doc = Löschens `entry`, and every entry under it: a folder empties with its entries, as one change a person takes back at once.

d-b6hozlbl-entry = Entry

d-xmgwozuc = Sprache
    .doc = Sets the language `entry` is coloured and checked as, by the name of a language the editor knows.
    .w-is-not-a-language-the-editor = `{ $language }` is not a language the editor knows

d-xmgwozuc-language = Sprache

d-thmoupjg = Ersetzen the text
    .doc = Writes `text` as the whole file, for an agent or a script that holds a file's new text.
    .did = wrote a file
    .w-a-folder-has-no-text = A folder has no text

d-thmoupjg-text = Text

d-54h7hn2x = Ersetzen a range
    .doc = Ersetzens `count` characters from `at` with `text`: a range or a caret in one file, counted in characters from its start.
    .did = wrote a file
    .w-a-folder-has-no-text = A folder has no text
    .w-the-range-lies-outside-the-file = The range lies outside the file

d-54h7hn2x-at = At

d-54h7hn2x-count = Count

d-54h7hn2x-text = Text

d-hkywvnmq = Zeile duplizieren
    .doc = Copies a line of the file to the line under it.
    .w-the-line-is-outside-the-file = The line is outside the file

d-hkywvnmq-line = Zeile

d-u4tf42bi = Verschieben line
    .doc = Verschiebens a line over the one above it (`up`) or under the one below it.
    .w-the-line-is-outside-the-file = The line is outside the file
    .w-the-line-is-at-the-file = The line is at the file's edge

d-u4tf42bi-line = Zeile

d-u4tf42bi-up = Up

d-eczcslbz = Löschen line
    .doc = Löschens a line of the file.
    .w-the-line-is-outside-the-file = The line is outside the file

d-eczcslbz-line = Zeile

d-pggfkkcf = Kommentar umschalten
    .doc = Comments out a line, or takes its comment away when the line stands commented already: the marker is the language's own, at the line's own indentation.
    .w-a-file-has-no-line-comment = A { $language } file has no line comment
    .w-the-line-is-outside-the-file = The line is outside the file

d-pggfkkcf-line = Zeile

d-cocpiuyx = Einrücken
    .doc = Adds one indentation step to a line.
    .w-the-line-is-outside-the-file = The line is outside the file

d-cocpiuyx-line = Zeile

d-cbzbgevx = Ausrücken
    .doc = Takes one indentation step off a line.
    .w-the-line-is-outside-the-file = The line is outside the file

d-cbzbgevx-line = Zeile

d-jrfrdinx = Suchen
    .doc = Die Suche des Fensters findet die Wörter in der offenen Datei und über die Dateien des Projekts: die erste Stelle in der Datei wird gezeigt, sodass ein Treffer schon beim Tippen im Blick ist.

d-jrfrdinx-entry = Eintrag

d-jrfrdinx-text = Text

d-pyrg5gaw = Suchen in file
    .doc = Suchens the words `text` in a file, the case of their letters aside unless `case` says otherwise; `word` matches only where they stand alone as a word.
    .w-type-something-to-find = Type something to find

d-pyrg5gaw-text = Text

d-pyrg5gaw-case = Case

d-pyrg5gaw-word = Word

d-qeqmb2pr = Ersetzen in file
    .doc = Ersetzens every place the words `text` stand in a file, answering how many changed.
    .did = replaced words
    .w-type-something-to-replace = Type something to replace

d-qeqmb2pr-text = Text

d-qeqmb2pr-with = With

d-qeqmb2pr-case = Case

d-qeqmb2pr-word = Word

d-hedpejsd = Die Datei prüfen
    .doc = Checks a file and keeps what it found: the device's server's problems when one answers for the file, the editor's own otherwise.
    .w-a-folder-holds-no-text = A folder holds no text

d-hedpejsd-entry = Entry

d-6jlaqqyx = Open a project folder
    .doc = Opens the folder `folder` as the project: each file and folder it holds, read as entries, and each file's bytes as its text.

d-6jlaqqyx-folder = Folder

d-vmmfvt6f = Read into
    .doc = Records the entries of `listing`, standing in `folder`, and reads a file's own bytes: the tree is the project's, so every file the project shows is one a save can check.

d-vmmfvt6f-parent = Übergeordnet

d-thpwxqjh = Read folder into

d-thpwxqjh-folder = Folder

d-thpwxqjh-path = Path

d-og2uvscy = Read text

d-og2uvscy-entry = Entry

d-ip74dfdt = Open a file
    .doc = Opens a file a person chose, or one Dateis or another app handed over: `file` is the file it is, and its words become an entry of the project.
    .did = opened a file

d-ip74dfdt-file = Datei

d-pgtuoai2 = Speichern
    .doc = Writes a file's text to the device, checking first that the bytes there are the ones this project read: a file something else changed is not written over without the person saying so.
    .w-a-folder-has-no-text = A folder has no text
    .w-this-file-is-nowhere-on-the = This file is nowhere on the device yet: save it as one

d-pgtuoai2-entry = Entry

d-5kbwbngv = Write

d-5kbwbngv-entry = Entry

d-5kbwbngv-force = Force

d-m6yumndr = Speichern over it
    .doc = Speicherns a file over the device's change, as a person chose after reading what changed.

d-m6yumndr-entry = Entry

d-hkv2gfvd = Von der Platte neu laden
    .doc = Reads the file again from the device, dropping what changed here.

d-hkv2gfvd-entry = Entry

d-z6zgrduy = Speichern all
    .doc = Writes every file that changed here to the device.

d-yuwykmf6 = Speichern into
    .doc = Writes `entry` into the folder `files` names, under its own name.
    .w-choose-a-folder-to-save-the = Choose a folder to save the file in

d-yuwykmf6-entry = Entry

d-yuwykmf6-files = Dateis

d-nwpwungx = Verschieben on disk
    .doc = Verschiebens a file into the folder `to` of the project, on the device as in the tree.
    .w-this-file-is-nowhere-on-the = This file is nowhere on the device

d-nwpwungx-entry = Entry

d-nwpwungx-to = To

d-w5jbknvi = Import files
    .doc = Imports the files a person dropped on the window: a folder becomes a folder of the project, a text file an entry with its words.

d-w5jbknvi-files = Dateis

d-u2s46hxm = Ausführen
    .doc = Ausführens `command` in the project's folder, as the terminal does: its output and its exit status are kept with the command, and a person reads them beside the code.
    .w-type-a-command-to-run = Type a command to run

d-u2s46hxm-command = Command

d-u2s46hxm-cwd = Cwd

d-5axs3ogs = Stopp
    .doc = Stopps the command that runs in the project now.
    .w-nothing-runs-in-the-project-now = Nothing runs in the project now

d-v6rwio5m = Ausführen again
    .doc = Ausführens a command of the project's terminal again, as a person does from its history.

d-v6rwio5m-run = Ausführen

d-nfvb5ymp = Ausführen the test again
    .doc = Ausführens one test of a run again: the command with the test's name after it, run as that run's tool ran it.

d-nfvb5ymp-run = Ausführen

d-nfvb5ymp-test = Test

d-dmtobwu4 = Ausführen

d-dmtobwu4-cwd = Cwd

d-dmtobwu4-command = Command

d-ndblrsrx = Terminal leeren
    .doc = Erases the terminal's history.

d-juebimhb = Ein Terminal öffnen
    .doc = Opens the project's folder in the system's terminal: the shell a person runs by hand.

d-jkmncbqs = Ausgeführt
    .doc = The command a job runs ended, or moved: its output, exit status and truncated flag are kept with the command, its output's places become items, and a failure is the window's trouble.

d-jkmncbqs-j = J

d-o5c4jbiz = Gesucht
    .doc = The project search ended, or moved: each hit it found is added as it comes, and the files it searched are counted.

d-o5c4jbiz-j = J

d-7weouyp2 = Poll
    .doc = Asks how far the language server has got, so its state, problems and answers reach the window.

d-mqipns37 = Go to it
    .doc = Brings the caret to the place an item of a command's output names: the file is read if the project does not hold it yet, and the page shows it.
    .w-that-line-names-no-file = That line names no file

d-a7tfyjpe = Suchen the project
    .doc = Suchenes every file of the project for `needle`, the case of its letters aside unless `case` says otherwise.
    .w-type-something-to-find = Type something to find

d-a7tfyjpe-needle = Needle

d-a7tfyjpe-case = Case

d-a7tfyjpe-word = Word

d-5lsivooa = Stopp searching
    .doc = Stopps the project search.

d-ivfwweyt = Open the place
    .doc = Shows the place a search found: the file is read from the device when the project does not hold it yet, and the page opens it.

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

d-2p3qz2ll = Vorschlagen
    .doc = Asks the server for completions at a place in a file.

d-2p3qz2ll-entry = Entry

d-2p3qz2ll-line = Zeile

d-2p3qz2ll-column = Column

d-3cpxvmxh = Was hier steht
    .doc = Asks the server what stands at a place in a file.

d-3cpxvmxh-entry = Entry

d-3cpxvmxh-line = Zeile

d-3cpxvmxh-column = Column

d-nkwsrijh = Zur Definition
    .doc = Asks the server where the declaration at a place is written.

d-nkwsrijh-entry = Entry

d-nkwsrijh-line = Zeile

d-nkwsrijh-column = Column

d-pnged4ux = Write the suggestion
    .doc = Writes the completion a person chose where the caret stands.

d-pnged4ux-entry = Entry

d-fu3y2v7i = Formatieren the file
    .doc = Reformats a file as its language's formatter does, as one change.
    .w-a-folder-holds-no-text = A folder holds no text

d-fu3y2v7i-entry = Entry

d-cph2enjs = Sprache servers
    .doc = Turns language servers on or off, for the project.

d-cph2enjs-servers = Servers

d-m5tkeabl = Formatieren on save
    .doc = Turns formatting a file on save on or off.

d-m5tkeabl-format = Formatieren

d-m3cwqb2r = Erneut versuchen
    .doc = Stopps the language server, as a person pressing "Erneut versuchen" after a failure does.

d-l7lwb6q5 = Add child
    .doc = Starts a file inside the folder `e`: a row's "+", pressed.

d-baaey4sk = Neue Datei~present

d-2wzefmvk = Neuer Ordner~present

d-p3dsnwpy = Umbenennen~present

d-eqkywfls = Verschieben entry~present

d-cogoamzq = Verschieben after~present

d-j4nd4j3a = Set language~present

d-samgh7oc = Ersetzen text~present

d-24fjpwqk = Ersetzen range~present

d-3wawneyr = Zeile duplizieren~present

d-ijkczbmv = Verschieben line~present

d-u2y5ffw6 = Löschen line~present

d-6r5pq3fu = Kommentar umschalten~present

d-75tpm4zs = Einrücken line~present

d-se2ooaqw = Ausrücken line~present

d-lmrtkaxd = Check~present

d-jxe2rmpm = Speichern~present

d-33j2f7gw = Speichern into~present

d-fpjxwhph = Verschieben on disk~present

d-qfnk3hny = Ausführen~present

d-enlvxjgl = Stopp~present

d-unzcdvgw = Reveal item~present

d-pxdbowga = Formatieren file~present

d-yspw5xt2 = Datei row

d-liefjfl2 = Sprache menu
    .w-language = Sprache

d-zw7y7jba = Add inside
    .w-new-file = Neue Datei

d-qe5s5jlf = Projekt head
    .w-project = Projekt
    .w-open-a-folder = Einen Ordner öffnen…
    .w-new-file = Neue Datei
    .w-new-folder = Neuer Ordner
    .w-save-all = Speichern all
    .w-search-the-project = Suchen the project…
    .w-open-a-terminal = Ein Terminal öffnen

d-744hb56m = Projekt
    .w-no-files-yet-a-project-starts = Noch keine Dateien: a project starts with its first file
    .w-unsaved = Ungespeichert
    .w-changed-on-disk = Auf der Platte geändert
    .w-file = Datei
    .w-save = Speichern
    .w-save-as = Speichern as…
    .w-reload-from-disk = Von der Platte neu laden
    .w-format = Formatieren
    .w-duplicate-line = Zeile duplizieren
    .w-delete-line = Löschen line
    .w-toggle-comment = Kommentar umschalten
    .w-check-the-file = Die Datei prüfen
    .w-suggest = Vorschlagen
    .w-what-stands-here = Was hier steht
    .w-go-to-definition = Zur Definition
    .w-replace-all = Ersetzen all
    .w-panel = Bereich
    .w-problems = Probleme
    .w-terminal = Terminal
    .w-search-results = Suchen results
    .w-write-the-file = Write the file
    .w-line = line { $value }
    .w-unsaved-changes = Ungespeichert changes
    .w-1-problem = 1 problem
    .w-problems-2 = { $len } problems
    .w-try-again = Erneut versuchen
    .w-ln-col = Ln { $value }, Col { $value2 }
    .w-suggestions = Vorschlagenions
    .w-definitions = Definitions
    .w-choose-a-file-to-read-it = Choose a file to read it
    .w-go-to-line = Zu Zeile springen
    .w-hide-panel = Panel ausblenden
    .w-line-2 = Zeile { $value }

d-6jwzz6bh = Explorer footer
    .w-a-project-in-the-workspace-alone = A project in the workspace alone

d-7yzd7qdh = Probleme panel
    .w-this-file-has-not-been-checked = This file has not been checked yet
    .w-check-the-file = Die Datei prüfen
    .w-by = { $problems_said }, by { $source }
    .w-line-column = line { $value }, column { $value2 }

d-zl3bj7gd = Terminal panel
    .w-run-a-command-in-the-project = Ausführen a command in the project
    .w-run = Ausführen
    .w-stop = Stopp
    .w-clear = Clear
    .w-no-command-has-run-in-the = No command has run in the project
    .w-run-again = Ausführen again
    .w-terminal = Terminal
    .w-open-a-terminal = Ein Terminal öffnen
    .w-clear-the-terminal = Terminal leeren

d-2lxnmz5v = Suchen panel
    .w-stop = Stopp

d-d3ucmsac = Gefunden
    .w-found-in-this-file = In dieser Datei gefunden
    .w-line = Zeile { $value }

d-bbheeez5 = Datei view
    .w-write-the-file = Write the file

d-seqze3hc = Dateis
    .w-no-files-yet = Noch keine Dateien

d-erf5uzfi = Commands
    .w-no-command-has-run-in-the = No command has run in the project

d-ynyb6yyr = Command view
    .w-ran-in = Ran in
    .w-state = State

d-zu6g3bmd = Zeile anzeigen
    .doc = Die Zeile und Spalte, die jemand sehen wollte, ab 0 gezählt, und wie oft sie danach fragte: der Editor zeigt diese Stelle und fragt dieselbe Stelle erneut, wenn der Zähler sich ändert.

d-k4etx7ch = Spalte anzeigen

d-wtzldj2n = Anzeigeanfrage

d-m2evo7on-filter-reveal_line = Zeile anzeigen

d-m2evo7on-filter-reveal_column = Spalte anzeigen

d-m2evo7on-filter-reveal_ask = Anzeigeanfrage

d-llbxpk2e = Serverfortschritt
    .doc = Die Arbeit, die der Server gerade tut, so wie die Engine sie meldet (`Indexing`, 42).
    .noun = { $count ->
        [one] Serverfortschritt
       *[other] Serverfortschritte
    }

d-b22hm43k = Titel

d-33vetzuz = Detail

d-xvz6ifep = Prozent

d-llbxpk2e-filter-percent = Prozent

d-uaoqlskg-filter-progress = Fortschritt

d-albctzgc = Zum Symbol springen
    .doc = Die Deklarationen, die der Text einer Datei schreibt (`fn`, `struct`, `enum`, `trait`, `class`, `def`, `interface`, `type`, `const`), jede mit Zeile und Spalte, an der sie steht: was die Symbole des Projekts sind und was „Zum Symbol springen“ anbietet.

d-vmmfvt6f-answer = Antwort

d-mqipns37-path = Pfad

d-mqipns37-line = Zeile

d-373nszrk = Zu Zeile springen
    .doc = Zeigt `line` von `entry`, ab 0 gezählt: der Editor scrollt sie ins Bild, mit dem Cursor darauf.

d-373nszrk-line = Zeile

d-373nszrk-column = Spalte

d-ube34ufe = Zu Zeile springen
    .doc = Zeigt `line` der Datei unter `path`, ab 0 gezählt.

d-ube34ufe-path = Pfad

d-ube34ufe-line = Zeile

d-ivfwweyt-path = Pfad

d-ivfwweyt-line = Zeile

d-pnged4ux-label = Beschriftung

d-pnged4ux-insert = Einfügen
