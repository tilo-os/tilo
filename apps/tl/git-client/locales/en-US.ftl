# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-git-client = Git

d-so-tilo-git-client-keywords = git, repository, commit, branch, merge, diff, stage, stash, remote, version control, quellcode, versionierung, zweig

d-bgacivdi = Hunk
    .doc = One hunk of a diff, which can be staged or discarded on its own.
    .noun = { $count ->
        [one] hunk
       *[other] hunks
    }

d-a7uhrt52 = Index

d-5ccadzg6 = Header

d-dfethllq = Old start

d-pigkwwm6 = New start

d-bgacivdi-filter-index = Index

d-bgacivdi-filter-old_start = Old start

d-bgacivdi-filter-new_start = New start

d-nf7rji7c = Change
    .doc = One changed file: where it stands, what became of it and its diff.
    .noun = { $count ->
        [one] change
       *[other] changes
    }

d-ulyz6e5m = Path

d-pyq3ne6a = Old path

d-gyf3ucvv = Area

d-a64nnfrl = Kind

d-pgbhcfmd = Additions

d-cb3hp5ts = Deletions

d-ddlu6eiw = Binary

d-547agrds = Ours
    .doc = What keeping each side of a conflict would discard, in lines.

d-gcyh3qk6 = Theirs

d-5bywp6ty = Language
    .doc = The file's language token (`rust`, `json`, ...), for the code view's colouring.

d-2ejdsikb = Unified
    .doc = The change in one column: each line's old and new number, its marker and its words.

d-bdjzphf3 = Left
    .doc = The old side of a side-by-side reading, aligned row for row with `right`.

d-qmisixex = Right
    .doc = The new side of a side-by-side reading.

d-qo6hyuiv = Hunks
    .doc = Its hunks, each as its own JSON text.

d-nf7rji7c-filter-area = Area

d-nf7rji7c-filter-kind = Kind

d-nf7rji7c-filter-additions = Additions

d-nf7rji7c-filter-deletions = Deletions

d-nf7rji7c-filter-binary = Binary

d-nf7rji7c-filter-ours = Ours

d-nf7rji7c-filter-theirs = Theirs

d-xp24lijk = Ref label
    .doc = One ref that points at a commit.
    .noun = { $count ->
        [one] ref label
       *[other] ref labels
    }

d-zl57j56i = Name

d-7lddqvmy = Kind

d-xp24lijk-filter-kind = Kind

d-ssa2n2ws = Commit
    .doc = One commit of the repository.
    .noun = { $count ->
        [one] commit
       *[other] commits
    }

d-ge4jaage = Id

d-ktcweris = Summary

d-qkodo2um = Body

d-mbs2zev2 = Author name

d-2rplzpux = Author email

d-g5wqmtlu = Time
    .doc = When it was made, in seconds since the epoch.

d-i5ezibch = Age
    .doc = How long ago that was, in seconds.

d-ae7mbonn = Date
    .doc = When it was made, as a machine reads it.

d-vngsr7bo = Parents

d-3k7gvvan = Refs

d-okssmwxo = Column

d-ssa2n2ws-filter-time = Time

d-ssa2n2ws-filter-age = Age

d-ssa2n2ws-filter-column = Column

d-uz3urzuh = Branch
    .doc = One branch of the repository.
    .noun = { $count ->
        [one] branch
       *[other] branches
    }

d-mmht373l = Name

d-2xd7n6ck = Is head

d-ejqxjylh = Is remote

d-vkfjbs4a = Upstream

d-uvswvf53 = Ahead

d-awh54bp4 = Behind

d-fkwngzoz = Tip

d-uz3urzuh-filter-is_head = Is head

d-uz3urzuh-filter-is_remote = Is remote

d-uz3urzuh-filter-ahead = Ahead

d-uz3urzuh-filter-behind = Behind

d-erag4w44 = Tag
    .doc = One tag of the repository.
    .noun = { $count ->
        [one] tag
       *[other] tags
    }

d-eimgqjmn = Name

d-rvt4lxwy = Target

d-qpus5rj2 = Message

d-is4hlega = Stash
    .doc = One stash of the working tree.
    .noun = { $count ->
        [one] stash
       *[other] stashes
    }

d-plxxvefl = Index

d-npo2d33u = Message

d-kr54kttq = Branch

d-is4hlega-filter-index = Index

