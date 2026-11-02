# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-4jk2lkpt = Desk
    .doc = This device's desk as the session holds it: the time in the person's style, who they are, their notifications, the workspaces on this device, the tasks running for them and the surface the desk shows over its windows.
    .noun = { $count ->
        [one] desk
       *[other] desks
    }

d-kx4rprro = Person

d-6unuw4qc = Here
    .doc = The surface the person is in: `none` at rest, else one of `surface`'s names.

d-wey3uqyo = Switcher revealed
    .doc = Whether the switcher's panel shows: a quick Alt+Tab switches windows and draws nothing.

d-5mipdtxu = Switcher scope
    .doc = Which windows the switcher's walk crosses: `workspace` for every window, `app` for the windows of the app in front (Alt+`).

d-foayl4ai = Spotlight
    .doc = What the person typed in the shell's search, which opens the launcher where they left off.

d-wco4hiud = Banner
    .doc = The notice shown as a banner over the desk now, by its id.

d-7bs5q4ht = Clock

d-rlrql4ir = Timer
    .doc = The timer or stopwatch Clocks runs, by its name: its label, or `Timer` or `Stopwatch` when the app named none; empty when none floats.

d-m4heyzat = Timer left
    .doc = What the timer reads: what it has left, or what the stopwatch has run.

d-pql2t5r3 = Timer paused

d-qb4cqhjk = Timer stopwatch

d-zxfieu2y = Ask
    .doc = The question the session puts to the person about themselves; empty when none waits.

d-w2fyvufg = Ask detail
    .doc = What answering the question brings.

d-zqu6ku2o = Ask answer
    .doc = The question's answer, as its button says it.

d-eewsi53q = Ask open
    .doc = Whether the question stands open on the person's avatar.

d-gi7d6wct = Weekday

d-5cs3vm3f = Date

d-ydlbntd5 = Unread

d-hj6eqjwk = Quiet

d-lc33lyrs = Status panel

d-c5k5ua2t = Workspace

d-xpcwi5mg = Strip

d-vbyis266 = Workspaces

d-mewztugi = Tasks

d-xu2jijql = People

d-g4t5njqq = Bar reached
    .doc = The pointer at the top edge, and a window standing where the bar rides: what a bar that shows only sometimes follows.

d-mvfeip6u = Bar covered

d-pjnfirs3 = Dock reached
    .doc = The pointer at the bottom edge, and a window standing where the dock rides.

d-prsypg4e = Dock covered

d-twcpnt5t = Agents

d-sh62fec4 = Rooms

d-3vvnnztw = Notices

d-lz6qs7zm = Notice actions

d-5kokd6gv = Windows

d-pdaqdidq = Tray
    .doc = The running applications' tray items, in the order they registered.

d-pcq2hsmd = Runs
    .doc = The runs of what the person asked the session's agents, oldest first; each is a conversation (`conversation.tl`).

d-kyflwqbc = Run entries

d-l3sgzgcx = Entry refs

d-ihivrfzx = Run
    .doc = A run of what the person asked one of the session's agents: the conversation they have in it.
    .noun = { $count ->
        [one] run
       *[other] runs
    }

d-76zxb2mg = Key

d-gi3gmep2 = Title

d-7w52457h = Agent

d-rgfup3n2 = Phase

d-mlpdykxt = Active

d-svcsmns6 = Live
    .doc = What the agent says now, as the model streams it; empty between turns.

d-y2ocksvq = Run entry
    .doc = One entry of a run's conversation: what the person or the agent said, or a program either of them ran.
    .noun = { $count ->
        [one] run entry
       *[other] run entries
    }

d-pt4ssx46 = Key

d-kzqpygsb = Run

d-dfzwdenr = Seq

d-pwj3dxxn = Author

d-lpv4nya5 = By agent

d-gln4bzfr = Step

d-b52hiazz = Text
    .doc = The words, or the program as it ran.

d-tsjqixo4 = Outcome

d-euv7qons = Answer

d-oqhj7aau = Entry ref
    .doc = A resource a program in a conversation printed (a reference) or changed (a row to undo).
    .noun = { $count ->
        [one] entry ref
       *[other] entry refs
    }

d-twcxt5ku = Key

d-fqi7b3k2 = Entry

d-sjbkqhyd = Changed

d-kuignxum = Resource

d-32yhhdkm = App id

d-6kelnhk7 = Type kind

d-sobb47hr = Label

d-z66pleua = Change

d-bynckgvt = Workspace
    .doc = A workspace on this device.
    .noun = { $count ->
        [one] workspace
       *[other] workspaces
    }

d-pmjwp2sk = Key

d-nze6cia2 = Name

d-ce523tqy = Current

d-tlbg2fms = Tray item
    .doc = A running application's tray item: what the bar shows, and what a press on it reaches.
    .noun = { $count ->
        [one] tray item
       *[other] tray items
    }

d-o3wuotce = Key

d-e5qbz5cz = Title

d-lsd5bmss = Menu

