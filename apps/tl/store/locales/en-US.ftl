# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-store = App Store

d-so-tilo-store-keywords = store, apps, install, update, uninstall, get, flatpak, registry, repositories, bots

d-wtooim2w = Version
    .doc = One signed version of an app, as its source offers it.
    .noun = { $count ->
        [one] version
       *[other] versions
    }

d-tv4yjmd7 = Version
    .doc = The version, as its source writes it.

d-2jsas2xh = Released
    .doc = The day its source published it.

d-xzuri67c = Root
    .doc = The signed package root, hex.

d-pryqr455 = Surface
    .doc = The signature the package itself carries, hex.

d-j7j23dzx = Size
    .doc = Its bytes, as its source states them.

d-t73ddwug = Runtime
    .doc = The runtime reference it needs, when it names one.

d-a6avsz6m = Download
    .doc = Where its package file downloads from, when its source names a place.

d-wtooim2w-filter-size = Size

d-3iyqd2ry = Permission
    .doc = One thing an app can do on this computer, as its source says.
    .noun = { $count ->
        [one] permission
       *[other] permissions
    }

d-yrcnogu3 = What
    .doc = What it asks for, in the source's own words: `network`, `files:all`.

d-znsz4dgp = Broad
    .doc = Whether the request reaches everything rather than what a person chooses.

d-3iyqd2ry-filter-broad = Broad

d-cqetz3au = Release
    .doc = What changed in one version of an app, as its source says.
    .noun = { $count ->
        [one] release
       *[other] releases
    }

d-dbblppxc = Version

d-sil2634m = Notes

d-thnissxq = App
    .doc = One app the catalogue knows: what it is, where it comes from, what it offers, and how far it is on this workspace's devices.
    .noun = { $count ->
        [one] app
       *[other] apps
    }

d-bsz5kepr = Identity
    .doc = The app's identity: its package id.

d-z7s7wy7i = Name

d-7mugzbgk = Summary
    .doc = The one line a row shows, the tagline or else the description's first line.

d-xmhc7rzm = Description

d-6igcsadc = Developer

d-y6tadyrs = License

d-s2h3qmpk = Categories

d-z2ymswmt = Icon
    .doc = Its icon, as a web address the device fetches.

d-fbg5gcf6 = Screenshots

d-iuswhjxs = Homepage

d-dmpicgut = Source code

d-dyqo2u55 = Origin

d-gkbbivsc = Registry
    .doc = The registry or repository it came through, as people read it.

d-febktfoe = Branch
    .doc = The branch a Flatpak repository builds it in.

d-qay7cvdi = User managed
    .doc = Whether this workspace may change a repository app: a computer's own Flatpak repository is its administrator's.

d-hptnn5eg = Trust

d-kqqhh6as = Versions

d-sx2epvkk = Available
    .doc = The versions its source offers, newest first, as text.

d-rssj2htx = Installed
    .doc = The version this workspace runs, empty when it runs none.

d-qqimyoka = Pinned
    .doc = Whether this workspace keeps the version it runs.

d-ev27rmap = Fetched
    .doc = The version this computer has fetched for the workspace.

d-lmny564e = Runnable
    .doc = Whether its source builds it for this computer.

d-54f6mqrf = Processors
    .doc = The processors it is built for, in the source's words, when this computer is not one.

d-qbk2t2ii = Awaiting developer
    .doc = Its builds all speak an older interface than this system serves.

d-fmc644bz = Featured
    .doc = Its place in its source's featured list, smaller first; 0 when it is not featured.

d-5vq3mjxe = Updated
    .doc = The day its source last changed it.

d-aeiz77yf = Publisher

d-h6fnaygl = Digest
    .doc = The digest of the package this computer fetched, hex.

d-7tdammi6 = Bot
    .doc = A directory bot: what it does and needs, and no version to fetch on this computer.

d-rnazc2e5 = Bot triggers

d-su65i54b = Bot apps

d-4qzdclse = Bot connectors
    .doc = Its connectors, each as "name, version or newer".

d-ckhofqel = Bot appearance

d-ehbiqgtb = Permissions
    .doc = What it asks for, once its page read its source's details.

d-foea7x37 = Releases

