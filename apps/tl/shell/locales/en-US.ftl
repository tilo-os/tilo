# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-shell = Shell

d-4jk2lkpt = Desk
    .doc = This device's desk: the time in the person's style, who they are, their notifications, the workspaces on this device, the tasks running for them and the surface the desk shows over its windows.
    .noun = { $count ->
        [one] desk
       *[other] desks
    }

d-kx4rprro = Person

d-6unuw4qc = Here
    .doc = The surface the person is in, such as the overview or the notifications; empty at rest.

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
    .doc = The question the person is asked about themselves; empty when none waits.

d-w2fyvufg = Ask detail
    .doc = What answering the question brings.

d-zqu6ku2o = Ask answer
    .doc = The question's answer, as its button says it.

d-eewsi53q = Ask open
    .doc = Whether the question stands open on the person's avatar.

d-gi7d6wct = Weekday

d-5cs3vm3f = Date

d-hj6eqjwk = Quiet

d-c5k5ua2t = Workspace

d-xpcwi5mg = Strip

d-vbyis266 = Workspaces

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

d-s7dh3s7h = Apps
    .doc = The applications installed on this device: what the launcher lists and opens.

d-n2fzxj5u = Styles
    .doc = The desk styles every installed package offers, as the session publishes them: each a view of the arrangement (`shell.md` §6), listed by its own words.

d-4jk2lkpt-filter-switcher_revealed = Switcher revealed

d-4jk2lkpt-filter-timer_paused = Timer paused

d-4jk2lkpt-filter-timer_stopwatch = Timer stopwatch

d-4jk2lkpt-filter-ask_open = Ask open

d-4jk2lkpt-filter-quiet = Quiet

d-4jk2lkpt-filter-bar_reached = Bar reached

d-4jk2lkpt-filter-bar_covered = Bar covered

d-4jk2lkpt-filter-dock_reached = Dock reached

d-4jk2lkpt-filter-dock_covered = Dock covered

d-zxwtaxaw = Desk style
    .doc = A desk style: a view of the arrangement an installed app declares.
    .noun = { $count ->
        [one] desk style
       *[other] desk styles
    }

d-ly2aksp3 = Key
    .doc = The style's view reference, which the record keeps it by.

d-t6ojc3kt = Style
    .doc = The view reference: a package and a view, what `Viewing.style` names.

d-yxdj6rhs = Package
    .doc = The package that declares it.

d-uew43mty = Name
    .doc = The view's own name, as its declaration writes it.

d-rhx3oobw = Line
    .doc = The view's doc line, in its author's words: what the switch lists it by.

d-ifgxec27 = Loads
    .doc = Whether the style loads: false for one whose app is not here or does not work with this desk, or whose view failed when it ran, so the switch can say so and the desk shows the built-in strips until it is fixed.

d-eihyiyeb = Reason
    .doc = Why the style does not load, in the words it failed with, when it does not; empty when it does.

d-zxwtaxaw-filter-loads = Loads

d-dkwmlyh7 = App
    .doc = An application installed on this device, as the launcher lists it: its package id, its name in the person's language, the glyph it shows, and how it opens.
    .noun = { $count ->
        [one] app
       *[other] apps
    }

d-wabxc4xe = Key
    .doc = Its package id, which `open_app` names it by.

d-dhizmikp = Name
    .doc = Its name in the person's interface language.

d-bkmyu6to = Icon
    .doc = The glyph it shows, by the platform icon's kebab-case name.

d-fxhx35ly = Category
    .doc = Its category (`work`, `media`, `play`, `system`), empty when it named none.

d-vebrnbn2 = Description
    .doc = What it is for, in a few words; empty when it offers none.

d-bynckgvt = Workspace
    .doc = A workspace on this device.
    .noun = { $count ->
        [one] workspace
       *[other] workspaces
    }

d-pmjwp2sk = Key

d-nze6cia2 = Name

d-ce523tqy = Current

d-bynckgvt-filter-current = Current

d-tlbg2fms = Tray item
    .doc = A running application's tray item: what the bar shows, and what a press on it reaches.
    .noun = { $count ->
        [one] tray item
       *[other] tray items
    }