d-s2coi475 = Remote
    .doc = One remote the repository fetches from and pushes to.
    .noun = { $count ->
        [one] remote
       *[other] remotes
    }

d-fi4gej3v = Name

d-ug544zky = Url

d-eozposlx = Worktree
    .doc = One working directory linked to the repository.
    .noun = { $count ->
        [one] worktree
       *[other] worktrees
    }

d-syhlyvff = Name

d-s43oholm = Path

d-zumfqfnq = Head

d-rcy5osnr = Current

d-eozposlx-filter-current = Current

d-b5uulgn7 = Commit file
    .doc = One file a commit changed, with its diff.
    .noun = { $count ->
        [one] commit file
       *[other] commit files
    }

d-elliscls = Path

d-tlnyi6uz = Old path

d-65rizway = Kind

d-7u7ngqkr = Additions

d-qdtrudtf = Deletions

d-4pwix6ke = Binary

d-vgnn6ynm = Language

d-j2nczx73 = Unified

d-kgmcwjbi = Left

d-xtgtu5ad = Right

d-b5uulgn7-filter-kind = Kind

d-b5uulgn7-filter-additions = Additions

d-b5uulgn7-filter-deletions = Deletions

d-b5uulgn7-filter-binary = Binary

d-bduvfbif = Browse entry
    .doc = One folder the repository picker listed.
    .noun = { $count ->
        [one] browse entry
       *[other] browse entries
    }

d-mazzivt3 = Name

d-67eqnwql = Path

d-amedesig = Folder

d-bduvfbif-filter-folder = Folder

d-njfqrkl5 = Session
    .doc = One person's window on the repository: the repository they opened here, its working tree, and how they read diffs.
    .noun = { $count ->
        [one] session
       *[other] sessions
    }

d-vrfkfgub = Path
    .doc = The path of the repository this person opened here; empty until they open one.

d-qtrusa64 = Changes

d-qakm3prq = Layout

d-5pb4bgxw = Highlight

d-m5eoofqc = Whitespace

d-mk3evbqh = Wrap

d-ma5zpjxi = Pending switch
    .doc = The branch a switch waits on, while the working tree holds uncommitted work.

d-taytqzee = Browsing
    .doc = The picker's folder and entries, while the person chooses where a repository is.

d-cxuhuns5 = Folder

d-ermpuo4w = Entries

d-gpp2fllc = Commit id
    .doc = The commit whose changed files were read, and those files as a JSON list of texts.

d-7ihfqmuw = Commit files

d-mr62vl75 = Notice
    .doc = What the last action did, said once; what went wrong, said until it is put away.

d-hjgppyqi = Trouble

d-njfqrkl5-filter-layout = Layout

d-njfqrkl5-filter-highlight = Highlight

d-njfqrkl5-filter-whitespace = Whitespace

d-njfqrkl5-filter-wrap = Wrap

d-njfqrkl5-filter-browsing = Browsing

d-3peaa3l5 = Repository
    .doc = The workspace's Git: the repository it opens and what members see of it, and one window per person on it.
    .noun = { $count ->
        [one] repository
       *[other] repositories
    }

d-n4wepl5s = Path
    .doc = Where the repository is on this device.

d-6ju4ea2u = Name

d-ori4pxhp = Head

d-nqllokpk = Workdir

d-ev43omls = Author name

d-6ejc2liq = Author email

d-os3blsxu = Snapshot
    .doc = Everything the device answered about the repository, as the text it wrote: its commits, branches, tags, stashes, remotes and working directories.

d-vioio4dz = Sessions

d-cdrt77b4 = Loaded
    .noun = { $count ->
        [one] loaded
       *[other] loadeds
    }

d-u66layc7 = Answer
    .doc = What the op that answered it made, in a few words: a commit's or stash's short id.

d-dbwyzz4f = Name

d-bhabfybv = Head

d-ifdrbibf = Workdir

d-eptgnppa = Author name

d-zqo4qf2p = Author email

d-dijqjokd = Changes
    .doc = Each collection as a list of JSON texts, one an item.

d-ylfrksiq = Commits

d-lypvtyhb = Branches

d-g4pu5gfp = Tags

d-cvskkv3u = Stashes

d-sq36vhyn = Remotes

d-izkg5khe = Worktrees

d-t3ptayvw = Worktree error

d-533ctb76 = Wire change
    .noun = { $count ->
        [one] wire change
       *[other] wire changes
    }

d-37sxq5bs = Path

d-dvptb6yi = Old path