d-fngfv6n4 = Size
    .doc = The bytes its source downloads, when it says.

d-25c7mbdv = Reference
    .doc = The `.flatpakref` it was opened from, when it was.

d-tdkfnlcz = Runtime
    .doc = The runtime it needs, and the repository that offers it.

d-qnnzjwd4 = Runtime registry

d-babs5eju = Job
    .doc = The change running for it, by the engine's job id.

d-66hvm5fy = Details read
    .doc = Whether its page read what its source says about it.

d-7l5m7ka6 = Updatable
    .doc = Whether a source offers a newer version than this computer runs.

d-awr7kqfn = Trouble
    .doc = Why this computer cannot change it, in the source's words; empty when it can.

d-thnissxq-filter-discover = Discover

d-thnissxq-filter-updates = Updates

d-thnissxq-filter-on_this_device = On this device

d-thnissxq-filter-bots = Bots

d-thnissxq-filter-categories = Categories

d-b67eswak = Remote
    .doc = A Flatpak repository, as this computer and this workspace offer it.
    .noun = { $count ->
        [one] remote
       *[other] remotes
    }

d-q6znklmy = Name

d-e74rqhdi = Title

d-yimcob4p = Url

d-7o2l4sfb = Scope
    .doc = "device" or "account": whose repository it is.

d-gbitboxx = Enabled

d-ixnjbq5u = Trouble
    .doc = Why its catalogue could not be read, when it could not.

d-mdalqw5g = Count
    .doc = How many apps its catalogue holds, once it was read.

d-pag3getc = Loaded
    .doc = Whether its apps are loaded.

d-b67eswak-filter-enabled = Enabled

d-b67eswak-filter-count = Count

d-b67eswak-filter-loaded = Loaded

d-ptgjb7qs = Block
    .doc = Someone's block of one app.
    .noun = { $count ->
        [one] block
       *[other] blocks
    }

d-qexvvrxm = Identity
    .doc = The app's identity.

d-cyq6y3du = Report
    .doc = A report about an app: a draft this workspace shares until someone submits it, then the registry's case and its status.
    .noun = { $count ->
        [one] report
       *[other] reports
    }

d-i2w5sqnt = Identity
    .doc = The app's identity.

d-tmhftkva = Note
    .doc = What the person saw, as evidence another reviewer can check.

d-riwxceey = Status
    .doc = The registry's status: "Not submitted" until it answers.

d-edrxm3vo = Case
    .doc = The registry's case id, once there is one.

d-sv5euoyy = Operation
    .doc = The signed operation the registry accepted, so a refresh or an appeal names exactly it.

d-sez3dgh3 = Registry
    .doc = The registry that holds the case.

d-r4polcvt = Store
    .doc = The workspace's App Store: the catalogue its sources offer, the repositories it configured, the reports people made, and each person's own blocks.
    .noun = { $count ->
        [one] store
       *[other] stores
    }

d-t6lkc6fh = Apps

d-gvveyw3n = Remotes

d-5g4uijlp = Reports

d-a6ibqgbw = Blocks

d-72gq3jue = Status
    .doc = What the last read of the sources came to.

d-sotfrvum = Trouble
    .doc = Why the last read of the sources failed, when it did.

d-eubzl4qx = Reading
    .doc = The read running now, by the engine's job id.

d-e22zgwb6 = Flatpak
    .doc = Whether this computer has Flatpak.

d-uxwwy2fl = Flatpak trouble
    .doc = Why this computer's Flatpak could not be read.

d-yut35juc = Unanswered
    .doc = The enabled registries that did not answer the last read.

d-y32wfrks = Expired
    .doc = The enabled registries whose catalogue has expired.

d-452zgqdl = Reviewed root
    .doc = The package someone handed the app, reviewed but not installed yet: its root, and the file it was read from.

d-5ynamzdl = Reviewed app

d-vihbdrl7 = Reviewed file

d-r4polcvt-filter-flatpak = Flatpak

d-r4polcvt-filter-unanswered = Unanswered

d-r4polcvt-filter-expired = Expired

d-h45lelxn = Item
    .doc = One app from a source, as the engine read it.
    .noun = { $count ->
        [one] item
       *[other] items
    }

