# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-shell = Shell

d-ndaefmcq = Picking
    .doc = What waits for the person's yes: every call that needs it, read from its own record in the workspace's invocations.
    .noun = { $count ->
        [one] picking
       *[other] pickings
    }

d-5xjx7oct = Folder
    .doc = The folder shown, by its path; empty until a question asks for files.

d-5esxwq2o = Entries
    .doc = What the folder holds, as it was last read.

d-tlhktser = Call edit
    .doc = One change the person made to the call before approving it: a field, by its path in the call's arguments, and what they wrote in it.
    .noun = { $count ->
        [one] call edit
       *[other] call edits
    }

d-r6c6zjdg = Path

d-5fhfjzcm = Text

d-cejnvssb = Shell
    .doc = The person's own shell document, reached as `shell` (`root shell: Shell`).
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

d-ftcph3ii = Cards corner
    .doc = The corner of the display in use the cards of what is live dock in: the person's choice, on every device they use.

d-ffa2ohzm = Dock running
    .doc = The dock shows the apps running even when nobody pinned them.

d-h5oo7js2 = Dock launcher
    .doc = The dock has a launcher entry.

d-5n4s55tz = Dock zoom
    .doc = A click on the dock's icons magnifies the item under the pointer, as macOS's Dock does.

d-7xqpekfu = Settings open
    .doc = Whether the chosen style's own settings stand over the shell (`style_settings`), opened from the bar's style menu: the one place a person changes the style they show without an agent, as `shell.md` §6 says.

d-ayatksvb = Strips tabs
    .doc = Strips: whether a strip of several places shows its tab bar.

d-fyquhn4g = Frames snap
    .doc = Frames: whether a place let go on the canvas lands on the style's grid, so frames line up as they are placed.

d-6bamt22e = Canvas asked
    .doc = The canvas: whether the person has asked on it yet.

d-dpb7af4j = Reporting
    .doc = Whether the report picker stands over the windows: the person is choosing the window a report about a problem is about (`feedback.tl`).

d-7bxopjro = Report picks
    .doc = The windows the report being made is about, in the order they were picked: the shell's own record of what the person pointed at, so a report is about something they can read back.

d-r6satdro = Report area
    .doc = The outline of the rectangle the person dragged over the picker, as `x, y, width, height` in canvas pixels: empty while no drag stands.

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

d-cejnvssb-filter-cards_corner = Cards corner

d-cejnvssb-filter-dock_running = Dock running

d-cejnvssb-filter-dock_launcher = Dock launcher

d-cejnvssb-filter-dock_zoom = Dock zoom

d-cejnvssb-filter-settings_open = Settings open

d-cejnvssb-filter-strips_tabs = Strips tabs

d-cejnvssb-filter-frames_snap = Frames snap

d-cejnvssb-filter-canvas_asked = Canvas asked

d-cejnvssb-filter-reporting = Reporting

d-sfhd6wko = Bar part
    .doc = One part of the top bar, in the person's order.
    .noun = { $count ->
        [one] bar part
       *[other] bar parts
    }

d-fjzbw5un = Kind
    .doc = What the part is; the bar draws it from this, so a part moved is a part the same, and its name is the one that part goes by.

d-pbly7ymy = Shown
    .doc = Whether it shows on the bar; a part off keeps its place in the document.

d-sfhd6wko-filter-kind = Kind

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
    .doc = Its name on the dock, and what assistive technology reads: the app's or the resource's own, and empty for the launcher, which the dock names from this package's words.

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

d-66jw6ezz = Report pick
    .doc = Reporting a problem: the screen dims to the desktop's ground, every window of the workspace in front stands as a card with its own live picture, and the person chooses the window the report is about — a click on its card, or a rectangle dragged over it.
    .noun = { $count ->
        [one] report pick
       *[other] report picks
    }

d-hb2dxuji = Key

d-hu4bokd4 = Title

d-abe7lbj4 = App name

d-oomdh5mi = Layout
    .doc = The person's layout: what is open where.
    .noun = { $count ->
        [one] layout
       *[other] layouts
    }

d-uykuo63i = Person
    .doc = The person at this device, by their name.

d-k4p3z4ee = Front
    .doc = The strip whose window has the keyboard on this device, by its key; empty for none.

d-yb7sokzs = People
    .doc = The others here in the workspace in front.

d-7n7jrcg3 = Workspaces

d-hqul2vwj = Rows

d-7lq6xkzz = Strips

d-2ug5q5cq = Places
    .doc = Every place, each naming the strip it is in, so the views need not walk the tree.

d-dt4vnrqa = Queries
    .doc = The queries the canvas stands while an app answers them, by the place that shows each: what the person typed, what it found, or why nothing did (shell.md §8).

d-fqirvfzw = Refs
    .doc = The resources those queries found, each naming the query it is of.

d-cswol6ca = Recents
    .doc = What the person opened lately in the workspace in front, read off the workspace's history of what its windows showed: each resource once, theirs first, newest first.

d-rjnlecke = Layout recent
    .doc = A resource the person opened lately: what it is, its app and type, and who opened it last and when.
    .noun = { $count ->
        [one] layout recent
       *[other] layout recents
    }

d-zyqmwz53 = Key
    .doc = Its id: the resource's address.

d-g5lvltn4 = Resource
    .doc = The resource, by its address, as `session.open` takes it.

d-33ygyyvo = Title

d-rgnzsrl5 = Origin app
    .doc = The app that keeps it, by its canonical id.

d-qegnrgvk = App title
    .doc = That app's name.

d-wpv5ccaw = Kind
    .doc = Its type, as its app publishes it.

d-nk3qn5vg = By
    .doc = Who opened it last, by name.

d-n77p6zrw = At

d-5xciib7g = Mine
    .doc = Whether the person looking opened it last themselves.

d-rjnlecke-filter-at = At

d-rjnlecke-filter-mine = Mine

d-zsaib3op = Layout query
    .doc = A query a place shows while its app answers it: what the person typed, in which app, and what came of it.
    .noun = { $count ->
        [one] layout query
       *[other] layout queries
    }

d-6jget5cg = Key
    .doc = Its id: the place showing it.

d-iwuvfj66 = Text
    .doc = What the person typed.

d-ybpst64r = In app
    .doc = The app asked, by its canonical id.

d-ibasoiis = Kind
    .doc = The kind of resource the query names, as its app publishes it (`tube.channel`).

d-a6ewjjom = State
    .doc = How the search stands: `searching`, `one`, `several`, `none` or `failed`.

d-xsjcdedm = Reason
    .doc = Why it failed, in the app's own words; empty while it does better.

d-jq4ftwnw = Layout ref
    .doc = A resource a query found: its address, the name it goes by, and its kind, the same three words a place's document is named by.
    .noun = { $count ->
        [one] layout ref
       *[other] layout refs
    }

d-k652vouu = Key

d-vdsinb5l = Query
    .doc = The query it is of, by its key.

d-vj5mgxi6 = Address

d-pnom4n55 = Title

d-7giiatvv = Kind