d-eeoetc53 = Task
    .doc = A task running for the person: an agent's or a script's.
    .noun = { $count ->
        [one] task
       *[other] tasks
    }

d-qvmai6xx = Key

d-h3h2rsdg = Title

d-b3ijx2a6 = State

d-bqittbss = Person
    .doc = Someone else here in the workspace in front.
    .noun = { $count ->
        [one] person
       *[other] persons
    }

d-vfhh3oax = Key

d-lmoug2df = Name

d-r7dcmwzl = Win
    .doc = A window of the desk: a tab of a strip in the workspace in front.
    .noun = { $count ->
        [one] win
       *[other] wins
    }

d-dfgqyodj = Key

d-by5gisol = Title

d-v5xdn5a6 = Origin app

d-zscvpgbm = App key
    .doc = The app it shows, shared by two windows of one app: the switcher's app scope crosses the windows holding it.

d-drnqlrp5 = Icon

d-mjnpzf6r = Strip

d-g3ddlvlc = Strip key
    .doc = Its strip's id, as moving a window there names the strip.

d-hnoh7dgq = Workspace

d-z3pjamxl = Recency

d-a5fwclo5 = Focused

d-tb2jierp = Preview

d-labby3mp = Agent
    .doc = An agent here for the person, or that acted for them.
    .noun = { $count ->
        [one] agent
       *[other] agents
    }

d-njbk3cds = Key

d-4nhbpuss = Name

d-6zpjzgvl = State

d-zcdb3jtl = Doing

d-ty5fvze2 = Mark

d-gbehvj6x = Unseen

d-jlqw5py5 = Room
    .doc = A workspace the person is in, as the workspace overview names it.
    .noun = { $count ->
        [one] room
       *[other] rooms
    }

d-wvbsg6yl = Key

d-wofgn4vi = Name

d-5p6whtgp = Current

d-huv4cci3 = Members

d-l4d25hj6 = Present

d-no5yi6m7 = Activity

d-zk7pzrt2 = Notice
    .doc = A notification in the centre, as its sender or its task named it.
    .noun = { $count ->
        [one] notice
       *[other] notices
    }

d-jq4onnp4 = Key

d-7snyky6u = Title

d-wauytevp = Body

d-nxunr2dc = Origin app

d-xofho4e2 = Icon

d-bsp6shbm = Kind

d-z67wqj3i = Read

d-uvxcllkj = Group

d-b4i657ss = Wants reply

d-7rjfrgeo = Notice action
    .doc = One action a notification offers, as its sender named it.
    .noun = { $count ->
        [one] notice action
       *[other] notice actions
    }

d-ohcc5fba = Notice

d-mfyylqsl = Key

d-6ehy2qwa = Label

d-qhf2pyze = Arrangement
    .doc = The person's own arrangement: what is open where (docs/designs/app-language/shell.md §2, workspace-model.md §9).
    .noun = { $count ->
        [one] arrangement
       *[other] arrangements
    }

d-rknsknpw = Workspaces

d-2aititgj = Rows

d-x2vx3spj = Strips

d-id3ec54i = Places
    .doc = Every place, each naming the strip it is in, so the views need not walk the tree.

d-wydxt4zu = Arrangement workspace
    .doc = A workspace the person's desk shows.
    .noun = { $count ->
        [one] arrangement workspace
       *[other] arrangement workspaces
    }

d-6m3fzjng = Key

d-tljqo2yo = Name

d-k6bv5cxt = Arrangement row
    .doc = A row of the desk: a strip across a workspace's width, holding columns.
    .noun = { $count ->
        [one] arrangement row
       *[other] arrangement rows
    }

d-idcxp7jb = Key

d-qassqvic = Workspace

d-yppyohd5 = Name

d-awgwtm6j = Shell
    .doc = The shell that made it; empty reads as this shell's.

d-vn23musu = Arrangement strip
    .doc = A strip: a column of places, with a width, in a row of a workspace.
    .noun = { $count ->
        [one] arrangement strip
       *[other] arrangement strips
    }

d-heuzka3o = Key

d-dqpmay7u = Workspace

d-r22fgcaa = Row

d-pbep7iq6 = Width
    .doc = Its width in per-mille of its row's usable width (333 narrow, 667 normal, 1000 wide).

d-gpviayl3 = Maximized

d-opotzabz = Active
    .doc = The place in front, by its id; empty for none.

d-54qldnir = Placed
    .doc = Whether it has a place on its workspace's canvas.

d-mnpd23ty = Canvas x

d-yzbtjovv = Canvas y

d-m67g5cso = Canvas width

d-4n7jst3f = Canvas height

d-n2pmgnow = Shell

d-gpmk4o3r = Arrangement place
    .doc = A place: one window, showing a resource through a view.
    .noun = { $count ->
        [one] arrangement place
       *[other] arrangement places
    }

d-ez2plmfg = Key

d-rsnsojnc = Strip
    .doc = The strip it is in.

d-uxd54qen = Shows
    .doc = The app showing the resource, by its canonical id (`so.tilo.terminal`).

d-jecpdz4d = Shown as
    .doc = The view it shows the resource through; empty for the app's default.

