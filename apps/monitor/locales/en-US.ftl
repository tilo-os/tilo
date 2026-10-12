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