d-njeklucw = Layout workspace
    .doc = A workspace the person's layout shows.
    .noun = { $count ->
        [one] layout workspace
       *[other] layout workspaces
    }

d-5zibpqs6 = Key

d-b2vzzczj = Name

d-hwfwvs3f = Current
    .doc = Whether it is the workspace in front, which the shell shows.

d-jbtiqjt4 = Home
    .doc = Where the workspace opens as a page on the web (`/w/<workspace>/desktop`), when the shell is served there: what its card links to.

d-njeklucw-filter-current = Current

d-dpuvzi4j = Layout person
    .doc = Someone else here in the workspace in front: a person or an agent.
    .noun = { $count ->
        [one] layout person
       *[other] layout people
    }

d-3rpmhtjf = Key

d-6a7urcxy = Name

d-4s5jxxte = Layout row
    .doc = A row of the layout: a strip across a workspace's width, holding columns.
    .noun = { $count ->
        [one] layout row
       *[other] layout rows
    }

d-3xc5xbon = Key

d-v5ef7tkv = Workspace

d-iqgu3ggi = Name

d-sm4scdu2 = Current
    .doc = Whether it is the row in front in its workspace.

d-bowkspu3 = Shell
    .doc = The shell that made it; empty reads as this shell's.

d-4s5jxxte-filter-current = Current

d-w25t6rl7 = Layout strip
    .doc = A strip: a column of places, with a width, in a row of a workspace.
    .noun = { $count ->
        [one] layout strip
       *[other] layout strips
    }

d-dufcke3u = Key

d-x42tiiqe = Workspace

d-dvberkpa = Row

d-ssbg6dg6 = Display
    .doc = The display this device shows the strip on, by its id (`device.screen.output`): each part of the shell draws its own display's strips.

d-7ekq6qgi = Width
    .doc = Its width in per-mille of its row's usable width (333 narrow, 667 normal, 1000 wide).

d-2cbikm5m = Maximized

d-yk2oxjrj = Active
    .doc = The place in front, by its id; empty for none.

d-44lq6lon = Placed
    .doc = Whether it has a place on its workspace's canvas.

d-vbv2zrob = Canvas x

d-jvrwsckz = Canvas y

d-m75kjoue = Canvas width

d-s3lpes2e = Canvas height

d-s6rivl3a = Canvas hugs
    .doc = Whether `canvas_height` is the most it takes: what it shows is as tall as what it holds up to it (a thing found where the person clicked).

d-pznsxbgq = Shell

d-w25t6rl7-filter-display = Display

d-w25t6rl7-filter-width = Width

d-w25t6rl7-filter-maximized = Maximized

d-w25t6rl7-filter-placed = Placed

d-w25t6rl7-filter-canvas_x = Canvas x

d-w25t6rl7-filter-canvas_y = Canvas y

d-w25t6rl7-filter-canvas_width = Canvas width

d-w25t6rl7-filter-canvas_height = Canvas height

d-w25t6rl7-filter-canvas_hugs = Canvas hugs

d-farfsl4d = Layout place
    .doc = A place: one window, showing a resource through a view.
    .noun = { $count ->
        [one] layout place
       *[other] layout places
    }

d-ba3kyokq = Key

d-7dno3n47 = Strip
    .doc = The strip it is in.

d-jpzk4cli = Shows
    .doc = The app showing the resource, by its canonical id (`so.tilo.terminal`).

d-nprpusuo = Shown as
    .doc = The view it shows the resource through; empty for the app's default.

d-ofkshr4v = Resource
    .doc = The document it shows, when it shows one.

d-qqcejiim = Title
    .doc = The title it is shown under.

d-jeatudax = Name
    .doc = What this place is called, on every surface: its window's title and its app's own name (`Groceries` in `Notes`, the one the person picked), or the app's name alone when the window has no title.

d-vkbhstef = Pinned
    .doc = Whether the person pinned it: an agent's layout leaves it alone.

d-soy4vimv = Shell
    .doc = The shell that made it; empty reads as this shell's.

d-farfsl4d-filter-pinned = Pinned

d-nkjtmr33 = Viewing
    .doc = What the person shows of their layout: the shell's own view of it, in their own workspace (`resources: .own`, `root viewing: Viewing`).
    .noun = { $count ->
        [one] viewing
       *[other] viewings
    }

d-kna5sco4 = Style
    .doc = The style the person shows, as a view reference: a package and a view (`so.tilo.shell:strips`, `me.styles:timeline`).

d-nxjnwtsd = Edge
    .doc = The person's shell: which parts the top bar and the dock have, in what order, on which edge, how they look and when they show.

d-rbzbofjd = Top

d-e4zlwr65 = Bottom

d-tzvst7nv = Left

d-qykb6xix = Right

d-tcsafce7 = Card corner
    .doc = The corner of the display the cards of what is live dock in.

d-ufe4dez5 = Top left

d-y4tcqd6i = Top right

d-kes3aytt = Bottom left

d-wt6lph2u = Bottom right

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

d-sscc6gs3 = While no window covers it

d-v2eyzsgr = At the screen's edge

d-62fazfo4 = After a moment

d-v3z2zasp = Size
    .doc = How large the parts are.

d-2yxm7xfn = Small

d-uv33v3c2 = Regular

d-5czgy2f6 = Large

d-vtiohdqj = Bar part kind
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

d-socv3cfz = Chrome preset
    .doc = A named set of the settings, which the person applies in one action: a chrome preset.

d-cb4a3qsa = The dock on the bottom with the bar on the top: Tilo's own
    .doc = The dock on the bottom with the bar on the top: Tilo's own.

d-qmzl573l = A floating, rounded, translucent dock at the bottom that hides while a window covers it, and a top bar with the focused app's menus after the system's
    .doc = A floating, rounded, translucent dock at the bottom that hides while a window covers it, and a top bar with the focused app's menus after the system's.

d-iriqaem6 = A full-width taskbar at the bottom, always shown
    .doc = A full-width taskbar at the bottom, always shown.

d-n44stfvj = The top bar alone; the overview on demand
    .doc = The top bar alone; the overview on demand.

d-dah75qd2 = No chrome at rest; the edges reveal what the person needs
    .doc = No chrome at rest; the edges reveal what the person needs.

d-zsqlezfj = The traditional desktop: frames floating over the shell, the focused window's own commands in the top bar, and a floating dock at the foot, the shape macOS and GNOME people know
    .doc = The traditional desktop: frames floating over the shell, the focused window's own commands in the top bar, and a floating dock at the foot, the shape macOS and GNOME people know.

d-q7rxwd72 = Bar room
    .doc = How much room the top bar has: for the focused window's menu, and for its parts.

d-w3dorpbd = The most: the menu by its window's name, the tasks and who is here too
    .doc = The most: the menu by its window's name, the tasks and who is here too.

d-qqg4smi5 = The menu by its window's name, but not the tasks and who is here
    .doc = The menu by its window's name, but not the tasks and who is here.

d-7caakb5b = The least: the menu stands as the window's icon, and only what a phone keeps stays
    .doc = The least: the menu stands as the window's icon, and only what a phone keeps stays.

