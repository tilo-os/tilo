## Verlauf der geteilten Prozesstabelle

monitor-monitor-sort = Die Prozesse nach { $new ->
    [pid] PID
    [name] Namen
    [user] Benutzer
    [mem] Speicher
    [threads] Threads
   *[cpu] CPU
} sortiert
monitor-monitor-ascending = { $new ->
    [true] Die Prozesse aufsteigend sortiert
   *[false] Die Prozesse absteigend sortiert
}
monitor-monitor-flat = { $new ->
    [true] Die Prozesse flach aufgelistet
   *[false] Den Prozessbaum angezeigt
}

## Fenster- und Barrierefreiheits-Texte
monitor-a11y-processes = Prozesse
