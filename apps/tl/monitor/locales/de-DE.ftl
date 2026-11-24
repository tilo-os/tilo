# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-monitor = Systemüberwachung

d-so-tilo-monitor-keywords = Überwachung, system monitor, aktivität, prozesse, cpu, speicher, task-manager, ressourcen, prozesse

d-syvxuusz = Zeile
    .doc = Eine Zeile der Prozessliste: der Prozess, wie tief er unter seinem Elternprozess steht, und ob er Kinder hat.
    .noun = { $count ->
        [one] Zeile
       *[other] Zeilen
    }

d-uoc4p4dx = Prozess

d-qwzsalow = Tiefe

d-6waph57m = Hat Kinder

d-syvxuusz-filter-process = Prozess

d-syvxuusz-filter-depth = Tiefe

d-syvxuusz-filter-has_children = Hat Kinder

d-meixnb6c = Überwachung
    .doc = Die Wurzel der App.
    .noun = { $count ->
        [one] Überwachung
       *[other] Überwachungen
    }

d-4imh4jjl = App beenden
    .doc = Bittet alle Prozesse einer App zu schließen.

d-4imh4jjl-application = Anwendung

d-nqpvnf2v = Standeswörter
    .w-running = Läuft
    .w-sleeping = Schläft
    .w-waiting-on-the-disk = Wartet auf die Platte
    .w-stopped = Angehalten
    .w-zombie = Zombie
    .w-idle = Untätig
    .w-dead = Tot

d-x6zdquhf = Bytewörter
    .w-b = { $bytes } B
    .w-kib = { $value } KiB
    .w-mib = { $value } MiB
    .w-gib = { $value } GiB

d-5kjrbacm = Ratenwörter
    .w-b-s = { $to_int } B/s
    .w-kib-s = { $to_int } KiB/s
    .w-mib-s = { $to_int } MiB/s
    .w-gib-s = { $to_int } GiB/s

d-ty7iq25n = Laufzeitwörter
    .w-d-h-m = { $days }d { $hours }h { $minutes }m
    .w-h-m = { $hours }h { $minutes }m
    .w-m = { $minutes }m

d-byhoiade = Fenster
    .w-include-children = Unterprozesse einbeziehen
    .w-system-monitor = Systemüberwachung
    .w-sort-by = Sortieren nach
    .w-processor = Prozessor
    .w-memory = Speicher
    .w-name = Name
    .w-pid = PID
    .w-user = Benutzer
    .w-threads = Threads
    .w-falling = Fallend
    .w-rising = Steigend
    .w-update-frequency = Aktualisierungsintervall
    .w-every-second = Jede Sekunde
    .w-every-1-5-seconds = Alle 1,5 Sekunden
    .w-every-2-seconds = Alle 2 Sekunden
    .w-every-3-seconds = Alle 3 Sekunden
    .w-every-5-seconds = Alle 5 Sekunden
    .w-paused = Angehalten
    .w-flat-list = Flache Liste
    .w-kernel-threads = Kernel-Threads
    .w-monitor = Überwachung
    .w-processes = Prozesse
    .w-resources = Ressourcen
    .w-cpu = CPU
    .w-memory-in-use = Speicher in Verwendung
    .w-network = Netzwerk
    .w-disk = Platte
    .w-processes-2 = { $len } Prozesse
    .w-no-processes-match-the-search = Kein Prozess passt zur Suche
    .w-no-process-selected = Kein Prozess ausgewählt

d-prvlvs67 = Engine fehlt
    .w-processes-unavailable = Prozesse nicht verfügbar
    .w-this-computer-s-system-information-service = Der Systeminformationsdienst dieses Computers antwortet nicht.
    .w-try-again = Erneut versuchen

d-rbjilrpa = Prozessdetails
    .w-command-line = Kommandozeile
    .w-open-files = Offene Dateien
    .w-cpu = CPU
    .w-memory-rss = Speicher (RSS)
    .w-pid = PID
    .w-user = Benutzer
    .w-status = Stand
    .w-app = App
    .w-parent = Elternprozess
    .w-threads = Threads
    .w-virtual-memory = Virtueller Speicher
    .w-running-for = Läuft seit
    .w-end-process = Prozess beenden
    .w-kill-process = Prozess abbrechen
    .w-end-app = App beenden

d-fvsvp5iz = Ressourcenbereich
    .w-processor = Prozessor
    .w-processor-use = Prozessorauslastung
    .w-core = Kern
    .w-memory = Speicher
    .w-ram = RAM
    .w-of = { $bytes_words } von { $bytes_words2 }
    .w-swap = Auslagerung
    .w-memory-in-use = Speicher in Verwendung
    .w-network = Netzwerk
    .w-network-throughput = Netzwerkdurchsatz
    .w-received = Empfangen
    .w-sent = Gesendet
    .w-in-out = { $rate_words } ein, { $rate_words2 } aus
    .w-disk = Platte
    .w-disk-throughput = Plattendurchsatz
    .w-of-at = { $bytes_words } von { $bytes_words2 } unter { $mount }
    .w-graphics = Grafik
    .w-battery = Akku
    .w-temperature = Temperatur
    .w-c = { $to_int } °C
    .w-this-computer = Dieser Computer
    .w-host-name = Rechnername
    .w-kernel = Kernel
    .w-cores = Kerne
    .w-load-average = Durchschnittslast
    .w-processes = Prozesse
    .w-threads = Threads
    .w-uptime = Laufzeit
    .w-pressure = Druck
    .w-some = { $percent_words } teilweise

d-22v7oibb = CPU
    .doc = Was er nutzt, mit dem, was unter ihm steht, wenn die Person das verlangt.


d-cwilirks = Threads

d-mycnv7ws = Takt übernehmen
    .doc = Hält die Messung dieses Geräts im Takt, in dem die Person sie beobachtet.

d-mycnv7ws-ms = Millisekunden

d-mycnv7ws-paused = Pausiert


d-syvxuusz-filter-cpu = CPU
d-syvxuusz-filter-depth = Tiefe
d-syvxuusz-filter-has_children = Hat Unterprozesse
d-syvxuusz-filter-memory_bytes = Speicher
d-syvxuusz-filter-process = Prozess
d-syvxuusz-filter-threads = Threads

d-u6hj324t = Speicher