d-epxtvzuf = Identity

d-6a7tsprp = Name

d-ea2bjyc3 = Summary

d-gpwdasim = Description

d-tns2iluv = Developer

d-2zltc7rw = License

d-ukzeb4vu = Categories

d-uqkifo7y = Icon

d-5kd5ubmn = Screenshots

d-elvkwauw = Homepage

d-okmosbps = Source code

d-mke4chcm = Origin

d-xbvh2wz3 = Registry

d-cpannwt7 = Branch

d-wikfckou = User managed

d-nwfophjp = Trust

d-i4hnlx7u = Versions
    .doc = The versions its source offers, each as its own JSON text.

d-ehs57hul = Available

d-x3qagtfz = Installed

d-36fsx2jx = Pinned

d-cnrxvmkp = Fetched

d-7jxbeqay = Runnable

d-nyj6afge = Processors

d-jsqjxd7b = Awaiting developer

d-taengdq6 = Featured

d-cvzbzfol = Updated

d-px6eamz7 = Publisher

d-rga5kuoh = Digest

d-na6yqlcj = Bot

d-lb5drfvf = Bot triggers

d-chtwyqcq = Bot apps

d-jcezfbrh = Bot connectors

d-cj5tnnei = Bot appearance

d-ojgbghet = Runtime

d-j5umq2va = Updatable

d-2nmtgjmg = Trouble

d-h45lelxn-filter-user_managed = User managed

d-h45lelxn-filter-pinned = Pinned

d-h45lelxn-filter-runnable = Runnable

d-h45lelxn-filter-awaiting_developer = Awaiting developer

d-h45lelxn-filter-featured = Featured

d-h45lelxn-filter-bot = Bot

d-h45lelxn-filter-bot_appearance = Bot appearance

d-h45lelxn-filter-updatable = Updatable

d-ljjwi6ge = Catalogue
    .doc = What one read of the sources found.
    .noun = { $count ->
        [one] catalogue
       *[other] catalogues
    }

d-mr6dwxv6 = Items
    .doc = Each app a source read, as its own JSON text.

d-onje62ck = Remotes
    .doc = Each repository, as its own JSON text.

d-ncdaffvy = Status

d-kntjub7s = Trouble

d-av6ojjws = Flatpak

d-xe2u37td = Flatpak trouble

d-elx5dznp = Unanswered

d-dhwcxbjv = Expired

d-ljjwi6ge-filter-flatpak = Flatpak

d-zvyjoyxd = Remote read
    .doc = One repository, as the engine read it.
    .noun = { $count ->
        [one] remote read
       *[other] remote reads
    }

d-rz4t6wig = Name

d-roitw26x = Title

d-635v2xjh = Url

d-xkljozjv = Scope

d-e4ylyjav = Enabled

d-rk5vdazr = Trouble

d-zvyjoyxd-filter-enabled = Enabled

d-bnyohoe2 = Details
    .doc = What a page's read of one app adds: the source's details.
    .noun = { $count ->
        [one] detail
       *[other] details
    }

d-6cb7jl47 = Description

d-7cwn55x3 = Developer

d-mptnhu62 = License

d-a37qv2oa = Homepage

d-uba5apij = Source code

d-nt6mule6 = Size

d-oj36amou = Screenshots

d-g3nhe33n = Permissions

d-lygslquo = Releases

d-whmuitft = Runtime

d-uijcroeo = Runtime registry

d-it2pk5w4 = Digest

d-bnyohoe2-filter-size = Size

d-g7icceba = Review
    .doc = A reviewed package, ready to install.
    .noun = { $count ->
        [one] review
       *[other] reviews
    }

d-mzd6fvgn = Item

d-hxcclid7 = Root

d-oiutpkg7 = Trouble

d-tvnwz7z4 = Case
    .doc = One repository's answer about a case.
    .noun = { $count ->
        [one] case
       *[other] cases
    }

d-zyv4ydev = Case

d-hf6ogyp3 = Status

d-o4zyd7r3 = Situation

d-zznn4lla = Trouble

d-7yi3ywbf = Prepared
    .doc = A report prepared for the registry: the operation it signs, and the registry that holds it.
    .noun = { $count ->
        [one] prepared
       *[other] prepareds
    }