d-fbymgkoh = Rename workspace
    .doc = Name the workspace `key` what the person typed: the workspace's own published `rename`, so the name reaches every device and every list of it.

d-fbymgkoh-key = Key

d-fbymgkoh-name = Name

d-lqy7sqnc = Rename strip
    .doc = Name the strip `key` what the person typed: one arranging action of the session's, so a script and an agent rename as a press does.

d-lqy7sqnc-key = Key

d-lqy7sqnc-name = Name

d-aegu5nc6 = Don't allow
    .doc = Decline the question at `key`: its asked record's own `resolve`, called by its address as any caller calls it.

d-aegu5nc6-key = Key

d-4pwuckls = Open folder
    .doc = Show the folder `path` to choose files in, as this device reads it now.

d-4pwuckls-path = Path

d-lqtiqtq2 = Close folder
    .doc = Stop showing a folder: the question that asked for files is answered.

d-ukun4zmh = Asked on the canvas
    .doc = The person asked on the canvas: the hint the empty canvas says until then has been said (`Shell.canvas_asked`), and stays away from now on.

d-2e7aappl = Size it
    .doc = Gives the place `key` the size a drag of its edge or corner left it at, `size` its width and height in canvas pixels: one arranging step of the person's, its corner where it stood.

d-2e7aappl-key = Key

d-2e7aappl-size = Size

d-ce3pp2or = Drew place
    .doc = Keeps how tall the hugging place `key` is drawn now, `size` carrying its height in canvas pixels as the map lays it out: the free area under it on the canvas is what the person sees there (canvas.md §1, §7), though the place could grow into it.

d-ce3pp2or-key = Key

d-ce3pp2or-size = Size

d-nljxfsom = Move it here
    .doc = Puts a resource dropped on the canvas where it was let go.

d-nljxfsom-target = Target

d-nljxfsom-at = At

d-zoqbhs7o = Seed
    .doc = What a new shell starts with: the bar's parts and the dock's, in the order the default shell shows them.

d-vglzmkiy = Drop the power part
    .doc = A shell that kept the retired `power` part beside the status loses it.

d-g7yuomid = Drop the tasks part
    .doc = A shell that kept the retired `tasks` part loses it: what an agent does is the turn of a conversation, which Chat shows, so the bar has no running-tasks cluster.

d-eanbdi6l = Use this layout
    .doc = Uses the preset `layout`: every setting it names at once.

d-eanbdi6l-layout = Layout

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

d-jqwkinjd-edge = Edge

d-u6k2x6mp = Set bar fit

d-u6k2x6mp-fit = Fit

d-i2mddhin = Set bar finish

d-i2mddhin-finish = Finish

d-w4d7htty = Set bar showing

d-w4d7htty-showing = Showing

d-fdnwo2gt = Set bar size

d-fdnwo2gt-size = Size

d-fcxzqnum = Show the top bar
    .doc = Shows or hides the top bar: the one control that turns it on and off, so a person sees whether it is on where they press it.

d-fcxzqnum-on = Shown

d-5l5rmk47 = Dock on
    .doc = Moves the dock to `edge`, keeping how it is built.

d-5l5rmk47-edge = Edge

d-jid2lypn = Dock cards
    .doc = Docks the cards of what is live in the corner `corner` of the display in use.

d-jid2lypn-corner = Corner

d-jllttdkc = Set dock fit

d-jllttdkc-fit = Fit

d-uw5xrdst = Set dock finish

d-uw5xrdst-finish = Finish

d-iyws3u4w = Set dock showing

d-iyws3u4w-showing = Showing

d-quwdhi4n = Set dock size

d-quwdhi4n-size = Size

d-23faedwo = Show the dock
    .doc = Shows or hides the dock: the one control that turns it on and off, so a person sees whether it is on where they press it.

d-23faedwo-on = Shown

d-exyxrzml = Running apps
    .doc = Whether the dock shows the apps running that nobody pinned.

d-exyxrzml-on = In the dock

d-vlnaqn4u = Launcher entry
    .doc = Whether the dock has a launcher entry.

d-vlnaqn4u-on = In the dock

d-z2w4wlev = Magnify the dock
    .doc = Whether the dock magnifies the item under the pointer.

d-z2w4wlev-on = Under the pointer

d-vurzkuat = Move
    .doc = Puts a bar part with the one it comes before, wherever the part stands now: the one action a drag commits, and the same change from the settings, a shortcut or an agent.
    .w-a-part-cannot-come-before-itself = A part cannot come before itself

d-vurzkuat-part = Part

d-vurzkuat-before = Before

d-4ljvsals = Move
    .doc = Puts a bar part at `position`, counted from the leading edge: what a drag that names a place gives.

d-4ljvsals-part = Part

d-4ljvsals-position = Position

d-5esgamkb = Shown on the bar
    .doc = Shows or hides one part of the bar.

d-5esgamkb-part = Part

d-5esgamkb-on = On the bar

d-hphul6cv = Move
    .doc = Puts a dock item with the one it comes before: the one action a drag commits.
    .w-an-item-cannot-come-before-itself = An item cannot come before itself

d-hphul6cv-item = Item

d-hphul6cv-before = Before

d-tv7bsor6 = Move
    .doc = Puts a dock item at `position`, counted from the leading edge: what a drag that names a place gives.

d-tv7bsor6-item = Item

d-tv7bsor6-position = Position

d-utk4ewgf = Pin to the dock
    .doc = Pins a resource or an app to the dock, before `next` (or at the end when no neighbour is named): what dragging one onto the dock does, and what the person does from a menu.
    .w-is-on-the-dock-already = { $title } is on the dock already

d-utk4ewgf-key = App

d-utk4ewgf-title = Title

d-utk4ewgf-icon = Icon

d-utk4ewgf-next = Before

d-5yz5iwzs = Remove from the dock
    .doc = Takes an item off the dock.

d-5yz5iwzs-item = Item

d-aitkuflo = Open item
    .doc = What a press on a dock item does, by the app the item stands for: a window of that app on this screen comes to the front, so the shell says which window the person is in and the top bar's commands are its own; the window is the one the item was drawn for, so a press never opens a second copy.

d-aitkuflo-key = App

d-2cmvxqpt = Report a problem
    .doc = Start a pick: what a report about a problem is about, chosen on the whole screen.

d-fipjigt5 = Pick report window
    .doc = The window `key` is what the report is about: it joins the report, the picker closes, and the report's box opens about the window, the person free to point at the part of it that is wrong.

d-fipjigt5-key = Window

d-y6cjt65t = Pick the window a report is about
    .doc = The rectangle `area` the person dragged over the picker — its x, y, width and height in canvas pixels — picks the window it covers first, as a click on that card does.

d-y6cjt65t-area = Area

d-zek2dnmp = Cancel
    .doc = Leave the pick with nothing attached: no window, no report.

d-46uwq3gq = Close History
    .doc = Puts the surface away once a recent resource is opened again beside the window in front, by the same `open` an agent and a reference's press run.