d-o3wuotce = Key

d-e5qbz5cz = Title

d-lsd5bmss = Menu

d-tlbg2fms-filter-menu = Menu

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

d-c7yq7qkl = Workspace key
    .doc = Its workspace's id, as switching to it names the workspace.

d-bard6ca3 = Group key
    .doc = The window it is a tab of: a group's tabs are one window on the desk, and its other tabs stand behind it.

d-lbxt5665 = Tab index
    .doc = Its place among its group's tabs: 0 for the tab in front, then the tabs behind it in their order.

d-kiwhpw3z = Share
    .doc = Its share of its strip's width, 0..1: the width the window keeps on the desk.

d-hm2x5otu = Local
    .doc = Whether it runs on this device: false for a window on another of the person's devices, or on a peer's.

d-wxi227u2 = Device
    .doc = The device that runs it, when it is not this one: its key, empty when it is not known.

d-aadqdflh = Device name
    .doc = That device's name, as its certificate labels it.

d-ctnewsrr = Place
    .doc = Where it stands in the picture of its workspace: 1 for the first tab of the first group of the first strip, on along them, strips top to bottom.

d-z3pjamxl = Recency

d-a5fwclo5 = Focused

d-tb2jierp = Preview

d-4agha6k5 = Not responding
    .doc = Whether its app stopped answering: its card says so, and offers to keep waiting for the app or to quit it.

d-r7dcmwzl-filter-tab_index = Tab index

d-r7dcmwzl-filter-share = Share

d-r7dcmwzl-filter-local = Local

d-r7dcmwzl-filter-place = Place

d-r7dcmwzl-filter-recency = Recency

d-r7dcmwzl-filter-focused = Focused

d-r7dcmwzl-filter-not_responding = Not responding

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

d-p6gfsj72 = Seat
    .doc = Whether it is seated at the person's agent door now, a client they admitted, rather than a run's own agent.

d-labby3mp-filter-unseen = Unseen

d-labby3mp-filter-seat = Seat

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

d-jlqw5py5-filter-current = Current

d-jlqw5py5-filter-present = Present

d-zk7pzrt2 = Notice
    .doc = Something a sender told the person here: an app, or the agent task that did.
    .noun = { $count ->
        [one] notice
       *[other] notices
    }

d-jq4onnp4 = Key

d-7snyky6u = Title

d-wauytevp = Body

d-nxunr2dc = Origin app

d-bsp6shbm = Kind

d-z67wqj3i = Seen

d-zk7pzrt2-filter-seen = Seen

d-7rjfrgeo = Notice action
    .doc = One action a notice offers, as its sender named it.
    .noun = { $count ->
        [one] notice action
       *[other] notice actions
    }

d-ohcc5fba = Notice

d-mfyylqsl = Key

d-6ehy2qwa = Label

d-qhf2pyze = Arrangement
    .doc = The person's arrangement: what is open where.
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

d-3uxxhan3 = Current
    .doc = Whether it is the workspace in front, which the desk shows.

d-wydxt4zu-filter-current = Current

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

d-vn23musu-filter-width = Width

d-vn23musu-filter-maximized = Maximized

d-vn23musu-filter-placed = Placed

d-vn23musu-filter-canvas_x = Canvas x

d-vn23musu-filter-canvas_y = Canvas y

d-vn23musu-filter-canvas_width = Canvas width

d-vn23musu-filter-canvas_height = Canvas height

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

d-joj2ey7d = Name
    .doc = What this place is called, on every surface: its window's title and its app's own name (`Groceries` in `Notes`, the one the person picked), or the app's name alone when the window has no title.

d-aonjenrr = Pinned
    .doc = Whether the person pinned it: an agent's arrangement leaves it alone.

d-rqrtd6vq = Shell
    .doc = The shell that made it; empty reads as this shell's.

d-gpmk4o3r-filter-pinned = Pinned