d-wywxs5oj = Operation

d-zenjiysp = Registry

d-owkrdhjj = Trouble

d-l7f77ylc = Job
    .doc = How far the engine's job has got.
    .noun = { $count ->
        [one] job
       *[other] jobs
    }

d-mqizmue3 = State

d-ygdo3z6c = Done

d-msrdztlm = Total

d-fatgfp52 = Error

d-35ydyeno = Status

d-l7f77ylc-filter-done = Done

d-l7f77ylc-filter-total = Total

d-vtmqhpab = Case read
    .doc = A registry's moderation case as this app keeps it for a refresh.
    .noun = { $count ->
        [one] case read
       *[other] case reads
    }

d-wi7dxv7v = Case

d-f67cduxy = Status

d-5cmcfoao = Situation

d-u3qiqphi = Change
    .doc = A change one app asks the engine for: what it is, the exact version its source offered, and the reference a Flatpak installs.
    .noun = { $count ->
        [one] change
       *[other] changes
    }

d-sr6eixi6 = Identity

d-rkd5rikz = Kind

d-wfeji5lc = Version

d-gawnzinv = Root

d-xz347kth = Surface

d-wa7a4ebr = Publisher

d-jalnpzfr = Flatpak

d-uuhlsrfk = Reference

d-2usub7jq = File

d-smpzmxa2 = Size

d-u3qiqphi-filter-size = Size

d-dhqg2zaa = Ask
    .doc = A read of the sources, as the engine takes it.
    .noun = { $count ->
        [one] ask
       *[other] asks
    }

d-k33jf5d6 = Force

d-dhqg2zaa-filter-force = Force

d-bqnffizl = Started
    .doc = The engine's answer when it starts a job.
    .noun = { $count ->
        [one] started
       *[other] starteds
    }

d-omjipbxx = Job

d-haex7z3y = Error

d-g5vn7lms = One ask
    .doc = What one app is asked about: its id, and the registry its source is, when a bot is added from it.
    .noun = { $count ->
        [one] one ask
       *[other] one asks
    }

d-bzdopfzn = Identity

d-kz4m2q7x = Registry

d-64wyfi3e = One
    .noun = { $count ->
        [one] one
       *[other] ones
    }

d-ibr65ngi = Item

d-wmgpz4vn = One read
    .noun = { $count ->
        [one] one read
       *[other] one reads
    }

d-gwl2nm5a = Item

d-wmgpz4vn-filter-item = Item

d-ibsutfg5 = Remote reads
    .noun = { $count ->
        [one] remote read
       *[other] remote reads
    }

d-6476ldq6 = Remotes
    .doc = Each repository, as its own JSON text.

d-jr4k76wg = Flatpak

d-zt4zikpc = Trouble

d-ibsutfg5-filter-flatpak = Flatpak

d-oy3ekw2b = Remote ask
    .noun = { $count ->
        [one] remote ask
       *[other] remote asks
    }

d-7ltvsddp = Name

d-p4q2b4s7 = Url

d-vmyl5m7m = Enabled

d-oy3ekw2b-filter-enabled = Enabled

d-qebm5qpq = Block ask
    .noun = { $count ->
        [one] block ask
       *[other] block asks
    }

d-dlrecm5k = Identity

d-xjtxb4gh = Blocked

d-qebm5qpq-filter-blocked = Blocked

d-3su3ovbd = Package ask
    .noun = { $count ->
        [one] package ask
       *[other] package asks
    }

d-fxwybrap = File

d-qaju5cxd = Name

d-bkjyjpcr = Size

d-3su3ovbd-filter-size = Size

d-ta3ymo2c = Runtime ask
    .noun = { $count ->
        [one] runtime ask
       *[other] runtime asks
    }

d-vpcuziom = Runtime

d-snmv7oek = Registry

d-pjq42giv = Report ask
    .noun = { $count ->
        [one] report ask
       *[other] report asks
    }

d-3x7qwbnh = Identity

d-4ibtuh5s = Note

d-gea2szrq = Case

d-6qwyarvg = Registry

d-jjlocy5a = Appeal

d-pjq42giv-filter-appeal = Appeal

