## History

disks-name-drive = Named a drive “{ $name }”
disks-disks-names = Changed the name of a drive

disks-title = Disks
disks-partition-map = Partition map
disks-loading = Loading
disks-select-whole-disk = Select whole disk

## Window and accessibility copy
disks-drive-empty = This drive has no partitions.
disks-a11y-smart = SMART attributes
disks-none-mounted = No filesystems are mounted.
disks-a11y-drives = Drives

## Facts, property labels and section titles
disks-section-details = Details
disks-used-for = Used for
disks-free = Free
disks-used = Used
disks-mounted-at = Mounted at
disks-encryption = Encryption
disks-filesystem = Filesystem
disks-type = Type
disks-size = Size
disks-device = Device
disks-serial = Serial number
disks-interface = Interface
disks-capacity = Capacity
disks-section-filesystems = Filesystems
disks-partition-a11y = Select partition { $device }

disks-section-attributes = Attributes
disks-section-drive = Drive
disks-section-partition-map = Partition map

## What the window shows
disks-open-in-files = Open in Files
disks-space-left = How much space is left?
disks-swap = Swap space
disks-filesystem-at = { $filesystem } at { $path }
disks-in-use = In use
disks-not-in-use = Not in use
disks-purpose-root = Running system
disks-purpose-home = Home folders
disks-purpose-boot = Starts this device
disks-partition-encrypted = encrypted
disks-partition-encrypted-locked = encrypted and locked
disks-partition-free = { $room } free
disks-not-mounted = Not mounted
disks-encryption-unlocked = { $scheme }, unlocked
disks-encryption-locked = { $scheme }, locked

disks-severity-warning = Warning
disks-severity-critical = Critical
disks-severity-healthy = OK
disks-empty-drives-hint = Attach a drive, then rescan.

## What formatting a drive or a partition erases, in the question it asks first
disks-format-loses-drive = { $count ->
    [0] Everything on it is erased for good.
    [one] Its partition and every file on it are erased for good.
   *[other] Its { $count } partitions and every file on them are erased for good.
}
disks-format-loses-files = Every file on it is erased for good.
disks-format-loses-files-on = Every file on “{ $label }” is erased for good.
disks-format-loses-used = The { $size } of files on it are erased for good.
disks-format-loses-used-on = The { $size } of files on “{ $label }” are erased for good.
## The window bar's slots
disks-rescan-drives = Rescan drives
disks-views = View


## What a drive's operations are called, and what SMART says of it
disks-action-mount = Mount
disks-action-unmount = Unmount
disks-action-format = Format…
disks-action-benchmark = Benchmark
disks-action-eject = Eject
disks-health-healthy = Healthy
disks-health-attention = Needs attention
disks-health-failing = Failing
disks-health-unknown = Unknown

## Actions

disks-device-rescan = Rescan
disks-device-select = Select
disks-device-show-tab = Show tab
disks-device-format = Format
disks-device-disk-usage = Show disk usage
disks-system = Storage devices
disks-system-name-drive = Name drive