d-yesjoqbj = Record
    .doc = The record it shows, when it shows one.

d-iu3wwk2u = Title
    .doc = The title it is shown under.

d-aonjenrr = Pinned
    .doc = Whether the person pinned it: an agent's arrangement leaves it alone (§5).

d-rqrtd6vq = Shell
    .doc = The shell that made it; empty reads as this shell's.

d-nkjtmr33 = Viewing
    .doc = What the person shows of their arrangement, and which strip stands in front: the shell's own view of it, in their own record (`records: .own`, `root viewing: Viewing`).
    .noun = { $count ->
        [one] viewing
       *[other] viewings
    }

d-nl27ikjq = Showing
    .doc = Which preset the person shows.

d-6b2coqll = Focused
    .doc = The strip in front, by its key; empty for none.

d-cejnvssb = Shell
    .doc = The person's own shell record, reached as `shell` (`root shell: Shell`).
    .noun = { $count ->
        [one] shell
       *[other] shells
    }

d-qsrjrjkd = Preset
    .doc = The preset the settings were last made from; `tuned` says whether the person has changed a setting since.

d-2se66uzq = Based on
    .doc = The preset the settings were last made from, kept while the person tunes one thing, so the picker says "Custom, based on …" rather than losing where they started.

d-nt6y5bk3 = Tuned
    .doc = Whether the person has changed a setting since choosing a preset.

d-kqnswwzm = Bar shown
    .doc = The top bar.

d-mi3yni2m = Bar edge

d-la4yrobd = Bar fit

d-6ujrnpej = Bar finish

d-b2fcmpwg = Bar showing

d-773tn644 = Bar size

d-voqg5seo = Bar parts

d-b7grsgnv = Dock shown
    .doc = The dock.

d-o6tdqxq3 = Dock edge

d-dq6kzv2v = Dock fit

d-y5rlj3v3 = Dock finish

d-nz74n3iu = Dock showing

d-wx25jsef = Dock size

d-hx5alotd = Dock items

d-ffa2ohzm = Dock running
    .doc = The dock shows the apps running even when nobody pinned them.

d-h5oo7js2 = Dock launcher
    .doc = The dock has a launcher entry.

d-5n4s55tz = Dock zoom
    .doc = A click on the dock's icons magnifies the item under the pointer, as macOS's Dock does.

d-sfhd6wko = Bar part
    .doc = One part of the top bar, in the person's order.
    .noun = { $count ->
        [one] bar part
       *[other] bar parts
    }

d-fjzbw5un = Slot
    .doc = What the part is; the bar draws it from this, so a part moved is a part the same.

d-v3iwpjov = Title
    .doc = Its name on the bar, and in the settings.

d-pbly7ymy = Shown
    .doc = Whether it shows on the bar; a part off keeps its place in the record.

d-bnfk7mx5 = Dock item
    .doc = One thing on the dock: a pinned app or resource, the launcher, or an app running now.
    .noun = { $count ->
        [one] dock item
       *[other] dock items
    }

d-rfsefuzv = Key
    .doc = What the item stands for: an app id, or a resource's reference as text.

d-qsnrl2gk = Title
    .doc = Its name on the dock, and what assistive technology reads.

d-6gyb7csc = Icon
    .doc = The glyph it shows, by the platform icon's kebab-case name.

d-k376lylv = Pinned
    .doc = Whether the person pinned it; a running app that is not pinned is there while it runs.

d-5vpgvzqb = Running
    .doc = Whether it runs now.

d-mgcmvxjx = Badge
    .doc = How many things it has new, as its badge shows.

d-e7ft4mly = Attention
    .doc = Whether it wants the person: a pulsing badge, and an attention mark.