d-rs2j4mmu = Choose launcher row
    .doc = What choosing the row the person reached does, read off the row's value: opening an application or a window here, running a row the search found, or one of the shell's own surfaces.

d-rs2j4mmu-choice = Choice

d-rs2j4mmu-current = Current

d-q6bzrju7 = Open surface
    .doc = Open the shell's surface `surface` (`overview`, `workspaces`, `centre`), as the bar's own controls do.

d-q6bzrju7-surface = Surface

d-n2ucmbpe = Close launcher
    .doc = Leave the launcher: the surface stack goes back to the shell at rest.

d-7j7tzynq = Show as
    .doc = Shows the shell through the style `style`: a package and a view (`so.tilo.shell:strips`).
    .w-it-is-shown-that-way-already = It is shown that way already

d-7j7tzynq-style = Style

d-hnvxrx3k = Toggle vpn
    .doc = Turn the VPN on through its first profile, or off through the one connected: the tile's one press, whichever way it flips.

d-hnvxrx3k-v = V

d-zm35wbjg = A workspace
    .doc = Adds a workspace, with one row, to `a`, and returns the row's key.

d-zm35wbjg-name = Name

d-st426t4p = A front workspace
    .doc = Adds the workspace in front, with its row in front, to `a`, and returns the row's key.

d-st426t4p-name = Name

d-ztflhbbf = A strip
    .doc = Adds a strip named `name` to `row` at `width`, and returns its key.

d-ztflhbbf-row = Row

d-ztflhbbf-name = Name

d-ztflhbbf-width = Width

d-nynnosmh = A strip on
    .doc = Adds a strip named `name` to `row` at `width`, shown on `display`, and returns its key.

d-nynnosmh-row = Row

d-nynnosmh-name = Name

d-nynnosmh-width = Width

d-nynnosmh-display = Display

d-chvuqgur = A note place
    .doc = Adds a place showing the note at `index` of the notebook to `strip`, and returns its key.

d-chvuqgur-strip = Strip

d-chvuqgur-index = Index

d-tnecawb6 = A foreign place
    .doc = A place of a kind this build has no document for: it stands as it is, which is what a place another shell made looks like.

d-tnecawb6-strip = Strip

d-tnecawb6-index = Index

d-tnecawb6-title = Title

d-bmd7xmln = A query place
    .doc = Adds a place showing a search for `text` to `strip`, with the two matches the notes app answered, and returns the place's key: the state a place stands in until the person chooses one of the resources it found (shell.md §8).

d-bmd7xmln-strip = Strip

d-bmd7xmln-text = Text

d-5o2gn6hf = A style settings
    .doc = A shell of one note in the style named, with that place in front, and the style's own settings open over it: what a person sees who opens them from the bar's style menu.

d-5o2gn6hf-style = Style

d-d63pkwee = Recents of an afternoon
    .doc = Ada's opens of an afternoon and two of Mia's, newest first, as the session lists them.

d-u6kdedav = Query show
    .doc = Show `found` in the place `place` while it shows a query: the same place, now the found resource (shell.md §8).

d-u6kdedav-place = Place

d-u6kdedav-found = Found

d-4vtnv5px = Query retry
    .doc = Look for what the place `place`'s search names again.

d-4vtnv5px-place = Place

d-mtt6d3af = Tab it here
    .doc = Tab the place whose window's bar was dragged onto the strip `s` into that strip: a person drags one window's bar onto another's to make its tabs, and the window's menu's "Move into … as a tab" runs the same arranging action.

d-mtt6d3af-s = S

d-mtt6d3af-target = Target

d-n2ez6yop = Rearrange
    .doc = Rank the place `place` at `at` among its strip's tabs: what dragging a pill within the window's bar does, as the shell's own `move` with the place it goes before.

d-n2ez6yop-s = S

d-n2ez6yop-place = Place

d-n2ez6yop-at = At

d-d64rg4sw = Its own window
    .doc = Give the place `place` a window of its own: what dragging a tab out of its window's bar does, as the shell's own `split-right` for the window in front.

d-d64rg4sw-place = Place

d-b3rfswle = Show place
    .doc = Shows the place `place` and brings its window forward with it, the keyboard in it: what choosing a tab does, and the same arranging action a script or an agent runs.

d-b3rfswle-place = Place

d-wln2heq6 = Bring to front
    .doc = Brings the window of the strip `s` to the front, the keyboard in it: the session brings its place in front forward, as it does for a press, a script or an agent alike.

d-wln2heq6-s = S

d-mnpvowua = Move the window left
    .doc = Move the window `w` before the window `before` of its strip: one place along it.

d-mnpvowua-w = The window

d-mnpvowua-before = The window it goes before

d-c4363mpd = Move the window right
    .doc = Move the window `w` one place to the trailing side: before the window after the next, or last in its strip while none stands there.

d-c4363mpd-w = The window

d-x6ly4cml = Equal widths
    .doc = Give every window of the row the same share of it: what "every window the same" does where a person sees their windows side by side in a band.

d-x6ly4cml-w = The window

d-3ajs72ce = Style settings
    .doc = The style's own settings, and the two ways of changing a style (`docs/designs/app-language/ shell.md` §6): without an agent, in the one place a person's settings live, and with an agent, by the same actions the person's controls run.

d-33qlgb4v = Close
    .doc = Closes them.

d-nhawvsi6 = Style settings
    .doc = Shows or hides their being open: one control for both, so a person sees the state where they press it.

d-nhawvsi6-on = On

d-2ycpcfh4 = Strip tabs
    .doc = Whether the strip of several places shows its tab bar: the strips style's own setting.

d-2ycpcfh4-on = On

d-wjsjhild = Snap places to a grid
    .doc = Whether a place let go on the framed canvas lands on the grid: the frames style's own setting.

d-wjsjhild-on = On

d-4xei7g27 = Folder line
    .w-your-workspace = Your workspace
    .w-the-server = the server
    .w-is-paused = { $name } is paused
    .w-hasn-t-reached-this-computer-yet = { $name } hasn't reached this computer yet
    .w-is-waiting-for = { $name } is waiting for { $server }
    .w-isn-t-syncing = { $name } isn't syncing: { $stuck_reason }
    .w-sending-changes-in-to = Sending changes in { $name } to { $server }
    .w-some-files-in-aren-t-on = Some files in { $name } aren't on { $server } yet
    .w-is-in-sync-with = { $name } is in sync with { $server }

d-dywbbsoi = Stuck reason
    .w-hasn-t-said-which-files-it = { $server } hasn't said which files it holds yet
    .w-hasn-t-answered-for-some-files = { $server } hasn't answered for some files yet
    .w-part-of-the-file-list-hasn = part of the file list hasn't arrived from { $server } yet
    .w-some-changes-wait-for-others-to = some changes wait for others to reach { $server } first
    .w-has-no-room-for-more-changes = { $server } has no room for more changes yet; they're kept here until it has
    .w-the-workspace-doesn-t-let-you = the workspace doesn't let you make some of these changes; ask one of its owners
    .w-isn-t-answering-changes-are-sent = { $server } isn't answering; changes are sent again shortly
    .w-didn-t-take-some-changes-they = { $server } didn't take some changes; they're sent again shortly
    .w-some-files-details-are-on-neither = some files' details are on neither this computer nor { $server }
    .w-some-changes-haven-t-reached-yet = some changes haven't reached { $server } yet; sending them again