d-4mcprvp6 = Send ask
    .noun = { $count ->
        [one] send ask
       *[other] send asks
    }

d-mn7ndrul = Identity

d-rq53vgig = Operation

d-f72wtnsy = Note

d-3knw3o2v = Status ask
    .noun = { $count ->
        [one] status ask
       *[other] status asks
    }

d-5xifqzkr = Identity

d-pdu4bxlq = Case

d-dnrxsz6b = Registry

d-o23b3xvi = Changes
    .noun = { $count ->
        [one] change
       *[other] changes
    }

d-zxtxkidh = List

d-dj5auzr7 = Items
    .noun = { $count ->
        [one] item
       *[other] items
    }

d-r4t6n2uh = Items

d-k5i5m2oa = Evidence
    .noun = { $count ->
        [one] evidence
       *[other] evidence
    }

d-j6n5hmtr = Identity

d-bnqubb5u = Note

d-cbhojiz7 = Status

d-t7a6wmgq = Case

d-ra5kasg4 = Registry

d-dvwnjvtj = Operation

d-tdmfwxqo = Origin
    .doc = Where an app in the catalogue comes from.

d-c3vyt4cm = A Tilo registry
    .doc = A Tilo registry.

d-mlsekpul = Flathub
    .doc = Flathub.

d-ibi4yaz6 = A Flatpak repository this workspace or computer configured
    .doc = A Flatpak repository this workspace or computer configured.

d-5ufarnar = A package file someone handed this app
    .doc = A package file someone handed this app.

d-jzntjxm2 = Trust
    .doc = What a source's review of an app comes to.

d-wieqigty = Its publisher's signature and the registry's review
    .doc = Its publisher's signature and the registry's review.

d-bulyhjth = Flathub, which Tilo has not reviewed itself
    .doc = Flathub, which Tilo has not reviewed itself.

d-qloka2bk = A repository this workspace configured, which Tilo has not reviewed
    .doc = A repository this workspace configured, which Tilo has not reviewed.

d-3asz6773 = A package verified by an attestor this computer already trusts
    .doc = A package verified by an attestor this computer already trusts.

d-2klx4tra = Refresh
    .doc = Reads the workspace's sources again: the registries, Flathub and this computer's Flatpak repositories, and what the workspace runs.

d-hb6v2zdc = Poll
    .doc = Asks how far the read or the change running has got, and puts what it found into the workspace.

d-nurc7hol = Follow
    .doc = The job an app's change is, as far as the engine has got: its row says where it stands, and what it made is read once it ends.

d-nurc7hol-a = A

d-banfx2pr = Begin reading
    .doc = A job that changes the catalogue itself rather than one app, followed as the read is: its end reads the sources again, so a new repository or an update of everything lands.

d-banfx2pr-op = Op

d-banfx2pr-ask = Ask

d-emblroed = Begin
    .doc = A job the engine started for `a`, or why it did not start.

d-emblroed-a = A

d-emblroed-op = Op

d-emblroed-ask = Ask

d-s3xf57z2 = Read app
    .doc = Reads one app as it stands now: what the workspace runs, what the computer fetched, and the newest offer its source makes.

d-s3xf57z2-a = A

d-6v23i3ur = Show details
    .doc = Reads one app's page: what its source says about it.

d-6v23i3ur-a = A

d-kcpgrqgg = Take catalogue
    .doc = The catalogue one read found, put into the workspace: apps in the order the sources gave them, each replaced where it stands, and the ones its sources stopped listing taken out.

d-kcpgrqgg-status = Status

d-jb2fqpim = Absorb text
    .doc = One app as a source wrote it, read and kept where it stands: an app this workspace already knew keeps what the person looked at of it.

d-jb2fqpim-text = Text

d-fdx4aslc = Absorb remote text
    .doc = One repository as a source wrote it, kept where it stands.

d-fdx4aslc-text = Text

d-bcruk52u = Refresh remotes
    .doc = Reads this computer's Flatpak repositories again.

d-ayzvtjki = Add repository
    .doc = Adds a Flatpak repository to this workspace.
    .w-a-repository-has-a-name = A repository has a name
    .w-a-repository-name-holds-at-most = A repository name holds at most 128 letters
    .w-repositories-must-use-https = Repositories must use HTTPS
    .w-this-repository-is-already-here = This repository is already here