d-clhqzrkb = Width
    .doc = How wide a strip stands within its row, as the record keeps it in per-mille (§2's strip widths: the presets and free widths).

d-hrjwxtwf = Three tenths of the row: a list, a set of options
    .doc = Three tenths of the row: a list, a set of options.

d-ipingyqi = Two thirds of the row: a note, a conversation
    .doc = Two thirds of the row: a note, a conversation.

d-nfufnx7j = The row's whole usable width
    .doc = The row's whole usable width.

d-vvznzcd4 = Preset
    .doc = A way of laying out the same resources and actions, which the person picks: a preset of shell.md §6, whose finer settings live underneath (§8).

d-25lm42aq = Columns of places, scrolled horizontally, with strips stacked and scrolled vertically
    .doc = Columns of places, scrolled horizontally, with strips stacked and scrolled vertically.

d-3ha7yyn2 = Places at points on a pannable, zoomable surface, each drawn in a frame
    .doc = Places at points on a pannable, zoomable surface, each drawn in a frame.

d-whadpf2h = Places at points, each drawn as its own view: resources standing as content on one
    .doc = Places at points, each drawn as its own view: resources standing as content on one surface, grouped by where they are rather than by a frame around them.

d-6uzzl3rd = The traditional desktop: frames on a floating desk, the focused window's own commands in the
    .doc = The traditional desktop: frames on a floating desk, the focused window's own commands in the top bar, and a floating dock at the foot, the shape macOS and GNOME people know.

d-nxjnwtsd = Edge
    .doc = The person's shell: which parts the top bar and the dock have, in what order, on which edge, how they look and when they show.

d-rbzbofjd = Top

d-e4zlwr65 = Bottom

d-tzvst7nv = Left

d-qykb6xix = Right

d-frrucget = Fit
    .doc = How a bar or dock sits in its edge: inset with rounded corners, floating over what it covers, or edge to edge and square.

d-kheriztb = Floating

d-ztqim6ly = Full

d-luthoy6b = Finish
    .doc = What a bar or dock is made of: the theme's translucent material over what is behind it, or its opaque bar ground.

d-ipplqs3o = Translucent

d-bc2qov6r = Solid

d-rfgelicn = Showing
    .doc = When a bar or dock shows.

d-p4owlmpg = Always

d-sscc6gs3 = Shown while no window overlaps it; it hides when a window would cover it
    .doc = Shown while no window overlaps it; it hides when a window would cover it.

d-v2eyzsgr = Hidden until the pointer bumps the screen edge it rides
    .doc = Hidden until the pointer bumps the screen edge it rides.

d-62fazfo4 = Hidden after the pointer has not been over it for a moment, shown when it returns
    .doc = Hidden after the pointer has not been over it for a moment, shown when it returns.

d-v3z2zasp = Size
    .doc = How large the parts are.

d-2yxm7xfn = Small

d-uv33v3c2 = Regular

d-5czgy2f6 = Large

d-vtiohdqj = Slot
    .doc = One part a person can put on the top bar, in their order.

d-2k6zzh2i = The system's menu: Tilo, and the device's own commands
    .doc = The system's menu: Tilo, and the device's own commands.

d-ujo3oxvs = The workspace in front, opening the workspaces to switch between
    .doc = The workspace in front, opening the workspaces to switch between.

d-e43glabw = The focused app's menus: what that app can do now, on its resource
    .doc = The focused app's menus: what that app can do now, on its resource.

d-bvj3ji7x = The strip in front
    .doc = The strip in front.

d-tqo2gugp = The tasks running for the person
    .doc = The tasks running for the person.

d-j2kj3zvb = The time
    .doc = The time.

d-ntj3ee4g = Who else is here, the person, and the roster of everyone
    .doc = Who else is here, the person, and the roster of everyone.

d-bd6dyvh6 = The device's status (the sound, the connection and the battery) and the notifications
    .doc = The device's status (the sound, the connection and the battery) and the notifications.

d-z4jepckl = The battery on its own, retired: the status part shows it now
    .doc = The battery on its own, retired: the status part shows it now.

d-ezboaery = The agent's own indicator: a task working for the person
    .doc = The agent's own indicator: a task working for the person.

d-fqaqauwe = A flexible gap: the bar's parts before it lead, those after it trail
    .doc = A flexible gap: the bar's parts before it lead, those after it trail.

d-oomdh5mi = Layout
    .doc = A named set of the settings, which the person applies in one action: a chrome preset.

d-gdzkj7tc = The dock on the bottom with the bar on the top: Tilo's own
    .doc = The dock on the bottom with the bar on the top: Tilo's own.

d-b6bz7ze6 = A floating, rounded, translucent dock at the bottom that hides while a window covers
    .doc = A floating, rounded, translucent dock at the bottom that hides while a window covers it, and a top bar with the focused app's menus after the system's.

d-k4de63ud = A full-width taskbar at the bottom, always shown
    .doc = A full-width taskbar at the bottom, always shown.

d-cqbe6g3k = The top bar alone; the overview on demand
    .doc = The top bar alone; the overview on demand.

d-hyr6xrfo = No chrome at rest; the edges reveal what the person needs
    .doc = No chrome at rest; the edges reveal what the person needs.

d-poagvor7 = The traditional desktop: frames on a floating desk, the focused window's own commands in the
    .doc = The traditional desktop: frames on a floating desk, the focused window's own commands in the top bar, and a floating dock at the foot, the shape macOS and GNOME people know.

d-q7rxwd72 = Bar room
    .doc = How much room the top bar has for the focused window's commands.

d-w3dorpbd = The frequent commands as controls, every run as a menu
    .doc = The frequent commands as controls, every run as a menu.

d-qqg4smi5 = Every run as a menu; the frequent commands within them
    .doc = Every run as a menu; the frequent commands within them.

d-7caakb5b = One menu, named by the window, with every run in it
    .doc = One menu, named by the window, with every run in it.

d-jpu3iwft = Show as
    .doc = Shows the desk through `shown` (§6): the scrolling strips, the framed canvas or the spaces canvas.

d-dkylvhw4 = In front
    .doc = Makes the strip `strip` the one in front: what a press on a column's header does.

d-zoqbhs7o = Seed
    .doc = A shell that has never been arranged: the bar's parts and the dock's, in the order the default shell shows them.

d-vglzmkiy = Drop the power part
    .doc = A shell that kept the retired `power` part beside the status loses it.

d-eanbdi6l = Use this layout
    .doc = Uses the preset `to`: every setting it names at once.

d-rpbpwrcy = Show the app's menus
    .doc = Puts the focused app's menus on the bar, after the system's: what the app in front can do now, on its resource.

d-7jggsdfm = Hide the app's menus
    .doc = Takes the focused app's menus off the bar.

d-2k74oe7b = Back to the preset
    .doc = Returns the shell to the preset it was based on, dropping the person's tuning.

d-jqwkinjd = Bar on
    .doc = Puts the bar on `edge`.

d-u6k2x6mp = Set bar fit

d-i2mddhin = Set bar finish

d-w4d7htty = Set bar showing

d-fdnwo2gt = Set bar size

d-fcxzqnum = Show the top bar
    .doc = Shows or hides the top bar.

d-5l5rmk47 = Dock on
    .doc = Moves the dock to `edge`, keeping how it is built.

d-jllttdkc = Set dock fit

d-uw5xrdst = Set dock finish

d-iyws3u4w = Set dock showing

d-quwdhi4n = Set dock size

d-23faedwo = Show the dock
    .doc = Shows or hides the dock.

d-exyxrzml = Running apps
    .doc = Whether the dock shows the apps running that nobody pinned.

d-vlnaqn4u = Launcher entry
    .doc = Whether the dock has a launcher entry.

d-z2w4wlev = Magnify the dock
    .doc = Whether the dock magnifies the item under the pointer.

d-vurzkuat = Move
    .doc = Puts a bar part with the one it comes before, wherever the part stands now: the one action a drag commits, and the same change from the settings, a shortcut or an agent.

d-4ljvsals = Move
    .doc = Puts a bar part at the place `at`, counted from the leading edge: what a drag that names a place gives.

d-5esgamkb = Shown on the bar
    .doc = Shows or hides one part of the bar.

d-hphul6cv = Move
    .doc = Puts a dock item with the one it comes before: the one action a drag commits.

d-tv7bsor6 = Move
    .doc = Puts a dock item at the place `at`, counted from the leading edge: what a drag that names a place gives.

d-utk4ewgf = Pin to the dock
    .doc = Pins a resource or an app to the dock, before `next` (or at the end when no neighbour is named): what dragging one onto the dock does, and what the person does from a menu.

d-5yz5iwzs = Remove from the dock
    .doc = Takes an item off the dock.

d-3edhzcjk = Seed run
    .doc = The run a conversation starts from: what Ada asked, the agent's words between its steps, a program it ran that printed two notes, one that changed a note, one of Ada's own that did not compile, and the agent's answer.

d-j4ax4otm = Seed notices
    .doc = The desk's notices a scenario starts from: what apps and an agent said today.

d-zm35wbjg = A workspace
    .doc = Adds a workspace, with one row, to `a`, and returns the row's key.

d-ztflhbbf = A strip
    .doc = Adds a strip named `name` to `row`, standing at `width`, and returns its key.

d-chvuqgur = A note place
    .doc = Adds a place showing the note at `index` of the notebook to `strip`, and returns its key.

d-tnecawb6 = A foreign place
    .doc = A place of a kind this build has no record for: it stands as it is, which is what a place another shell made looks like (§2).

d-i6ll2n4j = Desk view
    .doc = The person's desk as a window or a display shows it: the top bar on its edge, the switch between the presets under it and the preset they picked under that.

d-72tgr4px = Clock
    .doc = The time, to the minute, and the timer Clocks runs beside it: its label and what it has left (a stopwatch: what it has run), paused when the person paused it.

d-3ezewqgy = Bar
    .doc = The top bar: the parts the person put on it, in their order, each a view of its own, so a change runs only the part that reads it.

d-pvl6wwx6 = Bar part
    .doc = One part of the bar, drawn from what it is.

d-vidcax4k = Workspace menu
    .doc = The workspace in front, opening the workspaces to switch between and the actions on this device's workspaces.

d-k7lksxkj = Status icons
    .doc = The device's status as the session publishes it: the sound's level, the connection the machine's traffic leaves on, and the pack's charge.

d-hac3p23j = Volume status
    .doc = The sound's level, and that it is muted; nothing on a device whose sound reads no level.

d-iwv7cd5i = Network status
    .doc = The connection the machine's traffic leaves on: Wi-Fi under its network's name, a cable, or offline; nothing on a device that publishes no connection at all.

d-agni63m4 = Battery status
    .doc = The pack's charge as a percentage beside the battery, its glyph the charging battery while the pack takes charge (or waits for it) and the plain battery otherwise — the power service's own state, not whether the machine happens to run on the pack; nothing on a machine with no battery.

d-moh3hzne = Bell
    .doc = The notifications: an icon button named with what it shows ("Notifications", how many are unread, or that they are silenced), with the count on the bell's corner.

d-fniigjed = Viewing settings
    .doc = The shell's own settings: the preset the desk shows, and where to pick another (§6, §8).

d-j2fnnt4g = Show preset~present

d-m4owmri7 = Framed canvas
    .doc = The canvases of shell.md §2 and §8: places at points on a surface that pans and zooms, each drawn in a frame (`frames`), or as its own view standing as content and grouped by where it is into a space per task (`spaces`).

d-nt2inbrl = Spaces canvas
    .doc = The frameless canvas: every place at its point, drawn as its own view with no frame around it, grouped into the strip's space by nearness.

d-t3cux3yd = Framed place
    .doc = A place on the framed canvas: the shell's frame around it (the window frame a desktop's windows stand in, its controls at the end of the bar), and its resource's view composed inside, filling it, or the well its window shows in (shell.md §8).