d-qxbh2c44 = Part name
    .w-tilo = Tilo
    .w-workspace = Workspace
    .w-app-menus = App menus
    .w-strip = Strip
    .w-tasks = Tasks
    .w-clock = Clock
    .w-people = People
    .w-status = Status
    .w-battery = Battery
    .w-agents = Agents
    .w-gap = Gap

d-y45hdphl = Dock item name
    .w-launcher = Launcher

d-mxfb2qdk = Where line
    .w-working-in = Working in { $title }
    .w-viewing = Viewing { $title }
    .w-in = In { $in_app }
    .w-in-2 = In { $call }

d-fcxy4mu5 = Shell view
    .w-new-workspace = New workspace
    .w-join-a-workspace = Join a workspace
    .w-invite-someone = Invite someone
    .w-workspace-settings = Workspace settings
    .w-overview = Overview

d-72tgr4px = Clock
    .w-paused = Paused

d-3ezewqgy = Bar
    .w-top-bar = Top bar

d-pvl6wwx6 = Bar part
    .w-tilo = Tilo
    .w-overview = Overview
    .w-undo-layout-change = Undo layout change
    .w-redo-layout-change = Redo layout change
    .w-settings = Settings
    .w-about-this-computer = About this computer
    .w-open-your-card = { $ask }: open your card
    .w-open-your-card-2 = { $person }: open your card
    .w-you-open-your-card = You: open your card

d-vidcax4k = Workspace menu
    .w-workspaces = Workspaces
    .w-workspace-name = Workspace name
    .w-new-workspace = New workspace
    .w-join-a-workspace = Join a workspace
    .w-invite-someone = Invite someone
    .w-rename = Rename { $name }
    .w-delete = Delete { $name }
    .w-workspace-settings = Workspace settings
    .w-overview = Overview
    .w-history = History

d-rfepr4yx = Strip part
    .w-strip-name = Strip name
    .w-new-strip = New strip
    .w-rename = Rename { $name }
    .w-delete = Delete { $name }
    .w-move-left = Move { $name } left
    .w-move-right = Move { $name } right

d-nojvjjii = Strips list
    .w-strips = Strips
    .w-new-strip = New strip
    .w-done = Done
    .w-history = History

d-dgtjjxex = Person menu
    .w-style-and-workspaces = { $me }: style and workspaces
    .w-settings-for = Settings for { $style_title }
    .w-workspaces = Workspaces
    .w-new-workspace = New workspace
    .w-join-a-workspace = Join a workspace
    .w-workspace-settings = Workspace settings
    .w-overview = Overview
    .w-history = History
    .w-about-tilo = About Tilo
    .w-tilo = Tilo
    .w-who-is-here = Who is here
    .w-people = People
    .w-your-card = Your card

d-zlo3t3an = Capture status
    .w-watching = Watching { $label }
    .w-watching-and-more = Watching { $label } and { $more } more
    .w-watching-2 = Watching

d-hac3p23j = Volume status
    .w-volume-muted = Volume, muted
    .w-volume = Volume, { $percent }%

d-iwv7cd5i = Network status
    .w-wi-fi = Wi-Fi, { $called }
    .w-wi-fi-2 = Wi-Fi
    .w-wired = Wired, { $called }
    .w-wired-2 = Wired
    .w-offline = Offline

d-vipg3gla = Sync status

d-agni63m4 = Battery status
    .w-charging = { $percentage }%, charging

d-moh3hzne = Bell
    .w-notifications-silenced = Notifications silenced
    .w-notifications = Notifications
    .w-1-question-waiting = { $name }, 1 question waiting
    .w-questions-waiting = { $name }, { $asked } questions waiting
    .w-1-unread = { $name }, 1 unread
    .w-unread = { $name }, { $waiting } unread

d-o4f6z532 = Picking folder
    .w-no-folder-open = No folder open
    .w-a-question-that-asks-for-files = A question that asks for files opens one here

d-buc46sub = Asked over

d-i7735z6w = Asked banners

d-x266v635 = More asked
    .w-1-more-question = 1 more question
    .w-more-questions = { $more } more questions

d-vpg3b467 = Asking list
    .w-waiting-for-you = Waiting for you

d-uahwpm3g = Asked card

d-xfpagjt5 = Question

d-d4a7v5zp = Question body
    .w-undo-can-t-bring-this-back = Undo can't bring this back
    .w-this-is-answered-on-the-device = This is answered on the device that asked it
    .w-fill-in-what-it-asks-for = Fill in what it asks for first
    .w-don-t-answer = Don't answer
    .w-always-let = Always let { $asker_name } { $verb } { $value }
    .w-change = Change
    .w-don-t-allow = Don't allow

d-wo44tm5w = Files asked
    .w-choose = Choose
    .w-save = save
    .w-folder = folder
    .w-open = open
    .w-up = Up
    .w-there-is-no-folder-above-this = There is no folder above this one to go to
    .w-recent = Recent
    .w-name = Name
    .w-modified = Modified
    .w-size = Size
    .w-hidden-files = Hidden files
    .w-nothing-here-matches = Nothing here matches “{ $query }”
    .w-no-recent-files = No recent files
    .w-no-folders-here = No folders here
    .w-this-folder-is-empty = This folder is empty
    .w-folder-2 = Folder
    .w-files-chosen = { $len } files chosen
    .w-cancel = Cancel
    .w-save-2 = Save
    .w-choose-folder = Choose folder
    .w-open-2 = Open
    .w-recent-files-hold-no-folder-to = Recent files hold no folder to save in or to choose
    .w-name-the-file-first = Name the file first
    .w-choose-a-file-first = Choose a file first
    .w-replace = Replace “{ $named }”?
    .w-replace-2 = Replace
    .w-already-holds-a-file-with-this = { $picker_name } already holds a file with this name. Replacing it writes over what it holds.

d-nsvxusve = Asker

d-s7yqm7cb = Canvas
    .w-click-anywhere-and-say-what-you = Click anywhere and say what you want
    .w-caret = Caret
    .w-ask-or-find-anything = Ask or find anything
    .w-send = Send
    .w-type-what-you-want-first = Type what you want first

d-vi5fveaq = Model choice

d-kzhftbrb = Canvas place

d-t3cux3yd = Framed place

d-s6b5stgz = Frameless place

d-ewgdlyyr = Show app menus~present

d-npxnqobp = Reset~present

d-vpc43pes = Show bar~present

d-nn5zjt6d = Show dock~present

d-vedyud4d = Move part~present

d-gkca5uj2 = Move item~present

d-i4odzov3 = Pin~present

d-ojscef66 = Dock

d-lwre5gd7 = Dock run

d-fr6w4hgp = Dock running tiles

d-lwt7iwsj = Dock tile
    .w-new = { $title }, { $badge } new