d-m6m22ctl = Area

d-rck5ywq4 = Kind

d-7anyihpj = Additions

d-kvrcmb3v = Deletions

d-7dddbyyj = Binary

d-zzjs6njo = Ours

d-ej3kja37 = Theirs

d-tlea3vto = Language

d-hr5ogyyj = Unified

d-edfwguh5 = Left

d-2kp7i4ab = Right

d-h6xzvamk = Hunks

d-533ctb76-filter-area = Area

d-533ctb76-filter-kind = Kind

d-533ctb76-filter-additions = Additions

d-533ctb76-filter-deletions = Deletions

d-533ctb76-filter-binary = Binary

d-533ctb76-filter-ours = Ours

d-533ctb76-filter-theirs = Theirs

d-eqy5lhxf = Wire commit
    .noun = { $count ->
        [one] wire commit
       *[other] wire commits
    }

d-5ycbn6i4 = Id

d-ugphvrb2 = Summary

d-hg67fbg2 = Body

d-cjxgeuao = Author name

d-ypiqlu3b = Author email

d-3mge2zfz = Time

d-uyueykhv = Age

d-o4wtnyc7 = Date

d-o5fnmdlp = Parents

d-oz6i3qfk = Refs

d-firuj3kf = Column

d-eqy5lhxf-filter-time = Time

d-eqy5lhxf-filter-age = Age

d-eqy5lhxf-filter-column = Column

d-pwhnois3 = Wire ref
    .noun = { $count ->
        [one] wire ref
       *[other] wire refs
    }

d-cgcdv4xh = Name

d-wlrzlclq = Kind

d-pwhnois3-filter-kind = Kind

d-jizhu6al = Wire branch
    .noun = { $count ->
        [one] wire branch
       *[other] wire branches
    }

d-iwhrqigk = Name

d-lnt6kiuo = Is head

d-i4erkio3 = Is remote

d-ej7g53n3 = Upstream

d-vupfshru = Ahead

d-koq4jc7k = Behind

d-mfbh6kyh = Tip

d-jizhu6al-filter-is_head = Is head

d-jizhu6al-filter-is_remote = Is remote

d-jizhu6al-filter-ahead = Ahead

d-jizhu6al-filter-behind = Behind

d-7kdb2p7c = Wire tag
    .noun = { $count ->
        [one] wire tag
       *[other] wire tags
    }

d-nsgs47is = Name

d-yn67zzls = Target

d-uju3pds5 = Message

d-hjphbbmv = Wire stash
    .noun = { $count ->
        [one] wire stash
       *[other] wire stashes
    }

d-zyrzfh5t = Index

d-bd32e2q6 = Message

d-f3lmbvrn = Branch

d-hjphbbmv-filter-index = Index

d-3dq47wrt = Wire remote
    .noun = { $count ->
        [one] wire remote
       *[other] wire remotes
    }

d-d577aqii = Name

d-ihlupgbv = Url

d-pvxnlfqr = Wire worktree
    .noun = { $count ->
        [one] wire worktree
       *[other] wire worktrees
    }

d-htbitpma = Name

d-f3a3kvnf = Path

d-cqc2kufj = Head

d-dvbe2xkw = Current

d-pvxnlfqr-filter-current = Current

d-yzovkqfv = Wire file
    .noun = { $count ->
        [one] wire file
       *[other] wire files
    }

d-44l2jnvk = Path

d-fr7rebu2 = Old path

d-qggzylu7 = Kind

d-b4js3rjp = Additions

d-ivsbutw6 = Deletions

d-f4hq5otl = Binary

d-bjqxfixo = Language

d-o6jj727v = Unified

d-oc5lrtao = Left

d-vmcjzzz5 = Right

d-yzovkqfv-filter-kind = Kind

d-yzovkqfv-filter-additions = Additions

d-yzovkqfv-filter-deletions = Deletions

d-yzovkqfv-filter-binary = Binary

d-adg4z6qi = Path ask
    .noun = { $count ->
        [one] path ask
       *[other] path asks
    }

d-aqyg5aef = Path

d-dioumhuy = Whitespace
    .doc = Whether a diff shows the whitespace at the end of its lines.

d-adg4z6qi-filter-whitespace = Whitespace

d-7wb3dvw6 = File ask
    .noun = { $count ->
        [one] file ask
       *[other] file asks
    }

d-qahiprkz = Path

d-vywcliib = File