d-nkjtmr33 = Viewing
    .doc = What the person shows of their arrangement, and which strip stands in front: the shell's own view of it, in their own record (`records: .own`, `root viewing: Viewing`).
    .noun = { $count ->
        [one] viewing
       *[other] viewings
    }

d-kna5sco4 = Style
    .doc = The desk style the person shows, as a view reference: a package and a view (`so.tilo.shell:strips`, `me.styles:timeline`).

d-nl27ikjq = Showing
    .doc = The preset the person showed before styles were views.

d-6b2coqll = Focused
    .doc = The strip in front, by its key; empty for none.

d-nkjtmr33-filter-showing = Showing

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

d-4zknrelh = Settings open
    .doc = Whether the chosen style's own settings stand over the desk (`style_settings`), opened from the bar's style menu: the one place a person changes the style they show without an agent, as `shell.md` §6's "Two ways to change a desk" says.

d-dwct4abs = Strips tabs
    .doc = Strips: whether a strip of several places shows its tab bar.

d-gfmzi3qr = Frames snap
    .doc = Frames: whether a place let go on the canvas lands on the style's grid, so frames line up as they are placed.

d-cejnvssb-filter-preset = Preset

d-cejnvssb-filter-based_on = Based on

d-cejnvssb-filter-tuned = Tuned

d-cejnvssb-filter-bar_shown = Bar shown

d-cejnvssb-filter-bar_edge = Bar edge

d-cejnvssb-filter-bar_fit = Bar fit

d-cejnvssb-filter-bar_finish = Bar finish

d-cejnvssb-filter-bar_showing = Bar showing

d-cejnvssb-filter-bar_size = Bar size

d-cejnvssb-filter-dock_shown = Dock shown

d-cejnvssb-filter-dock_edge = Dock edge

d-cejnvssb-filter-dock_fit = Dock fit

d-cejnvssb-filter-dock_finish = Dock finish

d-cejnvssb-filter-dock_showing = Dock showing

d-cejnvssb-filter-dock_size = Dock size

d-cejnvssb-filter-dock_running = Dock running

d-cejnvssb-filter-dock_launcher = Dock launcher

d-cejnvssb-filter-dock_zoom = Dock zoom

d-cejnvssb-filter-settings_open = Settings open

d-cejnvssb-filter-strips_tabs = Strips tabs

d-cejnvssb-filter-frames_snap = Frames snap

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

d-sfhd6wko-filter-slot = Slot

d-sfhd6wko-filter-shown = Shown

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
    .doc = The glyph it shows, by its icon's name.

d-k376lylv = Pinned
    .doc = Whether the person pinned it; a running app that is not pinned is there while it runs.

d-5vpgvzqb = Running
    .doc = Whether it runs now.

d-mgcmvxjx = Badge
    .doc = How many things it has new, as its badge shows.

d-e7ft4mly = Attention
    .doc = Whether it wants the person: a pulsing badge, and an attention mark.

d-bnfk7mx5-filter-pinned = Pinned

d-bnfk7mx5-filter-running = Running

d-bnfk7mx5-filter-badge = Badge

d-bnfk7mx5-filter-attention = Attention

d-clhqzrkb = Width
    .doc = How wide a strip stands within its row, in per-mille of the row's width.

d-hrjwxtwf = Three tenths of the row: a list, a set of options
    .doc = Three tenths of the row: a list, a set of options.

d-ipingyqi = Two thirds of the row: a note, a conversation
    .doc = Two thirds of the row: a note, a conversation.

d-nfufnx7j = The row's whole usable width
    .doc = The row's whole usable width.

d-vvznzcd4 = Preset
    .doc = A way of laying out the same resources and actions a person picked before the desk had styles: `Viewing.showing` kept one.

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
    .doc = How much room the top bar has: for the focused window's menu, and for its parts.

d-w3dorpbd = The most: the menu by its window's name, the tasks and who is here too
    .doc = The most: the menu by its window's name, the tasks and who is here too.

d-qqg4smi5 = The menu by its window's name, but not the tasks and who is here
    .doc = The menu by its window's name, but not the tasks and who is here.

