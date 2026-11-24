# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-backups = Backups

d-so-tilo-backups-keywords = backup, backups, snapshot, restore, retention, archive, datensicherung, sicherung, wiederherstellen

d-lcjavy23 = Entry
    .doc = One entry of a snapshot's file index, as the restore preview shows it.
    .noun = { $count ->
        [one] entry
       *[other] entries
    }

d-cvlieest = Name

d-c2m5wzf4 = Depth
    .doc = How deep in the snapshot it sits.

d-pfl5jyx2 = Size
    .doc = Its size in bytes; zero for a folder.

d-tzyurmps = Folder

d-lcjavy23-filter-depth = Depth

d-lcjavy23-filter-size = Size

d-lcjavy23-filter-folder = Folder

d-euqtsjtw = Snapshot
    .doc = One stored snapshot of the folder this device protects.
    .noun = { $count ->
        [one] snapshot
       *[other] snapshots
    }
    .untitled = Snapshot

d-dallfwqf = Label
    .doc = The name a person gave it, when they gave one.

d-ngqbekfc = At
    .doc = The second it was taken, counted from 1970.

d-hydekdnu = Taken

d-yffctwuv = Kept

d-6kvlgzq4 = Health

d-ljb7imfg = Size bytes

d-vcgdjszb = Delta bytes

d-yr6zgb2x = Files changed

d-cyesgrji = Failure
    .doc = Why it failed, when it did.

d-5nvgiivd = Files

d-wzd6czrl = Policy
    .noun = { $count ->
        [one] policy
       *[other] policies
    }

d-cyugw7lh = Hourly
    .doc = Snapshots kept of each class.

d-avot6p6r = Daily

d-ywba2jzd = Weekly

d-3miz6dsu = Monthly

d-wzd6czrl-filter-hourly = Hourly

d-wzd6czrl-filter-daily = Daily

d-wzd6czrl-filter-weekly = Weekly

d-wzd6czrl-filter-monthly = Monthly

d-v7z4w6ml = Place
    .doc = The folder a person picked in the picker, before it becomes the backup's folder.
    .noun = { $count ->
        [one] place
       *[other] places
    }

d-dho3qqg3 = Name

d-f3pl3mol = Path

d-igeecx5d = Backups
    .doc = The workspace's Backups app: the folder this device protects, where its snapshots go, the snapshots it holds, and the retention policy that prunes them.
    .noun = { $count ->
        [one] backup
       *[other] backups
    }
    .untitled = Backups

d-p2imk5l5 = Policy
    .doc = How many snapshots of each class a backup keeps.

d-srrmxp5i = Source
    .doc = The folder this device protects; empty until a person picks one.

d-6e5yqotr = Destination
    .doc = The folder snapshots are written to; empty until a person picks one.

d-62wijgsn = Source name
    .doc = The name the source folder goes by.

d-x6ca2mpx = Source origin

d-gfsjtev7 = Source cadence

d-3wbouwis = Source failing

d-v3i3cihq = Destination name
    .doc = The name the destination folder goes by, and what it holds.

d-kqbve5pf = Used bytes

d-mdbssite = Total bytes
    .doc = The drive's capacity; zero when it does not say.

d-kz5xznku = Encrypted

d-hlhhrwm3 = Destination failing

d-n7gmur3t = Snapshots
    .doc = The stored snapshots, newest first.

d-ik3j63rv = Place
    .doc = The place the sidebar shows: `all`, `source`, `destination` or `retention`.

d-6fg5mkqa = Newest first
    .doc = Whether the timeline lists the newest first.

d-sxzlbnam = Trouble
    .doc = The look at the repository went wrong, in the device's words.

d-yelwker2 = Failure
    .doc = The work failed or was refused, in the device's words; a look does not clear it.

d-6rkekawk = Notice
    .doc = One line under the list: what was done or stopped.

d-bluheyea = Job
    .doc = The work under way on this device, by the job the device gave it.

d-wmrb4vzi = Working
    .doc = Which work the job is: `backup`, `verify` or `restore`.

d-v7lhpobw = Progress
    .doc = How far the work has got, in files and bytes.

d-xsai3dea = Result
    .doc = What a verification or a restore found, once it finished.

d-fihmdmmi = Subject
    .doc = The snapshot being verified or restored, by its second.