d-lcmux77m = Hunk ask
    .noun = { $count ->
        [one] hunk ask
       *[other] hunk asks
    }

d-nh7yqayy = Path

d-vfjaobn2 = File

d-tprahdo6 = Hunk

d-7pgxgdw3 = Unstage

d-lcmux77m-filter-hunk = Hunk

d-lcmux77m-filter-unstage = Unstage

d-ewgh6vvx = Commit ask
    .noun = { $count ->
        [one] commit ask
       *[other] commit asks
    }

d-ofstus7y = Path

d-bxihpw7v = Message

d-oypyxpzg = Branch ask
    .noun = { $count ->
        [one] branch ask
       *[other] branch asks
    }

d-fi4zi42q = Path

d-c4fbjyom = Name

d-l7meu2gh = Checkout

d-oypyxpzg-filter-checkout = Checkout

d-ha25kgjn = Checkout ask
    .noun = { $count ->
        [one] checkout ask
       *[other] checkout asks
    }

d-klf3gt64 = Path

d-rpchrkvk = Name

d-cuml3cz6 = Stash

d-ha25kgjn-filter-stash = Stash

d-vumkq7wu = Commit files ask
    .noun = { $count ->
        [one] commit files ask
       *[other] commit files asks
    }

d-3w5wb2ew = Path

d-vbqr55tj = Commit

d-nrx2nxsc = Worktree ask
    .noun = { $count ->
        [one] worktree ask
       *[other] worktree asks
    }

d-ukwgoqln = Path

d-py6thmfz = Destination

d-rwocyieo = Branch

d-ibmiafbe = Start

d-obk6vca4 = Stash save ask
    .noun = { $count ->
        [one] stash save ask
       *[other] stash save asks
    }

d-yeg5b74s = Path

d-igkwc6uc = Message

d-h2kfi5de = Stash apply ask
    .noun = { $count ->
        [one] stash apply ask
       *[other] stash apply asks
    }

d-y3nby6jr = Path

d-eoe3gfz5 = Index

d-h2kfi5de-filter-index = Index

d-7oa4a4o4 = Resolve ask
    .noun = { $count ->
        [one] resolve ask
       *[other] resolve asks
    }

d-yltk3frx = Path

d-o2dbxhal = File

d-qvivrkzl = Ours

d-7oa4a4o4-filter-ours = Ours

d-3lwjrl2s = Remote ask
    .noun = { $count ->
        [one] remote ask
       *[other] remote asks
    }

d-gifph57t = Path

d-tz3p3qeb = Remote

d-uzzqrnrx = Change area
    .doc = Where a change stands: in the next commit, still in the working tree, or not tracked yet.

d-37k7idbc = Staged

d-w2lkndlx = Unstaged

d-6fdncnb6 = Untracked

d-4uryee3o = Change kind
    .doc = What became of a file.

d-y36zq2bw = Added

d-4keolvna = Modified

d-lwiydt7j = Deleted

d-66cy3vrg = Renamed

d-hwlxq3oi = Untracked

d-gxg2bwkg = Conflicted

d-leotiz2h = Ref kind
    .doc = What a ref on a commit is.

d-lqphpv2e = Head

d-axu5fcnu = Local branch

d-7er7phvj = Remote branch

d-tjfal3xx = Tag

d-drarr3v2 = Diff layout
    .doc = How a diff reads.

d-fowmhd76 = Unified

d-27umt2ce = Split

d-sevk4ejg = Arrive
    .doc = Opening the app gives the person their window on it, though they open no repository yet.

d-noccxekz = Refresh
    .doc = Reads the repository on disk into the app's records, for this person's window.

d-hftffvs5 = Open repository
    .doc = Opens the repository at `path` on this device.

d-hftffvs5-path = Path

d-537xcsny = Choose repository
    .doc = Starts choosing a repository's folder, listing this device's home folder.

d-2eebbcq4 = Read folder
    .doc = Lists `folder` in the repository picker.

d-2eebbcq4-folder = Folder

d-cdmdeeny = Open browsed
    .doc = Opens the folder the picker shows as a repository.

d-nyioin33 = Close picker
    .doc = Puts the picker away.

d-2tto3qfj = Stage file
    .doc = Stages one changed file.

d-2tto3qfj-c = C

d-by2ln3ko = Unstage file
    .doc = Takes one staged file back out of the next commit.

d-by2ln3ko-c = C