d-s6b5stgz = Space content
    .doc = A place on the frameless canvas: its resource's view standing as content (or its window's well), with a line naming what it is rather than a frame around it.

d-w6ftt2gh = Canvas entry
    .doc = What the person types on the canvas: a search that becomes a place.

d-ewgdlyyr = Show app menus~present

d-npxnqobp = Reset~present

d-vedyud4d = Move part~present

d-gkca5uj2 = Move item~present

d-i4odzov3 = Pin~present

d-bzxwxsra = Composer
    .doc = The agent composer: ask an agent from anywhere, about what the person points at or has selected, and see the tasks running and what they did.

d-6hsxbazg = Task line
    .doc = One running task: what it is on, its state, and the way into its log.

d-dz2lhlbc = Conversation
    .doc = The conversation: the agent chat and the command line in one view, over a run of the session's agent.

d-edxsethn = Said
    .doc = Words in the conversation: the person's to the agent, or the agent's between its steps, under who said them.

d-zhqh5ven = Said now
    .doc = What the agent is saying now, as the model streams it, until its turn ends.

d-ojxyofwa = Program step
    .doc = One program run in the conversation, by the person or the agent: the program as code, how it ended when it did not run to its end, what it answered when that is more than what it names, the resources it changed and the ones it printed.

d-ghaelk2z = Reference
    .doc = A resource a step printed, as a reference: a row naming it and its type.

