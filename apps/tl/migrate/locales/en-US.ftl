# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-migrate = Migrations

d-so-tilo-migrate-keywords = migrate, import, move, bring in, thunderbird, browsers, cloud storage

d-fosjwamu = Item
    .doc = Something the review found in the source: an account, a folder, an address book, a calendar.
    .noun = { $count ->
        [one] item
       *[other] items
    }

d-z7gblqe6 = Key
    .doc = What the item is in the source, as the engine names it, unique within the migration.

d-fvmu6lru = Kind
    .doc = What sort of thing it is: "account", "folder", "address book".

d-ove3mkdj = Title

d-7b7r6oxc = Count
    .doc = How many things it holds, counted as `counts` says.

d-kz76r36d = Counts
    .doc = The plural noun of what `count` counts: "messages", "contacts".

d-qubfwmfb = Within
    .doc = The key of the item it sits under; empty for one at the top.

d-ghq4i3f7 = Chosen
    .doc = Whether it is to be brought in.

d-2ogrf3gp = Note
    .doc = What the review has to say about it: "Shared with you, read only".

d-fosjwamu-filter-count = Count

d-fosjwamu-filter-chosen = Chosen

d-7fp3ivdj = Count
    .doc = How many things of one kind the last run brought in: "contacts" 120.
    .noun = { $count ->
        [one] count
       *[other] counts
    }

d-3cttshux = Kind

d-imoqqond = Count

d-7fp3ivdj-filter-count = Count

d-p7c5y2uc = Run
    .doc = One run of a migration and the keys of what it brought in, so undoing it forgets exactly those.
    .noun = { $count ->
        [one] run
       *[other] runs
    }

d-wphp5gxk = Id
    .doc = The id of the run: the invocation of the engine's `start` that made it.

d-e73pil4f = Title

d-ifezl4lz = Keys

d-p7c5y2uc-filter-keys = Keys

d-rj2svje3 = Migration
    .doc = Bringing what one source holds into the workspace: a computer's home folders, its browsers, a Thunderbird profile, a mail account, a cloud drive, a service's pages or issues.
    .noun = { $count ->
        [one] migration
       *[other] migrations
    }

d-jy4y5q35 = Source
    .doc = The engine's key for the source: home, browsers, thunderbird, mail, gdrive, onedrive, dropbox, icloud, nextcloud, webdav, outline, linear, forgejo.

d-d66kqgv4 = Title
    .doc = What the source is, as a person reads it.

d-6muhvdtw = Device
    .doc = The key of the device that holds the data and runs the source; empty for a source any device can run.

d-kxg2fplq = Handle
    .doc = The profile, account or home the review read.

d-5nrd34wr = Items

d-dwktqelj = Unreadable
    .doc = What the review could not read, each in a sentence.

d-cbtpcpsr = State

d-4tzslsk2 = Run
    .doc = The id of the current or last run.

d-c6gmooon = Run title

d-v6notall = Failure

d-6tvxvli7 = Refused
    .doc = Why the device refused the last step asked of the migration; empty once a step goes through.

d-wtqlrste = Brought
    .doc = How many of each kind the last run brought in.

d-hl5hwgex = Already there
    .doc = How many things the last run found already in the workspace.

d-g44f5aep = Landed
    .doc = The keys of the items from this source already in the workspace.

d-45kagnwx = Runs
    .doc = What each run brought in.

d-rj2svje3-filter-unreadable = Unreadable

d-rj2svje3-filter-state = State

d-rj2svje3-filter-already_there = Already there

d-rj2svje3-filter-landed = Landed

d-uqkltwh2 = Migrations
    .doc = The workspace's migrations.
    .noun = { $count ->
        [one] migration
       *[other] migrations
    }

d-l7nahbny = Migrations

d-dgwyov6w = Run state
    .doc = Where a migration's run stands: reviewed, with what the source holds waiting to be chosen; running, while the device that holds the data brings in what was chosen; stopped by a person, keeping what it brought until then; failed, its failure saying why; or complete.

d-wi42igkx = Reviewed

d-yc72xedi = Running

d-7lcij7kb = Stopped

d-3ozfhjvz = Failed

d-7pmvqxs4 = Complete

d-znhlkiro = Reviewed
    .doc = The device that reviewed `source` lists it: the migration for that source on that device, made the first time, with its title and the handle the review read.
    .did = reviewed a source