d-7ugeomrq = Report picker
    .w-what-the-report-is-about = What the report is about
    .w-nothing-is-open-to-report-about = Nothing is open to report about
    .w-click-the-window-that-isn-t = Click the window that isn't working
    .w-nothing-is-open-to-report-about-2 = Nothing is open to report about.
    .w-click-the-window-that-isn-t-2 = Click the window that isn't working, or drag over it.
    .w-cancel = Cancel

d-lcq7ufsf = History
    .w-clear = Clear
    .w-nothing-opened-lately = Nothing opened lately
    .w-what-you-open-shows-up-here = What you open shows up here

d-6eidrgir = Recent row

d-dk2iupmd = Identify card

d-dpqvjh2f = Launcher
    .w-search-apps-windows-actions-and-files = Search apps, windows, actions and files
    .w-apps = Apps
    .w-all-apps = All apps
    .w-windows = Windows
    .w-recent-windows = Recent windows
    .w-welcome = Welcome
    .w-the-shell = The shell
    .w-overview = Overview
    .w-workspaces = Workspaces
    .w-notifications = Notifications
    .w-open-as-a-tab = Open as a tab
    .w-open = Open

d-fniigjed = Viewing settings
    .w-shell = Shell
    .w-switching-a-style-keeps-every-window = Switching a style keeps every window where it is.

d-gapwwnqc = Show style~present

d-outvdzmi = Layouts
    .w-chromepreset = ChromePreset

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
    .w-edge = Edge
    .w-fit = Fit
    .w-material = Material
    .w-when-it-shows = When it shows
    .w-size = Size
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

d-yd6yk7ui = Live pill
    .w-fold-back = Fold { $live_name } back
    .w-live = { $live_name }, live
    .w-show-as-a-card = Show as a card
    .w-more-live = { $value } more live

d-3nryt5p3 = Live section
    .w-live-now = Live now

d-5wjrqfa3 = Live row
    .w-on = On { $device }

d-nqzenak5 = Live card controls
    .w-show-on-the-bar = Show on the bar
    .w-keep-off-the-bar = Keep off the bar
    .w-back-to-the-bar = Back to the bar
    .w-show-as-a-card = Show as a card
    .w-put-away = Put away

d-3dxyousy = Native row

d-nraevli5 = Live control

d-elqi6l5x = Live cards

d-n2v5vhnx = Live card
    .w-card = { $live_name } card
    .w-on = { $source } on { $device }
    .w-card-place = Card place
    .w-back-in-the-corner = Back in the corner
    .w-cards-at-the = Cards at the { $lower }
    .w-back-to-the-bar = Back to the bar
    .w-put-away = Put away
    .w-in = { $live_name } in { $source }

d-ele2mmbh = Card body
    .w-position-in = Position in { $live_name }

d-clgbrhsn = Notices
    .w-notifications = Notifications
    .w-clear-all = Clear all
    .w-nothing-to-catch-up-on = Nothing to catch up on
    .w-search-notifications = Search notifications
    .w-nothing-matches-that = Nothing matches that
    .w-clear-the-search-to-see-every = Clear the search to see every notice

d-ytz24ncc = Undo band
    .w-notifications-cleared = Notifications cleared
    .w-undo = Undo

d-wud3t5ro = Notice group

d-t3ywtwqj = Notice row
    .w-hand-to-agent = Hand to agent
    .w-dismiss = Dismiss

d-yyk2izra = Banner
    .w-1-more-notification = 1 more notification
    .w-more-notifications = { $more } more notifications

d-gke2gvrj = Banner full
    .w-open = Open

d-wh7n4fhq = Banner row
    .w-answer = Answer
    .w-decline = Decline
    .w-snooze = Snooze
    .w-read = Read
    .w-dismiss = Dismiss

d-76zs5p4r = Reply row
    .w-reply = Reply
    .w-send = Send
    .w-write-a-reply-first = Write a reply first

d-ny7ms2ry = Osd readout

d-rcjzvfxp = Overview
    .w-overview = Overview
    .w-new-workspace = New workspace
    .w-join-a-workspace = Join a workspace
    .w-search-windows = Search windows
    .w-no-windows = No windows
    .w-open-something-to-see-its-window = Open something to see its window

d-766s7y4k = Workspaces here
    .w-here = { $len } here

d-7nxhynyj = Picture

d-nxpbvfox = Window place

d-6nn35yb7 = Found
    .w-nothing-matches = Nothing matches
    .w-windows = Windows

d-kxdzjhoo = Window line

d-luuijvwf = Key hints launcher
    .w-up = Up
    .w-down = Down
    .w-choose = Choose
    .w-tab = Tab
    .w-next-group = Next group
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

d-pdda7z3y = Hung card

d-tbvykixf = Crashed card
    .w-app-ended = App ended
    .w-reopen = Reopen
    .w-dismiss = Dismiss

d-n6arlzpq = Window elsewhere
    .w-on = { $origin_app } on { $device_name }
    .w-on-another-device = { $origin_app } on another device
    .w-this-window-runs-on-that-device = This window runs on that device. Open it there to see it.

d-uhzjvuan = People button
    .w-people = People
    .w-is-here = { $name } is here
    .w-and-are-here = { $name } and { $name2 } are here
    .w-and-others-are-here = { $name } and { $others } others are here

d-uvwnwa4d = Agent chip
    .w-new = { $name }, { $unseen } new
    .w-show-who-is-here = Show who is here
    .w-stop = Stop

d-xgvy7oje = People
    .w-who-is-here = Who is here
    .w-people = People
    .w-close = Close

d-vb456dpi = Roster
    .w-you = You
    .w-nobody-else-is-here = Nobody else is here
    .w-invite-someone-to-this-workspace-or = Invite someone to this workspace, or open the same one on another device
    .w-invite-someone = Invite someone

d-vyjuo52r = Person row
    .w-someone = Someone
    .w-follows-you = Follows you
    .w-in-a-call = In a call

d-lkpaptdc = Person card
    .w-stop-following = Stop following
    .w-follow = Follow
    .w-message = Message
    .w-wave = Wave
    .w-go-there = Go there
    .w-in-a-call = In a call: { $call }

d-nw3m2rzj = Card head
    .w-someone = Someone
    .w-follows-you = Follows you

d-sfputxmu = Own card
    .w-you = You
    .w-your-name = Your name
    .w-what-you-are-doing = What you are doing
    .w-add-a-device = Add a device
    .w-your-devices = Your devices

d-rjk6bai6 = Back to everyone
    .w-back-to-everyone = Back to everyone

d-lmr4ktas = Follow chip
    .w-following = Following { $name }
    .w-stop = Stop

d-hkigcrhq = Sharing indicator
    .w-sharing-1-window = Sharing 1 window
    .w-sharing-windows = Sharing { $windows } windows
    .w-stop-sharing = Stop sharing { $shown }
    .w-stop-sharing-2 = Stop sharing

d-5mioahup = Wave notice
    .w-waved-at-you = { $name } waved at you

