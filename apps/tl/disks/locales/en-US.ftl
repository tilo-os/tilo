# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-disks = Disks

d-so-tilo-disks-keywords = disks, drives, partitions, format, mount, smart, laufwerke, festplatten

d-nnkcw24e = Disks
    .doc = The names a person gives their drives, by serial number: the workspace's, so a name follows the drive to every device it is plugged into.
    .noun = { $count ->
        [one] disk
       *[other] disks
    }

d-6mf2skfm = Names

d-pezu5gyc = Name drive
    .doc = A drive's name in the words of the person reading it, by its serial.

d-pezu5gyc-serial = Serial

d-pezu5gyc-name = Name

d-hoqau5lk = Mount first
    .doc = Mounts the drive's first partition, which is what a drive without a filesystem of its own holds.

d-hoqau5lk-drive = Drive

d-hagdje6k = Format device
    .doc = Erases the device the confirmation named, and writes the chosen filesystem.

d-hagdje6k-device_name = Device name

d-hagdje6k-filesystem = Filesystem

d-3cndvboj = Size words
    .w-b = { $bytes } B
    .w-kb = { $value } kB
    .w-mb = { $value } MB
    .w-gb = { $value } GB
    .w-tb = { $value } TB

d-mtgfincv = Rate words
    .w-kb-s = { $to_int } kB/s
    .w-mb-s = { $to_int } MB/s
    .w-gb-s = { $to_int } GB/s

d-q7p23wal = Health words
    .w-predicted-to-fail = Predicted to fail
    .w-healthy = Healthy
    .w-its-health-is-not-reported = Its health is not reported

d-s5r7sgxu = Partition words
    .w-no-filesystem = no filesystem

d-iq7enjwf = Window
    .w-disks = Disks
    .w-drives = Drives
    .w-no-drives = No drives
    .w-read-the-drives-again = Read the drives again
    .w-drive = Drive
    .w-overview = Overview
    .w-health = Health
    .w-partitions = Partitions
    .w-no-partitions = No partitions
    .w-this-drive-holds-no-partition-table = This drive holds no partition table; open it as one filesystem or erase it.
    .w-open = Open
    .w-closed = Closed
    .w-close = Close
    .w-erase = Erase…
    .w-of = { $size_words } of { $size_words2 }
    .w-how-it-is-divided = How it is divided
    .w-unallocated = Unallocated
    .w-the-drive-read = The drive read { $rate_words }
    .w-random-reads-a-second = { $to_int } random reads a second
    .w-open-at = Open at { $mounted }
    .w-open-the-first-partition = Open the first partition
    .w-measure = Measure
    .w-eject = Eject
    .w-choose-a-drive = Choose a drive
    .w-pick-a-drive-on-the-left = Pick a drive on the left to see its map, its health and its partitions.
    .w-erase-2 = Erase { $device_name }?
    .w-everything-on-it-is-lost-and = Everything on it is lost, and cannot be brought back.
    .w-erase-3 = Erase
    .w-cancel = Cancel
    .w-the-drive-is-erased-and-a = The drive is erased and a new filesystem written on it.

d-qlslseop = Shape
    .w-open = open

d-ta2u27ch = Health pane
    .w-health = Health
    .w-verdict = Verdict
    .w-warnings = Warnings
    .w-temperature = Temperature
    .w-c = { $temperature } °C
    .w-powered-on = Powered on
    .w-hours = { $hours } hours
    .w-power-cycles = Power cycles
    .w-life-used = Life used
    .w-spare-capacity = Spare capacity
    .w-written = Written
    .w-read = Read
    .w-media-errors = Media errors
    .w-bad-sectors = Bad sectors
    .w-last-self-test = Last self-test
    .w-its-health-is-not-reported = Its health is not reported
    .w-this-drive-reports-no-health-of = This drive reports no health of its own.