d-7caakb5b = The least: the menu stands as the window's icon, and only what a phone keeps stays
    .doc = The least: the menu stands as the window's icon, and only what a phone keeps stays.

d-p4bqyuzt = Name the style
    .doc = A desk style picked before styles were views: the preset's name turned into this package's view of it (`strips` into `so.tilo.shell:strips`), so the person's choice is kept.

d-7j7tzynq = Show as
    .doc = Shows the desk through the style `shown`: a package and a view (`so.tilo.shell:strips`).
    .w-it-is-shown-that-way-already = It is shown that way already

d-dkylvhw4 = In front
    .doc = Makes the strip `strip` the one in front: what a press on a column's header does.

d-nljxfsom = Move it here
    .doc = Puts a resource dropped on the canvas where it was let go.

d-zoqbhs7o = Seed
    .doc = A shell that has never been arranged: the bar's parts and the dock's, in the order the default shell shows them.

d-vglzmkiy = Drop the power part
    .doc = A shell that kept the retired `power` part beside the status loses it.

d-g7yuomid = Drop the tasks part
    .doc = A shell that kept the retired `tasks` part loses it: what an agent does is the turn of a conversation, which Chat shows, so the bar has no running-tasks cluster.

d-eanbdi6l = Use this layout
    .doc = Uses the preset `to`: every setting it names at once.

d-rpbpwrcy = Show the app's menus
    .doc = Puts the focused app's menus on the bar, after the system's: what the app in front can do now, on its resource.
    .w-the-app-s-menus-are-on = The app's menus are on the bar already

d-7jggsdfm = Hide the app's menus
    .doc = Takes the focused app's menus off the bar.

d-2k74oe7b = Back to the preset
    .doc = Returns the shell to the preset it was based on, dropping the person's tuning.
    .w-nothing-was-changed-from-the-preset = Nothing was changed from the preset

d-jqwkinjd = Bar on
    .doc = Puts the bar on `edge`.

d-u6k2x6mp = Set bar fit

d-i2mddhin = Set bar finish

d-w4d7htty = Set bar showing

d-fdnwo2gt = Set bar size

d-fcxzqnum = Show the top bar
    .doc = Shows or hides the top bar: the one control that turns it on and off, so a person sees whether it is on where they press it.

d-5l5rmk47 = Dock on
    .doc = Moves the dock to `edge`, keeping how it is built.

d-jllttdkc = Set dock fit

d-uw5xrdst = Set dock finish

d-iyws3u4w = Set dock showing

d-quwdhi4n = Set dock size

d-23faedwo = Show the dock
    .doc = Shows or hides the dock: the one control that turns it on and off, so a person sees whether it is on where they press it.

d-exyxrzml = Running apps
    .doc = Whether the dock shows the apps running that nobody pinned.

d-vlnaqn4u = Launcher entry
    .doc = Whether the dock has a launcher entry.

d-z2w4wlev = Magnify the dock
    .doc = Whether the dock magnifies the item under the pointer.

d-vurzkuat = Move
    .doc = Puts a bar part with the one it comes before, wherever the part stands now: the one action a drag commits, and the same change from the settings, a shortcut or an agent.
    .w-a-part-cannot-come-before-itself = A part cannot come before itself

d-4ljvsals = Move
    .doc = Puts a bar part at the place `at`, counted from the leading edge: what a drag that names a place gives.

d-5esgamkb = Shown on the bar
    .doc = Shows or hides one part of the bar.

d-hphul6cv = Move
    .doc = Puts a dock item with the one it comes before: the one action a drag commits.
    .w-an-item-cannot-come-before-itself = An item cannot come before itself

d-tv7bsor6 = Move
    .doc = Puts a dock item at the place `at`, counted from the leading edge: what a drag that names a place gives.

d-utk4ewgf = Pin to the dock
    .doc = Pins a resource or an app to the dock, before `next` (or at the end when no neighbour is named): what dragging one onto the dock does, and what the person does from a menu.
    .w-is-on-the-dock-already = { $title } is on the dock already

d-5yz5iwzs = Remove from the dock
    .doc = Takes an item off the dock.