d-q6dvqz3z = Picking
    .doc = Which folder the person is picking: `source`, `destination` or `restore`.

d-vlgd4tne = Picker
    .doc = The folder the picker walks, and the folders in it.

d-6nds5bnc = Picks

d-5dqejapv = Restore into
    .doc = The folder a restore writes into, and whether it may replace what is there.

d-azcuku5y = Overwrite

d-igeecx5d-filter-policy = Policy

d-igeecx5d-filter-source_origin = Source origin

d-igeecx5d-filter-source_cadence = Source cadence

d-igeecx5d-filter-source_failing = Source failing

d-igeecx5d-filter-used_bytes = Used bytes

d-igeecx5d-filter-total_bytes = Total bytes

d-igeecx5d-filter-encrypted = Encrypted

d-igeecx5d-filter-destination_failing = Destination failing

d-igeecx5d-filter-newest_first = Newest first

d-igeecx5d-filter-subject = Subject

d-igeecx5d-filter-overwrite = Overwrite

d-v6ydjahg = Catalog ask
    .noun = { $count ->
        [one] catalog ask
       *[other] catalog asks
    }

d-gbluxeo3 = Source

d-ixnr6cnx = Destination

d-zzjhdxbx = Backup ask
    .noun = { $count ->
        [one] backup ask
       *[other] backup asks
    }

d-w7elqfbk = Source

d-ebybhxkw = Destination

d-qqrrtmrs = Policy hourly

d-qrbyjmwo = Policy daily

d-jd3hqbyz = Policy weekly

d-6z7komoa = Policy monthly

d-zzjhdxbx-filter-policy_hourly = Policy hourly

d-zzjhdxbx-filter-policy_daily = Policy daily

d-zzjhdxbx-filter-policy_weekly = Policy weekly

d-zzjhdxbx-filter-policy_monthly = Policy monthly

d-kjitlism = Snapshot ask
    .noun = { $count ->
        [one] snapshot ask
       *[other] snapshot asks
    }

d-2stygd3d = Destination

d-vfb2hw7j = Snapshot

d-3pulbfdz = Restore ask
    .noun = { $count ->
        [one] restore ask
       *[other] restore asks
    }

d-z2pcuhft = Destination

d-qz4acm4a = Snapshot

d-3hxnk3mo = Target

d-lbwhdcbx = Overwrite

d-3pulbfdz-filter-overwrite = Overwrite

d-66xmnr3b = Work state
    .noun = { $count ->
        [one] work state
       *[other] work states
    }

d-ptww3noe = State

d-pnevrulc = Progress

d-lrfiygoo = Answer

d-apwu74vg = Error

d-znoewa74 = Checked
    .noun = { $count ->
        [one] checked
       *[other] checkeds
    }

d-lg6ud7lc = Files

d-deudvrko = Bytes

d-znoewa74-filter-files = Files

d-znoewa74-filter-bytes = Bytes

d-tfxlhxcp = Taken
    .doc = Was a snapshot taken by the schedule or by hand?

d-ijjeki2d = Automatic

d-yu3bengf = Manual

d-dczpbrdi = Unknown

d-4nziw5wl = Kept
    .doc = The retention class a snapshot counts in.

d-guchm5y3 = Hourly

d-7ehlxq37 = Daily

d-dbtfb3es = Weekly

d-gdrmnn2g = Monthly

d-ib2ogkjg = Kept

d-5ew2vbah = Unknown

d-nyssh3ek = Health
    .doc = Whether a stored snapshot is whole, being checked or known broken.

d-z5ipmi4c = Complete

d-d57dpcpd = Verifying

d-cdszgdvi = Failed

d-y7yrr6jo = Origin
    .doc = What a source is, as much as its path says without naming it.

d-2owudtrn = Home

d-cmidvqwr = System

d-wzgcvto4 = Workspace

d-z6vj6vhj = Custom

d-qxqolkzh = Cadence
    .doc = When a source backs itself up.

d-wpyvyoau = On demand

d-67d5hncl = Hourly

d-jjyafzb3 = Daily

d-fzew3dgc = Weekly

d-3fzr3jov = Trouble
    .doc = Why the last look went wrong.

d-d2scemnx = None

d-3ahku3ip = Unreadable

d-xazatgpx = Missing

d-67y2enqb = Look again
    .doc = Looks at the repository again: the source, the destination and the snapshots it holds.