d-ayzvtjki-name = Name

d-ayzvtjki-url = Url

d-zebn7nsh = Enable source
    .doc = Enables one of this workspace's repositories.
    .w-this-repository-is-the-computer-s = This repository is the computer's. Only repositories in your workspace can be changed here.

d-zebn7nsh-r = R

d-aipro3dw = Disable source
    .doc = Disables one of this workspace's repositories.
    .w-this-repository-is-the-computer-s = This repository is the computer's. Only repositories in your workspace can be changed here.

d-aipro3dw-r = R

d-erliinv3 = Remote change

d-erliinv3-r = R

d-erliinv3-enabled = Enabled

d-3bhdbn3p = Remove repository
    .doc = Removes one of this workspace's repositories, and its apps from the catalogue.
    .w-this-repository-is-the-computer-s = This repository is the computer's. Only repositories in your workspace can be changed here.

d-3bhdbn3p-r = R

d-5gcbykzz = Browse
    .doc = Browses one repository's apps, through the system Flatpak metadata on this computer.

d-5gcbykzz-r = R

d-6zlctlbs = Get
    .doc = Gets an app the workspace does not run yet: the newest version its source offers.

d-hpw5iub6 = Update
    .doc = Updates an app the workspace runs to the newest version its source offers.

d-fwuiofga = Install the signed runtime
    .doc = Installs the runtime an app needs, once its page reviewed the exact signed offer.
    .w-this-app-names-no-runtime = This app names no runtime

d-qitieq3j = Uninstall
    .doc = Removes an app from this workspace.
    .w-this-app-belongs-to-the-computer = This app belongs to the computer. Ask its administrator to remove it.

d-ykz7qcg5 = Keep this version
    .doc = Keeps this workspace's version of an app, so it is never cleaned up when unused.
    .w-this-app-has-no-version-to = This app has no version to keep

d-pad5jjdg = Allow updates
    .doc = Lets this workspace's unused version of an app be cleaned up again.

d-4z4tkll4 = Stop
    .doc = Stops the change running for an app, keeping what it did until then.

d-s2mhdfg7 = Open
    .doc = Opens an app this computer has, fetching it first when the workspace runs it and this computer has not.

d-lm6i5ieu = Update all apps
    .doc = Updates every app the workspace runs and its sources offer a newer version of.
    .w-every-app-is-up-to-date = Every app is up to date

d-vmb6w54f = Add to this workspace…
    .doc = Adds a directory bot to this workspace as the person's own shortcut: its program is verified against the signed listing and saved as the shortcut the workspace's host runs for them, under the triggers the program declares.
    .w-only-a-directory-bot-is-added = Only a directory bot is added to a workspace

d-vmb6w54f-a = A

d-f32yh5vc = Block app
    .doc = Blocks an app: it then appears nowhere in the Store or in search, and cannot be opened or installed from them.

d-f32yh5vc-a = A

d-kpgqwe2u = Unblock
    .doc = Unblocks an app this person blocked.
    .w-this-app-is-not-blocked = This app is not blocked

d-kpgqwe2u-a = A

d-dxyl32te = Import package
    .doc = Reviews a package file someone hands the Store.

d-dxyl32te-file = File

d-7cabzl2m = Install reviewed package
    .doc = Installs the package someone just reviewed, after the person read its details.
    .w-review-this-exact-package-before-installing = Review this exact package before installing it

d-viajklb6 = Open reference
    .doc = Opens a `.flatpakref`: it joins the catalogue as a card, and installing it stays a separate step the person takes.

d-viajklb6-file = File

d-3fvgbh36 = Review an app's runtime
    .doc = Reads a runtime's exact signed offer, so its page says what installing it would run.

d-3fvgbh36-runtime = Runtime

d-3fvgbh36-registry = Registry

d-nbj74m2v = Create report draft

d-nbj74m2v-a = A

d-sbradm4n = Record moderation case
    .doc = Records the registry's case for an app, as a refresh read it.
    .w-a-case-has-an-id-and = A case has an id and a status