d-dterbuii = Phase badge
    .doc = Where a run is, as a status badge.

d-2u2ny6gr = Outcome badge
    .doc = How a step that did not run to its end ended, as a status badge; nothing for one that did.

d-gctjtdv7 = Desk
    .doc = The desk: what this device shows.

d-6jkhhsvr = Desk around
    .doc = The desk, with the arrangement `front` filling the room between its bar and its dock when there is one, as its preset shows it (`shown_preset`).

d-p2yoggx4 = Screen
    .doc = The desk as the whole of a display (`tilo-session --shell screen`): the bar, the dock and the desk's surfaces around the person's arrangement as the preset they picked shows it, each app's window standing in the well its place leaves it, as a window of the desk shows it (`desk_view`).

d-bhxvwkho = Shown preset
    .doc = What stands between the bar and the dock when the desk shows the arrangement: the places as the preset the person picked lays them out, under the switch between the presets, as `desk_view` shows them.

d-afjfzjl6 = Bar on edge
    .doc = The top bar on the edge it rides: the whole width with its parts spread along it at the `full` fit, or an inset run centred on the edge at the floating one, where it covers less of what is under it.

d-su7ogm3c = Top bar
    .doc = The bar's parts in the person's order, as the run the person reorders by dragging a part: what the desk's bar and the desk window's bar both draw.

d-37bjhdfb = At rest
    .doc = The desk at rest under its bar: the banner a notice rose as.

d-6a3axsap = Surface
    .doc = The surface the person is in, at the place its kind shows: the launcher fills the desk under the bar, the overview and the workspaces fill it, the centre sits at the trailing edge and the composer at the foot.

d-ojscef66 = Dock
    .doc = The dock: a run of items on the edge the person set (the launcher, the apps and resources they pinned, and the apps running now), each with its badge, and the ones that want the person marked.

d-lwre5gd7 = Dock run
    .doc = The dock's items in a run the person rearranges by dragging one onto another place.

d-lwt7iwsj = Dock tile
    .doc = One dock item: its glyph, with its badge on the glyph's top trailing corner (the count of what is new, or the mark that it wants the person), as a dock has always badged an app.

d-dpqvjh2f = Launcher
    .doc = The launcher and command palette: search and launching over everything the person can reach, typed at once.

d-tst43bqy = Window row
    .doc = One window the search found: what it is, what it shows, and where it is.

d-hlv2uw7o = Desk doors
    .doc = The desk's own doors, as rows that open them: each surface, and the ways to make and join a workspace.

d-outvdzmi = Layouts
    .doc = Layouts: the presets a person picks among, each a named set of the settings `chrome.tl` holds, and the individual settings underneath for anyone who wants to tune one thing.

d-5q3okayl = Layout card
    .doc = One preset's card: its name, what it is, and a miniature of the shell it makes, drawn from the preset's own settings rather than pictured.

d-mwph24zz = Preview
    .doc = A preset's shell in miniature: where its bar and dock stand, how they sit, and what they hold, drawn with the same views the shell itself uses at the size a card gives them.