d-xleo3or5 = Discard file changes
    .doc = Discards the working tree's edits to one file, or deletes it when it was never tracked.

d-xleo3or5-c = C

d-r3j7bakj = Stage all
    .doc = Stages every change.

d-ajzbhhzn = Unstage all
    .doc = Takes every staged change back out of the next commit.

d-t6hvr46v = Stage hunk
    .doc = Stages one hunk of a file.

d-t6hvr46v-c = C

d-t6hvr46v-hunk = Hunk

d-rliycbbj = Unstage hunk
    .doc = Takes one staged hunk back out of the next commit.

d-rliycbbj-c = C

d-rliycbbj-hunk = Hunk

d-p3tvpthq = Discard this change
    .doc = Discards one hunk of a file's working tree edits.

d-p3tvpthq-c = C

d-p3tvpthq-hunk = Hunk

d-24f7tx6t = Commit staged changes
    .doc = Creates a commit from what is staged.
    .w-a-commit-has-a-message = A commit has a message
    .w-there-are-no-staged-changes-to = There are no staged changes to commit

d-24f7tx6t-message = Message

d-33dk6utz = Keep a side
    .doc = Keeps one side of a conflicted file and stages it as resolved.

d-33dk6utz-c = C

d-33dk6utz-ours = Ours

d-pzeglzgm = Create branch
    .doc = Creates a branch and switches to it.
    .w-a-branch-has-a-name = A branch has a name

d-pzeglzgm-name = Name

d-ipza5mm5 = Checkout branch
    .doc = Switches to a branch, asking what becomes of uncommitted work when there is any.

d-ipza5mm5-name = Name

d-mjiefmgr = Switch stashing
    .doc = Answers the switch question by parking the uncommitted work on the stash first.

d-uvji2mjq = Switch carrying
    .doc = Answers the switch question by carrying the uncommitted work along.

d-h2e25sqj = Cancel switch
    .doc = Puts the switch question away.

d-svlikb4k = Switch to
    .doc = Switches to `name`, parking the uncommitted work when `stash` says so.

d-svlikb4k-name = Name

d-svlikb4k-stash = Stash

d-vvjj7n6v = Delete branch
    .doc = Deletes a local branch.

d-vvjj7n6v-name = Name

d-mu2sgzjp = Stash working changes
    .doc = Stashes the working tree's changes, with a message.

d-mu2sgzjp-message = Message

d-wboirbaa = Apply stash
    .doc = Applies a stash, leaving it in the list.

d-wboirbaa-index = Index

d-kn6abrns = Create worktree
    .doc = Makes a new branch at `start` and a working directory of its own.
    .w-a-worktree-branch-has-a-name = A worktree branch has a name
    .w-a-worktree-has-a-destination-path = A worktree has a destination path
    .w-a-worktree-starts-somewhere = A worktree starts somewhere

d-kn6abrns-path = Path

d-kn6abrns-branch = Branch

d-kn6abrns-start = Start

d-7lpwxrhi = Fetch from remote
    .doc = Fetches from a remote.

d-7lpwxrhi-remote = Remote

d-c2uabswh = Pull from remote
    .doc = Fetches, then fast-forwards the current branch to its upstream.

d-c2uabswh-remote = Remote

d-aqzo32et = Push to remote
    .doc = Pushes the current branch to a like-named branch on a remote.

d-aqzo32et-remote = Remote

d-vbt5h6db = Read commit
    .doc = Reads a commit's changed files and their diffs when it is opened.

d-vbt5h6db-id = Id

d-umhtkuee = Diff layout
    .doc = Switches how this person reads diffs: one column or two.

d-umhtkuee-s = S

d-umhtkuee-layout = Layout

d-idxn62yl = Syntax highlighting
    .doc = Turns syntax colouring on or off for this person.

d-idxn62yl-s = S

d-idxn62yl-on = On

d-fcrm23io = Show whitespace
    .doc = Shows or hides the whitespace at the end of diff lines for this person.

d-fcrm23io-s = S

d-fcrm23io-on = On

d-su4j4r5w = Wrap long lines
    .doc = Wraps long diff lines or lets them run wider than the pane, for this person.

d-su4j4r5w-s = S

d-su4j4r5w-wrap = Wrap

d-247oj4wo = Clear trouble
    .doc = Puts the trouble banner away.

d-eqtvrcxw = Clear notice
    .doc = Puts the notice banner away.