d-aitkuflo = Open item
    .doc = What a press on a dock item does, by the app the item stands for: a window of that app on this desk comes to the front, so the desk says which window the person is in and the top bar's commands are its own; the window is the one the item was drawn for, so a press never opens a second copy.

d-5giytccr = Open first
    .doc = Enter's work on a query: the first app the text finds opens, so typing "no" and pressing Enter opens Notes; a query that names no app is what search everywhere answers, as Enter has always done for what the desk's own rows do not hold.

d-vjnksgzm = Seed waiting
    .doc = Two notices a sender left and the person has not seen: what the shell stages on a desk it shows before the person has caught up, so the bar's bell has something to count.

d-j4ax4otm = Seed notices
    .doc = The desk's notices a scenario starts from: what apps and an agent said today.

d-zm35wbjg = A workspace
    .doc = Adds a workspace, with one row, to `a`, and returns the row's key.

d-ztflhbbf = A strip
    .doc = Adds a strip named `name` to `row`, standing at `width`, and returns its key.

d-chvuqgur = A note place
    .doc = Adds a place showing the note at `index` of the notebook to `strip`, and returns its key.

d-tnecawb6 = A foreign place
    .doc = A place of a kind this build has no record for: it stands as it is, which is what a place another shell made looks like.

d-b3rfswle = Show place
    .doc = Shows the place `place` and brings its strip forward with it: what choosing a tab does, and the same arranging action a script or an agent runs.

d-jipcl6u3 = Desk style settings
    .doc = The desk style's own settings, and the two ways of changing a desk (`docs/designs/app-language/ shell.md` §6): without an agent, in the one place a person's settings live, and with an agent, by the same actions the person's controls run.

d-wxxxvca4 = Close
    .doc = Closes them.

d-33wxlq7h = Desk style settings
    .doc = Shows or hides their being open: one control for both, so a person sees the state where they press it.

d-zitymayc = Strip tabs
    .doc = Whether the strip of several places shows its tab bar: the strips style's own setting.

d-lfa3rl6s = Snap places to a grid
    .doc = Whether a place let go on the framed canvas lands on the grid: the frames style's own setting.

d-i6ll2n4j = Desk view

d-72tgr4px = Clock
    .w-paused = Paused

d-3ezewqgy = Bar
    .w-top-bar = Top bar

d-pvl6wwx6 = Bar part
    .w-tilo = Tilo

d-vidcax4k = Workspace menu
    .w-workspaces = Workspaces
    .w-new-workspace = New workspace
    .w-join-a-workspace = Join a workspace
    .w-workspace-settings = Workspace settings
    .w-overview = Overview

d-hac3p23j = Volume status
    .w-volume-muted = Volume, muted
    .w-volume = Volume, { $percent }%

d-iwv7cd5i = Network status
    .w-wi-fi = Wi-Fi, { $called }
    .w-wi-fi-2 = Wi-Fi
    .w-wired = Wired, { $called }
    .w-wired-2 = Wired
    .w-offline = Offline

d-agni63m4 = Battery status
    .w-charging = { $percentage }%, charging

d-moh3hzne = Bell
    .w-notifications-silenced = Notifications silenced
    .w-notifications = Notifications
    .w-1-unread = { $name }, 1 unread
    .w-unread = { $name }, { $waiting } unread

d-fniigjed = Viewing settings
    .w-desk = Desk
    .w-switching-a-style-keeps-every-window = Switching a style keeps every window where it is.

d-gapwwnqc = Show style~present

d-s7yqm7cb = Canvas
    .w-find-something-or-ask = Find something, or ask

d-t3cux3yd = Framed place

d-s6b5stgz = Space content

d-ewgdlyyr = Show app menus~present

d-npxnqobp = Reset~present

d-vpc43pes = Show bar~present

d-nn5zjt6d = Show dock~present

d-vedyud4d = Move part~present

d-gkca5uj2 = Move item~present

d-i4odzov3 = Pin~present

d-gctjtdv7 = Desk

d-zzd3yipu = Strips

d-tj4z2obq = Frames
    .w-canvas = Canvas