d-sbradm4n-identity = Identity

d-sbradm4n-case = Case

d-sbradm4n-status = Status

d-4zlwbfzy = Prepare report
    .doc = Prepares the exact signed operation a report or an appeal sends, without sending it.

d-4zlwbfzy-r = R

d-4zlwbfzy-appeal = Appeal

d-poycyw6m = Submit report
    .doc = Signs a report with the workspace's identity and sends it to the app's repository.

d-c3wpc2me = Appeal decision
    .doc = Appeals a decision about an app.
    .w-only-a-decision-can-be-appealed = Only a decision can be appealed

d-pprcmnam = Refresh status
    .doc = Reads the registry's verified status for the report's case again.
    .w-this-report-has-no-case-yet = This report has no case yet

d-pprcmnam-r = R

d-mzdd436j = Read sources
    .doc = The catalogue: the apps the sources offer, by the place a person chose, with the app's page beside the list.

d-deg65ibs = Read details
    .doc = Opening an app's page reads what its source says about it, once.

d-wrb37n75 = Apply action
    .doc = The action a page's one button runs, by the name the catalogue decided.

d-wrb37n75-verb = Verb

d-omucqgg7 = Report file
    .doc = A report's evidence as the file a person saves and sends with it.

d-h6isdwur = Add remote~present

d-3zppaqhs = Enable remote~present

d-ap2xqqbg = Disable remote~present

d-d4bnh64q = Remove remote~present

d-qa3erebl = Get~present

d-frmgpell = Update~present

d-j4iwpnpf = Install runtime~present

d-l6ibqb6r = Remove~present

d-zbdh5lax = Pin~present

d-a6h37upx = Update all~present

d-mbmo3meu = Add bot~present

d-s6refga2 = Unblock app~present

d-ou24sxcw = Install reviewed~present

d-i4b3rb4w = Record moderation case~present

d-cm5whz4i = Appeal~present

d-bixeamzz = Refresh status~present

d-xo5mha7p = Catalogue
    .w-search = Search
    .w-all-apps = All apps
    .w-app-store = App Store
    .w-refresh-apps = Refresh apps
    .w-update-all-apps = Update all apps
    .w-review-package = Review package
    .w-flatpak-repositories = Flatpak repositories
    .w-trust-and-moderation = Trust and moderation
    .w-back-to-apps = Back to apps
    .w-package = package
    .w-categories = Categories
    .w-reading-the-sources = Reading the sources
    .w-could-not-read-the-sources = Could not read the sources
    .w-no-apps-found = No apps found
    .w-try-a-different-search-or-category = Try a different search or category.

d-nncj3afc = Discover
    .w-no-apps-available = No apps available
    .w-add-or-enable-a-repository-you = Add or enable a repository you trust.
    .w-featured = Featured
    .w-recently-updated = Recently updated
    .w-all-apps = All apps

d-alb5chrj = App card

d-ck3o6mji = App list
    .w-show-more = Show more

d-pmrqekaa = App row
    .w-working = Working…

d-vakqk5y4 = App page
    .w-allow-updates = Allow updates
    .w-keep-this-version = Keep this version
    .w-uninstall = Uninstall
    .w-stop = Stop
    .w-unblock = Unblock
    .w-block-app = Block app
    .w-technical-details = Technical details
    .w-cannot-change-this-app = Cannot change this app
    .w-working = Working…
    .w-package-verified = Package verified
    .w-review-its-details-before-installing-it = Review its details before installing it.
    .w-install-reviewed-package = Install reviewed package
    .w-latest-version = Latest: version { $available }
    .w-waiting-for-an-update-from-the = Waiting for an update from the developer
    .w-only-built-for = Only built for { $processors }
    .w-about-this-app = About this app
    .w-screenshots = Screenshots
    .w-screenshot-of = Screenshot of { $name }
    .w-what-s-new = What's new
    .w-version = Version { $version }
    .w-trust = Trust

d-gxuht36a = Permissions section
    .w-what-it-can-do = What it can do
    .w-runs-in-tilo-s-sandbox-and = Runs in Tilo's sandbox and asks before it uses your files, camera or microphone.
    .w-its-source-has-not-said-what = Its source has not said what this app can do.