d-pfjepyyc = Asks
    .doc = Runs `op` with `ask` and keeps the answer: the repository as it now stands, and the working tree as the device read it.

d-pfjepyyc-s = S

d-pfjepyyc-op = Op

d-pfjepyyc-ask = Ask

d-lre4z2mt = Commit~present

d-qmat4wnw = Create branch~present

d-u3txosuf = Create worktree~present

d-464a2hax = Changes
    .w-changes = Changes
    .w-git = Git
    .w-refresh = Refresh
    .w-fetch = Fetch
    .w-pull = Pull
    .w-push = Push
    .w-diff-layout = Diff layout
    .w-syntax-highlighting = Syntax highlighting
    .w-show-whitespace = Show whitespace
    .w-wrap-long-lines = Wrap long lines
    .w-stash-changes = Stash changes
    .w-stage-all = Stage all
    .w-unstage-all = Unstage all
    .w-commit-message = Commit message
    .w-commit = Commit
    .w-commit-to = Commit to { $head }
    .w-your-working-tree-is-clean = Your working tree is clean
    .w-unstage-file = Unstage file
    .w-stage-file = Stage file
    .w-discard-file-changes = Discard file changes
    .w-select-a-file-to-see-its = Select a file to see its changes
    .w-switch-to = Switch to { $pending_switch }?
    .w-you-have-uncommitted-changes-stash-them = You have uncommitted changes. Stash them and switch, or keep them; switching fails when a change would be overwritten.
    .w-stash-and-switch = Stash and switch
    .w-cancel = Cancel
    .w-keep-my-changes = Keep my changes

d-sbvmzuej = Change row
    .w-unstage = Unstage
    .w-unstage-2 = Unstage { $path }
    .w-stage = Stage
    .w-stage-2 = Stage { $path }

d-f5rw6soc = Change detail
    .w-renamed-from = Renamed from { $old }
    .w-unstage-file = Unstage file
    .w-stage-file = Stage file
    .w-discard-file = Discard file
    .w-both-sides-changed-this-file = Both sides changed this file
    .w-keeping-ours-drops-lines-of-theirs = Keeping ours drops { $theirs } lines of theirs; keeping theirs drops { $ours } lines of yours.
    .w-take-ours = Take ours
    .w-take-theirs = Take theirs
    .w-unstage-hunk = Unstage hunk
    .w-stage-hunk = Stage hunk
    .w-discard-hunk = Discard hunk

d-ci5fxsvq = Change code
    .w-binary-file = Binary file
    .w-there-is-no-text-to-compare = There is no text to compare.
    .w-no-changes-to-show = No changes to show
    .w-only-the-file-s-permissions-changed = Only the file's permissions changed.

d-x4ftmy7x = Code column

d-37sjstuv = Open page
    .w-open-a-repository = Open a repository
    .w-enter-the-path-of-a-git = Enter the path of a git repository on this computer.
    .w-home-you-project = /home/you/project
    .w-open = Open
    .w-choose = Choose…

d-hqkjgisz = Picker
    .w-open-this-folder = Open this folder
    .w-cancel = Cancel

d-m3qefblf = History
    .w-history = History
    .w-git = Git
    .w-no-commits-yet = No commits yet

d-eu2tlikq = Commit row

d-i4z2l7f7 = Commit page
    .w-commit = Commit
    .w-date = Date
    .w-parents = Parents
    .w-branches-and-tags = Branches and tags
    .w-changed-files = Changed files

d-fxxvyq63 = Commit file
    .w-binary-file = Binary file
    .w-there-is-no-text-to-compare = There is no text to compare.
    .w-no-changes-to-show = No changes to show
    .w-only-the-file-s-permissions-changed = Only the file's permissions changed.

d-m4t7437c = Branches
    .w-branches = Branches
    .w-git = Git
    .w-new-branch-name = New branch name
    .w-create-branch = Create branch
    .w-remote-branches = Remote branches
    .w-working-directories = Working directories
    .w-new-worktree-branch = New worktree branch
    .w-absolute-destination-path = Absolute destination path
    .w-start-point-branch-tag-or-commit = Start point (branch, tag or commit)
    .w-create-worktree = Create worktree
    .w-tags = Tags
    .w-stashes = Stashes
    .w-remotes = Remotes
    .w-fetch = Fetch
    .w-pull = Pull
    .w-push = Push

d-kz5j2n5t = Branch row
    .w-delete = Delete
    .w-delete-2 = Delete { $name }