d-6g5ux35b = Arrival notice
    .w-is-here = { $short } is here
    .w-and-1-other-are-here = { $short } and 1 other are here
    .w-and-others-are-here = { $short } and { $others } others are here

d-5xj23ufk = Ask card
    .w-not-now = Not now

d-agpwsfhm = Phone
    .w-sign-in-to-tilo = Sign in to Tilo
    .w-on-a-device-that-has-your = On a device that has your identity, run tilo device add and type the code it prints here. If this is your first device, start new.
    .w-not-signed-in = Not signed in
    .w-tilo1 = tilo1…
    .w-sign-in = Sign in
    .w-start-new = Start new
    .w-workspaces = Workspaces
    .w-no-workspaces-here-yet = No workspaces here yet
    .w-back = Back
    .w-apps = Apps
    .w-no-apps-yet = No apps yet

d-teccuwja = Quick settings

d-iworlvwt = Quick settings panel
    .w-quick-settings = Quick settings

d-dd3gpi64 = Control tile

d-vzdj2kff = Rotation tile
    .w-rotation-lock = Rotation lock

d-yxl6yq2r = Wifi tile
    .w-wi-fi = Wi-Fi
    .w-this-device-has-no-wi-fi = This device has no Wi-Fi
    .w-a-switch-on-this-device-holds = A switch on this device holds Wi-Fi off

d-d5wzqn37 = Bluetooth tile
    .w-bluetooth = Bluetooth
    .w-this-device-has-no-bluetooth = This device has no Bluetooth

d-6va3umd4 = Vpn tile
    .w-vpn = VPN

d-uzi5ah2q = Dark tile
    .w-dark-mode = Dark mode

d-s66oqjwe = Quiet tile
    .w-do-not-disturb = Do not disturb

d-fabhsxf6 = Night light tile
    .w-night-light = Night light

d-vr3zx6ry = Airplane tile
    .w-airplane-mode = Airplane mode

d-72cehcnn = Mobile tile
    .w-mobile-data = Mobile data

d-ntsyqtrw = Power mode tile
    .w-power-saver = Power saver
    .w-balanced = Balanced
    .w-performance = Performance

d-ij74wfyv = Clipboard section
    .w-clipboard = Clipboard
    .w-search-copies = Search copies
    .w-1-of-copies = 1 of { $len } copies
    .w-of-copies = { $len } of { $len2 } copies
    .w-stop-keeping = Stop keeping { $copy_label }
    .w-keep = Keep { $copy_label }
    .w-remove = Remove { $copy_label }
    .w-no-copy-matches = No copy matches
    .w-clear-the-rest = Clear the rest

d-w3fe7tjv = Levels
    .w-unmute = Unmute
    .w-mute = Mute
    .w-volume = Volume

d-wvxjuiww = Sound section
    .w-sound = Sound
    .w-unmute-the-microphone = Unmute the microphone
    .w-mute-the-microphone = Mute the microphone
    .w-microphone = Microphone
    .w-playing-here = Playing here
    .w-playing = Playing
    .w-paused = Paused
    .w-unmute = Unmute { $application }
    .w-mute = Mute { $application }
    .w-play-through = Play { $application } through
    .w-recording-here = Recording here

d-thduakjg = Network section
    .w-wi-fi = Wi-Fi
    .w-wired-network = Wired network
    .w-connected = Connected
    .w-cable-unplugged = Cable unplugged
    .w-a-switch-on-this-device-holds = A switch on this device holds Wi-Fi off
    .w-wi-fi-is-off = Wi-Fi is off
    .w-scanning-for-networks = Scanning for networks
    .w-known = Known
    .w-secured = Secured
    .w-open = Open
    .w-enter-the-password-for = Enter the password for { $password_for }
    .w-password = Password
    .w-join = Join
    .w-cancel = Cancel
    .w-hidden-network = Hidden network
    .w-join-a-network-that-does-not = Join a network that does not broadcast its name
    .w-network-name = Network name
    .w-saved-networks = Saved networks
    .w-joins-by-itself = Joins by itself
    .w-saved = Saved
    .w-this-network = This network
    .w-stop-joining-by-itself = Stop joining by itself
    .w-join-by-itself = Join by itself
    .w-forget-this-network = Forget this network

d-zjz5sskl = Bluetooth section
    .w-bluetooth = Bluetooth
    .w-bluetooth-is-off = Bluetooth is off
    .w-connected = Connected
    .w-paired = Paired
    .w-forget-this-device = Forget this device
    .w-stop-looking = Stop looking
    .w-look-for-devices = Look for devices
    .w-in-reach = In reach
    .w-no-devices-found = No devices found

d-lcjohgdk = Battery line

d-ycck5awo = Power row
    .w-lock = Lock
    .w-settings = Settings
    .w-power = Power

d-u2tma44r = Devices section
    .w-devices = Devices
    .w-play-here = Play { $media } here?
    .w-asks = { $source } asks
    .w-allow = Allow
    .w-decline = Decline
    .w-dismiss = Dismiss
    .w-stop = Stop
    .w-sounds-you-can-send = Sounds you can send
    .w-one-app-s-sound = One app's sound
    .w-everything-this-device-plays = Everything this device plays
    .w-drive-with-this-pointer = Drive { $label } with this pointer
    .w-nearby-send = Nearby Send
    .w-offers-this-keep-it-in = { $peer } offers this; keep it in { $name }
    .w-offers-this = { $peer } offers this
    .w-keep-it = Keep it
    .w-no-folder-here-takes-the-file = No folder here takes the file
    .w-turn-down = Turn down
    .w-it-did-not-finish = It did not finish
    .w-try-again = Try again
    .w-kept-here = Kept here
    .w-sent = Sent
    .w-turned-down = Turned down
    .w-no-devices-in-reach = No devices in reach

d-niotqkfd = Bluetooth hand off
    .w-move-it-to = Move it to…

d-2abiywd2 = Controls editor
    .w-customise-the-panel = Customise the panel
    .w-panel-controls = Panel controls
    .w-move-to-the-front = Move { $control_label } to the front
    .w-hide = Hide { $control_label }
    .w-not-on-the-panel = Not on the panel
    .w-show = Show { $control_label }
    .w-reset-the-panel = Reset the panel

d-6rjhuenw = Emoji card

d-wdiwq4yt = About sheet

d-tkt6f7u6 = About card
    .w-updates = Updates
    .w-close = Close
    .w-all-details = All details

d-vv7k7lcl = Peek card

d-mnufvdts = Power sheet
    .w-power = Power
    .w-lock = Lock
    .w-log-out = Log out
    .w-switch-user = Switch user
    .w-sleep = Sleep
    .w-restart = Restart
    .w-shut-down = Shut down

d-kvfaa7ju = Pass notice
    .w-dismiss = Dismiss

d-zzd3yipu = Strips

d-tj4z2obq = Frames
    .w-canvas = Canvas

d-tqbdxbtn = Frameless
    .w-frameless = Frameless

d-pbhumuny = Desktop

d-gb3g4lte = Dock ground
    .w-dock = Dock

d-afjfzjl6 = Bar on edge
    .w-top-bar = Top bar

d-su7ogm3c = Top bar