d-rvp6xxbz = Keep catalog
    .doc = Keeps what the device answered about its repository, so the window and an agent read the same snapshots.

d-rvp6xxbz-answer = Answer

d-ekhujpxs = Back up now
    .doc = Backs the folder up now, keeping what the retention policy keeps.
    .w-choose-a-folder-to-protect-first = Choose a folder to protect first
    .w-choose-where-the-snapshots-go-first = Choose where the snapshots go first
    .w-one-piece-of-work-is-running = One piece of work is running already

d-fmzgim3f = Verify the snapshot
    .doc = Checks that every file of a snapshot is still what the snapshot recorded.
    .w-one-piece-of-work-is-running = One piece of work is running already

d-fmzgim3f-s = S

d-ttl3xkqx = Restore the snapshot
    .doc = Opens the sheet that asks where a snapshot goes back to.
    .w-one-piece-of-work-is-running = One piece of work is running already

d-ttl3xkqx-s = S

d-4v3uefx5 = Begin restore
    .doc = Starts finding the folder a restore writes into, without running anything yet.

d-v5ylljvo = Restore
    .doc = Restores the snapshot into the folder the person chose.
    .w-choose-a-folder-to-restore-into = Choose a folder to restore into
    .w-one-piece-of-work-is-running = One piece of work is running already

d-v5ylljvo-s = S

d-pz4zxkew = Stop the work
    .doc = Stops the work under way; what it was writing goes.

d-eq6dkezf = Poll
    .doc = Asks the device how the work is doing, once a second while it runs.

d-nrshrfas = Open snapshot
    .doc = Shows one snapshot's sheet: what it holds and what can be done with it.
    .did = opened a snapshot

d-nrshrfas-s = S

d-5gdsin6x = Order
    .doc = Lists the timeline newest first, or oldest first.

d-5gdsin6x-newest_first = Newest first

d-w7nn64aa = Show the place
    .doc = Shows one of the sidebar's places.

d-w7nn64aa-place = Place

d-vap65q73 = Keep one more
    .doc = Keeps one more snapshot of a class.

d-vap65q73-which = Which

d-h6ethwbw = Keep one fewer
    .doc = Keeps one fewer snapshot of a class.

d-h6ethwbw-which = Which

d-x5h6br5g = Dismiss
    .doc = Dismisses the line under the list.

d-z6m2czb6 = Choose the folder to protect
    .doc = Asks the person which folder this device protects.

d-ioanf4us = Choose where snapshots go
    .doc = Asks the person where the snapshots go.

d-54fhu267 = Go
    .doc = Walks the picker to another folder and shows the folders in it.

d-54fhu267-path = Path

d-k5qqlbcc = Go up
    .doc = Walks the picker up one folder.

d-nj4ehqok = Use this folder
    .doc = Takes the folder the picker shows: as what this device protects, where snapshots go, or where a restore writes.

d-egcbexsk = Cancel
    .doc = Leaves the picker without changing anything.

d-5b35wzq6 = Read picker
    .doc = Reads the folders of the one the picker shows.

d-ifgic4vt = Start work
    .doc = Starts a piece of work and keeps the job the device gave it.

d-ifgic4vt-kind = Kind

d-ifgic4vt-ask = Ask

d-vq7l4nar = Close snapshot

d-apgumkkc = Allow overwrite

d-t5f6lf3u = Back up now~present

d-ngp7g7pa = Verify~present

d-s2kwexka = Ask restore~present

d-4vnqjv4m = Restore now~present

d-yeswuxpe = Window

d-5kspbgsu = Picker
    .w-choose-the-folder-to-protect = Choose the folder to protect
    .w-choose-where-snapshots-go = Choose where snapshots go
    .w-choose-the-folder-to-restore-into = Choose the folder to restore into
    .w-up = Up
    .w-restore-into-this-folder = Restore into this folder
    .w-use-this-folder = Use this folder
    .w-cancel = Cancel
    .w-that-folder-could-not-be-read = That folder could not be read
    .w-folders = Folders
    .w-no-folders-inside = No folders inside