d-tqbdxbtn = Spaces
    .w-spaces = Spaces

d-pbhumuny = Desktop

d-gb3g4lte = Dock ground
    .w-dock = Dock

d-afjfzjl6 = Bar on edge
    .w-top-bar = Top bar

d-su7ogm3c = Top bar

d-37bjhdfb = At rest

d-6a3axsap = Surface

d-ojscef66 = Dock

d-lwre5gd7 = Dock run

d-fr6w4hgp = Dock running tiles

d-lwt7iwsj = Dock tile
    .w-new = { $title }, { $badge } new

d-dpqvjh2f = Launcher
    .w-search-apps-windows-actions-and-files = Search apps, windows, actions and files
    .w-apps = Apps
    .w-all-apps = All apps
    .w-windows = Windows
    .w-recent-windows = Recent windows
    .w-nothing-here-matches = Nothing here matches
    .w-search-everywhere-for-it = Search everywhere for it
    .w-open-as-a-tab = Open as a tab
    .w-search-everywhere-for = Search everywhere for { $query }

d-7womqcp3 = App row

d-tst43bqy = Window row

d-hlv2uw7o = Desk doors
    .w-the-desk = The desk
    .w-overview = Overview
    .w-workspaces = Workspaces
    .w-notifications = Notifications

d-outvdzmi = Layouts
    .w-layout = Layout

d-5q3okayl = Layout card
    .w-in-use = In use

d-mwph24zz = Preview

d-cpowx55e = Preview bar
    .w-file = File
    .w-edit = Edit
    .w-view = View

d-ebrt7kdg = Preview dock

d-grqdyggg = Shell settings
    .w-top-bar = Top bar
    .w-show-the-top-bar = Show the top bar
    .w-dock = Dock
    .w-show-the-dock = Show the dock

d-p7ismolp = Bar parts
    .w-parts-in-order = Parts, in order

d-6phinc3w = Part row
    .w-shown = Shown
    .w-hidden = Hidden

d-e25dashr = Dock settings
    .w-running-apps = Running apps
    .w-launcher-entry = Launcher entry
    .w-magnify-the-item-under-the-pointer = Magnify the item under the pointer

d-clgbrhsn = Notices
    .w-notifications = Notifications
    .w-clear-all = Clear all
    .w-nothing-to-catch-up-on = Nothing to catch up on
    .w-search-notifications = Search notifications
    .w-nothing-matches-that = Nothing matches that
    .w-clear-the-search-to-see-every = Clear the search to see every notice

d-wud3t5ro = Notice group

d-t3ywtwqj = Notice row
    .w-dismiss = Dismiss

d-yyk2izra = Banner

d-gke2gvrj = Banner full
    .w-open = Open

d-wh7n4fhq = Banner row
    .w-dismiss = Dismiss

d-rcjzvfxp = Overview
    .w-overview = Overview
    .w-new-workspace = New workspace
    .w-join-a-workspace = Join a workspace
    .w-search-windows = Search windows
    .w-no-windows = No windows
    .w-open-something-to-see-its-window = Open something to see its window

d-f5tl6n63 = Rooms here
    .w-here = { $len } here

d-7nxhynyj = Picture

d-nxpbvfox = Window place

d-6l3wldtm = Window menu
    .w-this-window = This window
    .w-go-there = Go there
    .w-close = Close

d-6nn35yb7 = Found
    .w-nothing-matches = Nothing matches
    .w-windows = Windows

d-kxdzjhoo = Window line
    .w-this-window = This window
    .w-go-there = Go there
    .w-move-here = Move here

d-luuijvwf = Key hints launcher
    .w-up = Up
    .w-down = Down
    .w-choose = Choose
    .w-enter = Enter
    .w-open = Open
    .w-esc = Esc
    .w-close = Close

d-z4hw2mcw = Key hints switcher
    .w-alt = Alt
    .w-tab = Tab
    .w-walk = Walk
    .w-enter = Enter
    .w-go-there = Go there
    .w-esc = Esc
    .w-stay = Stay