d-znhlkiro-source = Source

d-znhlkiro-title = Title

d-znhlkiro-device = Device

d-znhlkiro-handle = Handle

d-7mgub726 = Review item
    .doc = The review found the item `key`: it is kept as found, and one found before keeps whether a person chose it; a new one is chosen as `chosen` says.

d-7mgub726-key = Key

d-7mgub726-kind = Kind

d-7mgub726-title = Title

d-7mgub726-count = Count

d-7mgub726-counts = Counts

d-7mgub726-within = Within

d-7mgub726-note = Note

d-7mgub726-chosen = Chosen

d-2e2qnaw6 = Forget items
    .doc = A fresh review no longer found the items whose keys are not in `keep`: they go.

d-2e2qnaw6-keep = Keep

d-54ygmmcv = Unreadable
    .doc = The review could not read `what`.

d-54ygmmcv-what = What

d-vnfd46in = Bring in
    .doc = Chooses an item of `g` to be brought in, or leaves it out: the device that holds the data keeps the choice, and the migration follows it there, as `choose_keys`.

d-vnfd46in-i = I

d-vnfd46in-chosen = Chosen

d-453xvewh = Choose keys
    .doc = The device chose the items whose keys are `keys`, or left them out; with `only`, every other item is left out.

d-453xvewh-keys = Keys

d-453xvewh-chosen = Chosen

d-453xvewh-only = Only

d-6owc7q4m = Begun
    .doc = The device began the run `run`, or resumed it: it is running.

d-6owc7q4m-run = Run

d-6owc7q4m-title = Title

d-sm5273v2 = Landed
    .doc = The run `run` brought in the items `keys`: `count` things of `kind`, and found `already` of them in the workspace before.

d-sm5273v2-run = Run

d-sm5273v2-keys = Keys

d-sm5273v2-kind = Kind

d-sm5273v2-count = Count

d-sm5273v2-already = Already

d-mzjsihxi = Ended
    .doc = The run ended as `state`: stopped, failed with `failure`, or complete.
    .w-a-run-ends-stopped-failed-or = A run ends stopped, failed or complete

d-mzjsihxi-state = State

d-mzjsihxi-failure = Failure

d-skk653vz = Forgot run
    .doc = The device took out of the workspace what the run `run` brought in: its keys are no longer landed and the run is gone.

d-skk653vz-run = Run

d-suc576de = Refused
    .doc = The device refused a step asked of `g`, for `why`: nothing chosen to bring in, a source it cannot read, a run it cannot take back.

d-suc576de-why = Why

d-mc6o2yv4 = Look again
    .doc = Read `g`'s source again: what it holds now is what there is to choose from.
    .did = looked at a source again

d-z7l36mcv = Start
    .doc = Bring in what is chosen of `g`'s source: the device that holds the data runs it as one run, and the migration follows it there, as `begun`, `landed` and `ended`.
    .did = started bringing in

d-b6a4nblx = Stop
    .doc = Stop `g`'s run after the item it is on: what it brought stays, and starting again resumes it.
    .did = stopped bringing in

d-x6sqvfbn = Undo
    .doc = Take what the run `r` brought in back out of the workspace.
    .did = took a run back

d-x6sqvfbn-r = R

d-rzpx6bzh = Ended~present

d-vzbtgrfk = Migrations
    .w-nothing-is-being-brought-in = Nothing is being brought in

d-gd45slyw = Migration row
    .w-brought-in = { $brought } brought in

d-m6xdvzmk = Migration page
    .w-could-not-be-read = Could not be read
    .w-what-to-bring-in = What to bring in
    .w-the-review-found-nothing-to-bring = The review found nothing to bring in
    .w-runs = Runs

d-zwcqzi7i = Run status
    .w-bringing-in = Bringing in
    .w-stop = Stop
    .w-resume = Resume
    .w-try-again = Try again
    .w-bring-in = Bring in
    .w-look-again = Look again
    .w-the-run-stopped-on-an-error = The run stopped on an error
    .w-that-could-not-be-done = That could not be done
    .w-already-here = { $already_there } already here

d-toxseay6 = Item row

d-htm3bmqz = Run row
    .w-items = { $len } items
    .w-undo = Undo
