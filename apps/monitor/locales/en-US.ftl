## History of the shared process table

monitor-monitor-sort = Sorted the processes by { $new ->
    [pid] PID
    [name] name
    [user] user
    [mem] memory
    [threads] threads
   *[cpu] CPU
}
monitor-monitor-ascending = { $new ->
    [true] Sorted the processes ascending
   *[false] Sorted the processes descending
}
monitor-monitor-flat = { $new ->
    [true] Listed the processes flat
   *[false] Showed the process tree
}

## Window and accessibility copy
monitor-a11y-processes = Processes

## Facts, property labels and section titles
monitor-command = Command
monitor-running-for = Running for
monitor-open-files = Open files
monitor-virtual-memory = Virtual memory
monitor-threads = Threads
monitor-parent = Parent
monitor-app = App
monitor-status = Status
monitor-user = User
monitor-memory-children = Memory with children
monitor-cpu-children = CPU with children
monitor-processor = Processor
monitor-kernel = Kernel
monitor-host-name = Host name

monitor-section-system = System
monitor-legend-in-use = In use { $percent }
monitor-section-memory = Memory

## What the window shows
monitor-cpu-total = Total { $percent }
monitor-cpu-core = Core { $core }
monitor-memory-swap = Swap
monitor-section-network = Network
monitor-section-disk = Disk
monitor-usage-unavailable-message = This computer's processor, memory, network and disk use couldn't be read.

## The window bar's slots
monitor-tab-resources = Resources
monitor-views = View
monitor-refresh = Refresh
monitor-update-frequency = Update frequency: { $cadence }
monitor-tree-view = Tree view
monitor-include-children = Include children
monitor-kernel-threads = Kernel threads
monitor-cadence-paused = Paused
monitor-cadence-second = Every second
monitor-cadence-seconds = Every { $seconds } seconds

## Actions

monitor-effect-show-tab = Show tab
monitor-effect-filter = Filter processes
monitor-effect-inspect = Show process details
monitor-effect-copy-pid = Copy PID
monitor-effect-end-process = End process
monitor-effect-kill = Force quit
monitor-effect-end-app = End app
monitor-processes = Processes
monitor-effect-refresh = Refresh processes