d-cpowx55e = Preview bar
    .doc = The miniature bar: its parts, left to right, in the fit the preset gives it.

d-ebrt7kdg = Preview dock
    .doc = The miniature dock: the launcher and three items, on the fit the preset gives it.

d-grqdyggg = Shell settings
    .doc = The settings underneath the presets: the bar's and the dock's, each change one action.

d-p7ismolp = Bar parts
    .doc = The bar's parts, in the person's order, each shown or hidden, and the two the bar can gain.

d-6phinc3w = Part row
    .doc = One bar part in the settings: its glyph, its name, and whether it shows.

d-e25dashr = Dock settings
    .doc = The dock's settings the form does not hold: what it shows besides the pinned items.

d-clgbrhsn = Notices
    .doc = The notifications: the banner that arrives, and the centre that keeps them.

d-wud3t5ro = Notice group
    .doc = One sender's notices, under its name: each with its title, what it says and its actions.

d-t3ywtwqj = Notice row
    .doc = One notice: its title, what it says, its sender, its actions and the way to dismiss it.

d-yyk2izra = Banner
    .doc = A banner: the newest notice, risen over the desk, with its first action and a dismiss.

d-gke2gvrj = Banner full
    .doc = A banner on a desk with the room: the notice with its kind's tone and its first action.

d-wh7n4fhq = Banner row
    .doc = A banner on a narrow desk: the notice as a row, its actions under it.

d-rcjzvfxp = Overview
    .doc = The overview: what the desk holds, to search and move things in, over the windows it covers.

d-f5tl6n63 = Rooms here
    .doc = The workspaces the person is in, as chips that switch to them, and how many people are here.

d-7nxhynyj = Picture
    .doc = One strip: its windows as places left to right, each opening what it shows and carrying its menu of moves.

d-nxpbvfox = Window place
    .doc = One window's place: its picture, its title and the app it shows, lifted while it has focus, with a menu that moves it to another strip or workspace, closes it, or asks about it.

d-6l3wldtm = Window menu
    .doc = A window's commands, where its place shows them: going there, carrying it to another strip or workspace, asking about it, and closing it.

d-6nn35yb7 = Found
    .doc = What a search finds across the person's other workspaces: each window, going there or moving it here.

d-kxdzjhoo = Window line
    .doc = One window of another workspace, as a row that opens there, with the way to bring it here.

d-luuijvwf = Key hints launcher
    .doc = The keys a surface shows under it, as `ui.KeyHints`: the drawn keycaps of each chord and what pressing them does, side by side, in the person's own keyboard legends.

d-z4hw2mcw = Key hints switcher
    .doc = The keys of the switcher: hold the modifier, Tab walks, letting go goes there.

d-uhzjvuan = People button
    .doc = Who is here, and their menus.

d-uvwnwa4d = Agent chip
    .doc = An agent here, as its presence: a chip that opens its menu.

d-e7u7zirf = Agent line
    .doc = An agent here with what it is doing and what it last changed: its face, its activity line and the mark of what it did, which opens the run's log.

d-teccuwja = Quick settings
    .doc = The quick settings: Wi-Fi, Bluetooth, dark mode and do-not-disturb as tiles, the volume and the brightness as sliders, the battery and the ways to leave the session.

d-yxl6yq2r = Wifi tile
    .doc = Wi-Fi, on while it is on, naming the network it is joined to; it cannot be pressed on a device without it.

d-d5wzqn37 = Bluetooth tile
    .doc = Bluetooth, on while it is on; it cannot be pressed on a device without it.

d-uzi5ah2q = Dark tile
    .doc = Dark mode: on while every app draws dark, each with the theme the person chose for it.

d-s66oqjwe = Quiet tile
    .doc = Do not disturb: on while banners and sounds wait.

d-w3fe7tjv = Levels
    .doc = The volume and the brightness, each where the device has it: dragging sets it as the device reads it back.

d-lcjohgdk = Battery line
    .doc = The battery's charge and whether it is charging, on a device that has one.

d-ycck5awo = Power row
    .doc = Locking the screen, and the ways to leave the session or the device.

d-wdjxaekq = Workspaces
    .doc = The workspace overview: every workspace the person is in, with who is there and what it holds, to switch to, join, or make one.

d-wcb3o55u = Room card
    .doc = One workspace: its name, who is in it, and what happened last.

d-r34xb7ak = Preset switch
    .doc = The preset switch: the three ways of showing the same arrangement, the one in use lit, so a person moves between the strips, the framed canvas and the spaces canvas as shell.md §6 picks.

d-qrahog47 = Strips view
    .doc = The desk: its rows stacked and scrolled vertically, each row's strips scrolled sideways.

d-xwdhqtyz = Row of
    .doc = One row: the strip across the desk, named, with its columns of places side by side.

d-7oiz3phb = Strip column
    .doc = One strip: a column of places in the shell's frame, headed by the strip's name (what its place in front shows) and lit while it is the strip in front; a press on it brings it to the front.