d-wizqobn5 = Facts section
    .w-facts = Facts
    .w-source = Source
    .w-version = Version
    .w-developer = Developer
    .w-licence = Licence
    .w-updated = Updated
    .w-categories = Categories
    .w-app-id = App ID
    .w-publisher-key = Publisher key
    .w-package-digest = Package digest
    .w-required-runtime = Required runtime
    .w-open-website = Open website
    .w-open-source-code = Open source code

d-6hlkpuof = Bot section
    .w-what-it-does = What it does
    .w-what-it-can-reach = What it can reach
    .w-nothing-beyond-the-workspace-it-runs = Nothing beyond the workspace it runs in.
    .w-uses-s-actions-and-watches-its = Uses { $other }'s actions and watches its changes
    .w-connector-version-or-newer = Connector, version { $connector } or newer
    .w-may-post-under-other-names-and = May post under other names and avatars. Always tagged as a bot.

d-amq6xvh5 = Report section
    .w-report-a-problem = Report a problem
    .w-reports-are-signed-with-your-workspace = Reports are signed with your workspace identity and stay drafts your workspace shares until you submit them.
    .w-create-report-draft = Create report draft

d-kcllfezw = Report form
    .w-case = Case { $case }
    .w-describe-what-happened = Describe what happened
    .w-submit-report = Submit report
    .w-refresh-status = Refresh status
    .w-appeal-decision = Appeal decision
    .w-export-evidence = Export evidence

d-3trohjzy = Report page
    .w-report-for = Report for { $identity }
    .w-show-the-app = Show the app

d-jyxy3v7p = Remotes page
    .w-browse-software-from-your-configured-repositories = Browse software from your configured repositories. Repositories supplied by this computer stay available; repositories you add belong to your workspace.
    .w-refresh-repositories = Refresh repositories
    .w-flatpak-unavailable = Flatpak unavailable
    .w-configured-repositories = Configured repositories
    .w-no-repositories-configured = No repositories configured
    .w-add-a-repository = Add a repository
    .w-name = Name
    .w-repository-name = Repository name
    .w-repository-url = Repository URL
    .w-repositories-must-use-https = Repositories must use HTTPS.
    .w-https = https://…
    .w-add-repository = Add repository

d-5pnjajg7 = Remote row
    .w-this-computer = This computer
    .w-this-workspace = This workspace
    .w-disable-source = Disable source
    .w-enable-source = Enable source
    .w-remove-repository = Remove repository
    .w-browse-apps = Browse apps

d-6u3w4u44 = Remote page
    .w-could-not-browse-repository = Could not browse repository
    .w-no-apps-loaded-from-this-repository = No apps loaded from this repository
    .w-browse-apps-to-read-what-it = Browse apps to read what it offers.
    .w-from = { $parts_words } from { $title }
    .w-disable-source = Disable source
    .w-enable-source = Enable source
    .w-browse-apps = Browse apps

d-bzwxksey = Trust page
    .w-how-reports-work = How reports work
    .w-reports-are-signed-with-your-workspace = Reports are signed with your workspace identity and stay drafts your workspace shares until you submit them. A new status shows only once it is verified, so an offline or refused request stays a draft you can send again.
    .w-what-a-signature-means = What a signature means
    .w-a-verified-package-comes-from-its = A verified package comes from its publisher and has not been changed. That does not make an app useful, private or honest, and the Store never invents a review badge.
    .w-publishing-guidelines = Publishing guidelines
    .w-listings-must-describe-what-the-app = Listings must describe what the app does, name any paid or online services it depends on, ask only for the permissions it needs, keep people's data across updates, work with assistive technology, and never impersonate others or track people without saying so.
    .w-reports = Reports
    .w-no-report-drafts = No report drafts
    .w-open-an-app-and-choose-create = Open an app and choose Create report draft.
    .w-sources = Sources
    .w-did-not-respond-other-apps-remain = { $name } did not respond. Other apps remain available.
    .w-the-app-catalogue-from-has-expired = The app catalogue from { $name } has expired. Apps you have keep working.

d-wtyvo4dc = Report row
    .w-report-for = Report for { $identity }
    .w-no-concern-described-yet = No concern described yet