d-xpn57dne = Setup
    .w-set-up-backups = Set up Backups
    .w-choose-a-folder-to-protect-and = Choose a folder to protect and where its snapshots go.
    .w-the-folder-to-protect = The folder to protect
    .w-nothing-chosen-yet = Nothing chosen yet
    .w-snapshots-copy-this-folder-as-it = Snapshots copy this folder as it is now.
    .w-choose-the-folder-to-protect = Choose the folder to protect
    .w-folder = Folder
    .w-called = Called
    .w-kind = Kind
    .w-choose-another-folder = Choose another folder
    .w-where-snapshots-go = Where snapshots go
    .w-each-snapshot-is-written-here-outside = Each snapshot is written here, outside the folder it copies.
    .w-choose-where-snapshots-go = Choose where snapshots go
    .w-used = Used
    .w-that-could-not-be-done = That could not be done
    .w-back-up-now = Back up now

d-pvbigiav = Shell
    .w-backups = Backups
    .w-all-backups = All backups
    .w-this-device = This device
    .w-where-they-go = Where they go
    .w-retention = Retention
    .w-snapshots = Snapshots
    .w-failed = Failed { $when }
    .w-stop = Stop
    .w-back-up-now = Back up now
    .w-look-again = Look again
    .w-work-under-way = Work under way
    .w-something-needs-a-look = Something needs a look
    .w-dismiss = Dismiss

d-sqdhwi4z = Working line
    .w-backing-up = Backing up
    .w-verifying-the-snapshot = Verifying the snapshot
    .w-restoring-the-snapshot = Restoring the snapshot
    .w-stop = Stop

d-anqvfopn = Timeline
    .w-last-backup = Last backup
    .w-snapshots = Snapshots
    .w-protected = Protected
    .w-need-a-look = Need a look
    .w-newest-first = Newest first
    .w-oldest-first = Oldest first
    .w-newest = Newest { $when }
    .w-no-snapshots-yet = No snapshots yet
    .w-back-up-the-folder-to-make = Back up the folder to make the first one.

d-xm44zffk = Snapshot row
    .w-changed = { $size_text }, { $files_text } changed

d-qrinzowj = Snapshot sheet
    .w-restore = Restore…
    .w-verify = Verify
    .w-taken = Taken
    .w-how = How
    .w-kept-as = Kept as
    .w-size = Size
    .w-changed = Changed
    .w-state = State
    .w-why-it-failed = Why it failed
    .w-files = Files
    .w-the-snapshot-holds-no-files = The snapshot holds no files
    .w-the-snapshot-s-files = The snapshot's files
    .w-restore-into = Restore into { $restore_into }
    .w-what-the-folder-holds-is-replaced = What the folder holds is replaced by the snapshot.
    .w-the-folder-must-be-new-or = The folder must be new or empty.
    .w-restore-here = Restore here
    .w-choose-another-folder = Choose another folder
    .w-choose-the-folder-to-restore-into = Choose the folder to restore into
    .w-replace-what-the-folder-holds = Replace what the folder holds

d-xs435fkb = File row

d-nel5mdzq = Source page
    .w-folder = Folder
    .w-called = Called
    .w-kind = Kind
    .w-schedule = Schedule
    .w-state = State
    .w-needs-a-look = Needs a look
    .w-fine = Fine
    .w-choose-another-folder = Choose another folder
    .w-its-snapshots = Its snapshots

d-6lykvoui = Destination page
    .w-folder = Folder
    .w-called = Called
    .w-used = Used
    .w-encrypted = Encrypted
    .w-yes = Yes
    .w-no = No
    .w-state = State
    .w-needs-a-look = Needs a look
    .w-fine = Fine
    .w-choose-another-folder = Choose another folder
    .w-its-snapshots = Its snapshots

d-76vcytvk = Retention page
    .w-retention = Retention
    .w-a-backup-keeps-this-many-snapshots = A backup keeps this many snapshots of each class, and the newest of all.
    .w-kept = Kept
    .w-hourly = hourly
    .w-hourly-2 = Hourly
    .w-daily = daily
    .w-daily-2 = Daily
    .w-weekly = weekly
    .w-weekly-2 = Weekly
    .w-monthly = monthly
    .w-monthly-2 = Monthly
    .w-kept-at-most = { $policy_total } kept at most
    .w-the-newest-snapshot-always-stays = The newest snapshot always stays

d-lxppnx67 = Policy row
    .w-one-fewer = One fewer { $label }
    .w-one-more = One more { $label }