d-o5w6ulql = Banners over

d-6a3axsap = Surface

d-xzb2gybw = Style switch

d-kssxjs7v = Style menu
    .w-showing = Showing { $style_title }
    .w-settings-for = Settings for { $style_title }

d-qrahog47 = Strips view
    .w-no-window-here-yet = No window here yet

d-47ahldi3 = Start cards
    .w-start-something = Start something
    .w-only-you-see-what-you-make = Only you see what you make here until you invite someone.
    .w-notes = Notes
    .w-write-ideas-down = Write ideas down
    .w-tasks = Tasks
    .w-write-down-what-needs-doing = Write down what needs doing
    .w-invite-people = Invite people
    .w-work-on-this-with-someone = Work on this with someone
    .w-catch-up = Catch up
    .w-see-what-changed-and-why = See what changed and why
    .w-arrows = Arrows
    .w-move = Move
    .w-enter = Enter
    .w-open = Open
    .w-super = Super
    .w-n = N
    .w-every-app = Every app
    .w-open-an-app = Open an app

d-4qeo26jr = Workspace rows

d-54b5rqo4 = Rows of the shell
    .w-no-window-here-yet = No window here yet

d-7sxqbtn2 = Row window

d-2fofsa7f = Strip window
    .w-nothing-here = Nothing here

d-p5wzoqr5 = Window controls
    .w-restore = Restore
    .w-maximize = Maximize
    .w-close = Close

d-7pn66ekb = Author chip

d-hl7f3yqb = Window bar parts
    .w-back-in-this-window = Back in this window
    .w-forward-in-this-window = Forward in this window
    .w-sidebar = Sidebar
    .w-more-actions-of-this-window = More actions of this window

d-xqbtyqvn = Bar button

d-kdh5crgd = Bar item

d-gtvxl4ug = Bar rename
    .w-window-name = Window name

d-j5z7brtk = Bar location
    .w-where-the-window-goes = Where the window goes

d-xfexkr2v = Bar search
    .w-search-this-window-and-replace = Search this window and replace
    .w-search-this-window = Search this window
    .w-previous-match = Previous match
    .w-next-match = Next match
    .w-close-the-search = Close the search

d-ktt65xag = Share control
    .w-shared-with-the-workspace = Shared with the workspace
    .w-shared-watching = Shared, { $watching } watching
    .w-stop-sharing = Stop sharing
    .w-stop-letting-them-control-it = Stop letting them control it
    .w-let-them-control-it = Let them control it
    .w-share-this-window = Share this window

d-6zyejo5v = Window bar actions
    .w-new-tab-of-this-window = New tab of this window
    .w-search-this-window = Search this window
    .w-history-of-this-window = History of this window
    .w-back-in-this-window = Back in this window
    .w-forward-in-this-window = Forward in this window

d-z4vc77e6 = Place controls
    .w-this-window = This window
    .w-pin-place = Pin place
    .w-close = Close

d-oyjbbzbk = Place content

d-43am22do = Place view

d-fhub73iw = Place body

d-6t5hb2z7 = Query view
    .w-retry = Retry
    .w-nothing-matched = Nothing matched { $text }

d-vrlwh4vn = This window menu
    .w-this-window = This window
    .w-reveal = Reveal
    .w-it-is-in-front-already = It is in front already
    .w-exit-full-screen = Exit full screen
    .w-full-screen = Full screen
    .w-restore = Restore
    .w-fill-the-strip = Fill the strip
    .w-snap-back = Snap back
    .w-float-this-window = Float this window
    .w-half-the-strip = Half the strip
    .w-narrow = Narrow
    .w-normal = Normal
    .w-wide = Wide
    .w-every-window-the-same = Every window the same
    .w-wider = Wider
    .w-as-wide-as-it-gets = As wide as it gets
    .w-narrower = Narrower
    .w-as-narrow-as-it-gets = As narrow as it gets
    .w-move-left = Move left
    .w-move-right = Move right
    .w-move-into-as-a-tab = Move into { $menu_strip_name } as a tab
    .w-move-to = Move to { $name }
    .w-display = Display { $number }
    .w-move-to-a-new-strip = Move to a new strip
    .w-new-tab-of-this-app = New tab of this app
    .w-rename = Rename
    .w-split-right = Split right
    .w-new-group = New group
    .w-unpin-window = Unpin window
    .w-pin-window = Pin window
    .w-stop-controlling-the-desktop = Stop controlling the desktop
    .w-close-tab = Close tab
    .w-close-this-window = Close this window

d-c6mbdwcc = Strip rail
    .w-of = { $number } of { $len }
    .w-new-strip = New strip

d-2nputj5f = Show style settings~present

d-htrvh33d = Style panel
    .w-settings-for = Settings for { $style_title }
    .w-close = Close

d-gncm64rz = Style sheet
    .w-settings-for = Settings for { $style_title }
    .w-close = Close

d-mmqndkio = Style settings body
    .w-comes-from-and-its-settings-are = { $style_label } comes from { $package }, and its settings are its package's own.

d-jdungghl = Theme settings
    .w-look = Look
    .w-dark = Dark
    .w-light = Light
    .w-translucency = Translucency
    .w-accent-follow-the-theme = Accent: follow the theme
    .w-window-corners = Window corners
    .w-refraction = Refraction

d-tcyusdmx = Desktop style settings
    .w-show-the-top-bar = Show the top bar
    .w-show-the-dock = Show the dock
    .w-dock-edge = Dock edge
    .w-dock-size = Dock size
    .w-when-the-dock-shows = When the dock shows

d-naeq6y2p = Strips style settings
    .w-show-the-tabs-of-a-strip = Show the tabs of a strip

d-xa2ijkev = Frames style settings
    .w-snap-places-to-a-grid = Snap places to a grid

d-uatqbuz2 = Frameless style settings
    .w-frameless-shows-each-place-as-its = Frameless shows each place as its own view and has nothing to set.

d-fziuz57n = Switcher
    .w-of = { $at } of { $count }
    .w-switch-windows-of-this-app = Switch windows of this app
    .w-switch-windows = Switch windows
    .w-this-app-s-windows-in-this = This app's windows in this workspace, most recently used first. Hold Alt and press ` to move, release Alt to switch, or press Escape to stay.
    .w-windows-in-this-workspace-most-recently = Windows in this workspace, most recently used first. Hold Alt and press Tab or the arrow keys to move, release Alt to switch, or press Escape to stay.
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
    .w-this-window = This window
    .w-close = Close

d-u55srubm = Action bar

d-cijhlzxf = Window commands
    .w-close = Close

d-yx7doxgr = Traditional bar
    .w-tilo = Tilo

d-6l3wldtm = Window menu

d-65ugxki4 = Window menu card

d-vhbxgivv = Window menu row

d-wdjxaekq = Workspaces
    .w-workspaces = Workspaces
    .w-name-a-new-workspace = Name a new workspace
    .w-create = Create
    .w-join-a-workspace = Join a workspace

d-7nsn4srj = Workspace card
    .w-open = Open
    .w-here = { $len } here
