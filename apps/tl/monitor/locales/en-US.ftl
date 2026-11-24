# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-monitor = System Monitor

d-so-tilo-monitor-keywords = monitor, system monitor, activity, processes, cpu, memory, task manager, ressourcen, prozesse

d-syvxuusz = Row
    .doc = One row of the process list: the process, how deep it stands under its parent, and whether it has children.
    .noun = { $count ->
        [one] row
       *[other] rows
    }

d-uoc4p4dx = Process

d-qwzsalow = Depth

d-6waph57m = Has children

d-syvxuusz-filter-process = Process

d-syvxuusz-filter-depth = Depth

d-syvxuusz-filter-has_children = Has children

d-meixnb6c = Monitor
    .doc = The app's root.
    .noun = { $count ->
        [one] monitor
       *[other] monitors
    }

d-4imh4jjl = End app
    .doc = Asks every process of an app to close.

d-4imh4jjl-application = Application

d-nqpvnf2v = State words
    .w-running = Running
    .w-sleeping = Sleeping
    .w-waiting-on-the-disk = Waiting on the disk
    .w-stopped = Stopped
    .w-zombie = Zombie
    .w-idle = Idle
    .w-dead = Dead

d-x6zdquhf = Bytes words
    .w-b = { $bytes } B
    .w-kib = { $value } KiB
    .w-mib = { $value } MiB
    .w-gib = { $value } GiB

d-5kjrbacm = Rate words
    .w-b-s = { $to_int } B/s
    .w-kib-s = { $to_int } KiB/s
    .w-mib-s = { $to_int } MiB/s
    .w-gib-s = { $to_int } GiB/s

d-ty7iq25n = Uptime words
    .w-d-h-m = { $days }d { $hours }h { $minutes }m
    .w-h-m = { $hours }h { $minutes }m
    .w-m = { $minutes }m

d-byhoiade = Window
    .w-system-monitor = System Monitor
    .w-sort-by = Sort by
    .w-processor = Processor
    .w-memory = Memory
    .w-name = Name
    .w-pid = PID
    .w-user = User
    .w-threads = Threads
    .w-falling = Falling
    .w-rising = Rising
    .w-update-frequency = Update frequency
    .w-every-second = Every second
    .w-every-1-5-seconds = Every 1.5 seconds
    .w-every-2-seconds = Every 2 seconds
    .w-every-3-seconds = Every 3 seconds
    .w-every-5-seconds = Every 5 seconds
    .w-paused = Paused
    .w-flat-list = Flat list
    .w-kernel-threads = Kernel threads
    .w-monitor = Monitor
    .w-processes = Processes
    .w-resources = Resources
    .w-cpu = CPU
    .w-memory-in-use = Memory in use
    .w-network = Network
    .w-disk = Disk
    .w-filter-processes = Filter processes
    .w-processes-2 = { $len } processes
    .w-no-processes-match-the-search = No processes match the search
    .w-no-process-selected = No process selected

d-prvlvs67 = Engine missing
    .w-processes-unavailable = Processes unavailable
    .w-this-computer-s-system-information-service = This computer's system information service is not answering.
    .w-try-again = Try again

d-rbjilrpa = Process detail
    .w-cpu = CPU
    .w-memory-rss = Memory (RSS)
    .w-pid = PID
    .w-user = User
    .w-status = Status
    .w-app = App
    .w-parent = Parent
    .w-threads = Threads
    .w-virtual-memory = Virtual memory
    .w-running-for = Running for
    .w-end-process = End process
    .w-kill-process = Kill process
    .w-end-app = End app

d-fvsvp5iz = Resources pane
    .w-processor = Processor
    .w-processor-use = Processor use
    .w-core = Core
    .w-memory = Memory
    .w-ram = RAM
    .w-of = { $bytes_words } of { $bytes_words2 }
    .w-swap = Swap
    .w-memory-in-use = Memory in use
    .w-network = Network
    .w-network-throughput = Network throughput
    .w-received = Received
    .w-sent = Sent
    .w-in-out = { $rate_words } in, { $rate_words2 } out
    .w-disk = Disk
    .w-disk-throughput = Disk throughput
    .w-of-at = { $bytes_words } of { $bytes_words2 } at { $mount }
    .w-graphics = Graphics
    .w-battery = Battery
    .w-temperature = Temperature
    .w-c = { $to_int } °C
    .w-this-computer = This computer
    .w-host-name = Host name
    .w-kernel = Kernel
    .w-cores = Cores
    .w-load-average = Load average
    .w-processes = Processes
    .w-threads = Threads
    .w-uptime = Uptime
    .w-pressure = Pressure
    .w-some = { $percent_words } some
