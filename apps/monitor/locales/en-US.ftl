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