d-scru4wch = Strip controls
    .doc = A strip's controls under its name: its width, one of the three, and the controls of the place in front (its pin, closing it, moving it to another row of the desk), each committing what a drag would.

d-so5z33zr = Tab chip
    .doc = One tab of a column: the place it stands for, lit when it is the place in front.

d-74cbhzyb = Place content
    .doc = A place's content: its resource drawn inline through the view it names, as its own app's code (shell.md §8, "The canvas composes views"), or the well the app's window shows in when there is no resource here to draw (a terminal, a legacy window).

d-43am22do = Place view
    .doc = What stands where a place's content goes: its record drawn through the view the place names (its type's default view when it names none), as its own app's code, or the well the app's window shows in when the place has no record this shell can draw (a terminal, a legacy window).

d-z4vc77e6 = Place controls
    .doc = A place's frame chrome: its pinned mark, and its pin, close and move controls.

d-fziuz57n = Switcher
    .doc = The window switcher: the windows of the desk in the order Alt+Tab walks them, in a panel over what the person is in.

d-dswgb57x = Window card
    .doc = One window of the walk: its picture or its app's glyph, its title, and the app it shows.

d-jzxijptr = Window names
    .doc = The window's title and the app it shows, under its picture or beside it.

d-3bxeb2i2 = Window face
    .doc = What a window's card shows of the window itself: its live picture where this names it, over the app's glyph.

d-4hp326dv = Traditional desktop
    .doc = The traditional desktop (`docs/designs/app-language/shell.md` §4, §8.2 step 5): the preset furthest from the strips and the canvas.

d-uekksuqv = Front window
    .doc = The window in front alone, filling the room it is given: what a compact output shows of the desktop (`desk`), where windows standing where they were left would show their corners cut off and the window the person is in could stand behind another.

d-6izhhsie = Desktop frame
    .doc = One frame on the desktop, at its strip's canvas point and size.

d-fabwkpdf = Frame classic
    .doc = A place's frame (§4): the window, with the shell's controls for the place at the trailing end of its bar.

d-efc4bnyi = Frame buttons
    .doc = A frame's controls: the way to carry the place to another row, and close.

d-u55srubm = Action bar
    .doc = The focused window's commands, as a macOS menu bar draws an app's: the window's name, then the actions its resource publishes (exactly what the app's window header offers), the frequent ones as controls and the rest in the run each action's declaration names.

d-ysqdgpzc = Action run
    .doc = The window's name, then the frequent commands of `offered` as bar controls, then one menu per run the app declared, in the order the app declared them, and the commands that name no run under the window's own name.

d-35o7co3i = Action control
    .doc = One command the person reaches for often: a bar control, lifted while the state it toggles is on.

d-cijhlzxf = Window commands
    .doc = The shell's own commands on a window whose record it cannot read (a foreign surface): its name, moving it to another row, and closing it.

d-yx7doxgr = Traditional bar
    .doc = The desktop's top bar, one row edge to edge: the system menu and the focused window's commands leading, and the tasks running, the time, who is here, the status, the notifications and the person's own face flush at the trailing edge, as a macOS or GNOME bar has them.

d-3ezewqgy~row0 = Bar for part

d-pvl6wwx6~row0 = Bar part for t

d-pvl6wwx6~row1 = Bar part for item

d-vidcax4k~row0 = Workspace menu for w

d-m4owmri7~row0~row0 = Framed canvas for p

d-m4owmri7~row0 = Framed canvas for s

d-nt2inbrl~row0~row0 = Spaces canvas for p

d-nt2inbrl~row0 = Spaces canvas for s

d-dz2lhlbc~row0 = Conversation for e

d-ojxyofwa~row0 = Program step for x

d-lwre5gd7~row0 = Dock run for item

d-lwre5gd7~row1 = Dock run for item

d-lwre5gd7~row2 = Dock run for item

d-p7ismolp~row0 = Bar parts for part

d-clgbrhsn~row0 = Notices for g

d-t3ywtwqj~row0 = Notice row for a

d-wh7n4fhq~row0 = Banner row for a

d-f5tl6n63~row0 = Rooms here for r

d-6l3wldtm~row0 = Window menu for s

d-6l3wldtm~row1 = Window menu for r

d-qrahog47~row0 = Strips view for workspace

d-7oiz3phb~row0 = Strip column for p

d-z4vc77e6~row0 = Place controls for r

d-4hp326dv~row0~row0 = Traditional desktop for p

d-4hp326dv~row0 = Traditional desktop for s

d-4hp326dv~row1~row0 = Traditional desktop for p

d-4hp326dv~row1 = Traditional desktop for s

d-efc4bnyi~row0 = Frame buttons for r

d-ysqdgpzc~row0 = Action run for c

d-ysqdgpzc~row1 = Action run for c

d-ysqdgpzc~row2 = Action run for c

d-ysqdgpzc~row3~row0 = Action run for b

d-ysqdgpzc~row3 = Action run for c

d-cijhlzxf~row0 = Window commands for r

d-yx7doxgr~row0 = Traditional bar for t