d-fehlcpo5 = Hung window
    .w-not-responding = Not responding
    .w-wait = Wait
    .w-force-quit = Force quit

d-uhzjvuan = People button
    .w-people = People
    .w-is-here = { $name } is here
    .w-and-are-here = { $name } and { $name2 } are here
    .w-and-others-are-here = { $name } and { $others } others are here

d-uvwnwa4d = Agent chip
    .w-stop = Stop

d-teccuwja = Quick settings
    .w-quick-settings = Quick settings

d-yxl6yq2r = Wifi tile
    .w-wi-fi = Wi-Fi

d-d5wzqn37 = Bluetooth tile
    .w-bluetooth = Bluetooth

d-uzi5ah2q = Dark tile
    .w-dark-mode = Dark mode

d-s66oqjwe = Quiet tile
    .w-do-not-disturb = Do not disturb

d-w3fe7tjv = Levels
    .w-volume = Volume
    .w-brightness = Brightness

d-lcjohgdk = Battery line

d-ycck5awo = Power row
    .w-lock = Lock
    .w-power = Power
    .w-sleep = Sleep
    .w-log-out = Log out
    .w-restart = Restart
    .w-shut-down = Shut down

d-wdjxaekq = Workspaces
    .w-workspaces = Workspaces
    .w-name-a-new-workspace = Name a new workspace
    .w-create = Create
    .w-join-a-workspace = Join a workspace

d-wcb3o55u = Room card

d-xzb2gybw = Style switch

d-kssxjs7v = Style menu
    .w-showing = Showing { $style_title }
    .w-settings-for = Settings for { $style_title }

d-qrahog47 = Strips view
    .w-no-window-here-yet = No window here yet

d-4qeo26jr = Workspace rows

d-7qrrtifg = Rows of the desk
    .w-no-window-here-yet = No window here yet

d-7sxqbtn2 = Row window

d-2fofsa7f = Strip window
    .w-nothing-here = Nothing here

d-p5wzoqr5 = Window controls
    .w-this-window = This window
    .w-narrow = Narrow
    .w-normal = Normal
    .w-wide = Wide
    .w-bring-to-front = Bring to front
    .w-unpin-window = Unpin window
    .w-pin-window = Pin window
    .w-restore = Restore
    .w-maximize = Maximize
    .w-close = Close

d-6bfcv6wf = Window actions
    .w-window-actions = Window actions

d-z4vc77e6 = Place controls
    .w-pin-place = Pin place
    .w-move-to-row = Move to row
    .w-close = Close

d-oyjbbzbk = Place content

d-43am22do = Place view

d-pjkvdmwk = Show style settings~present

d-vseev2rl = Style panel
    .w-settings-for = Settings for { $style_title }
    .w-close = Close
    .w-comes-from-and-its-settings-are = { $style_label } comes from { $package }, and its settings are its package's own.

d-g476xvac = Desktop style settings
    .w-show-the-top-bar = Show the top bar
    .w-show-the-dock = Show the dock

d-e3tczbcu = Strips style settings
    .w-show-the-tabs-of-a-strip = Show the tabs of a strip

d-mnbjennf = Frames style settings
    .w-snap-places-to-a-grid = Snap places to a grid

d-xwhxaimf = Spaces style settings
    .w-spaces-shows-each-place-as-its = Spaces shows each place as its own view and has nothing to set.

d-fziuz57n = Switcher
    .w-of = { $at } of { $count }
    .w-no-windows = No windows
    .w-open-something-to-switch-to-it = Open something to switch to it

d-dswgb57x = Window card

d-jzxijptr = Window names

d-3bxeb2i2 = Window face

d-sk6mvhah = Window picture

d-4hp326dv = Traditional desktop
    .w-desktop = Desktop

d-uekksuqv = Front window

d-6izhhsie = Desktop frame

d-fabwkpdf = Frame classic

d-efc4bnyi = Frame buttons
    .w-move-to-row = Move to row
    .w-close = Close

d-u55srubm = Action bar

d-cijhlzxf = Window commands
    .w-close = Close

d-yx7doxgr = Traditional bar
    .w-tilo = Tilo
