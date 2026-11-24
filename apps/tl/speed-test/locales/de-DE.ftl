# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-speed-test = Geschwindigkeitstest

d-so-tilo-speed-test-keywords = geschwindigkeitstest, internetgeschwindigkeit, bandbreite, ping, latenz, downloadgeschwindigkeit, upload, netzwerktest, verbindungsgeschwindigkeit, mbps

d-kobrqlxh = Geschwindigkeitstest
    .doc = The connections this workspace measured: one entry for every finished run, newest first, and the run the window shows, while it measures and after it ends.
    .noun = { $count ->
        [one] speed test
       *[other] speed tests
    }

d-hpwi5yye = Ergebnisse
    .doc = Every finished run, newest first.

d-phw7nc7e = Läufe
    .doc = The run on the page: the one measuring now, and the one that just ended.

d-ckq5elpb = Nächste Abfrage
    .doc = When the device is asked how far the run has got; left behind when the run ends, so nothing asks again.

d-kobrqlxh-filter-next_poll = Nächste Abfrage

d-l5v53ewu = Messung
    .doc = One finished run: what it measured, where, and on which device.
    .noun = { $count ->
        [one] measurement
       *[other] measurements
    }

d-5f54c4ou = Ping
    .doc = Round trip to the server, in milliseconds.

d-q5jjysbc = Jitter
    .doc = Variation of the round trip, in milliseconds.

d-kded6ppq = Download
    .doc = Download speed, in megabits per second.

d-x6xgknqc = Upload
    .doc = Upload speed, in megabits per second.

d-vcibj65j = Server
    .doc = The server that answered.

d-vbch5rhv = Gerät
    .doc = The device that ran it.

d-l5v53ewu-filter-ping = Ping

d-l5v53ewu-filter-jitter = Jitter

d-l5v53ewu-filter-download = Download

d-l5v53ewu-filter-upload = Upload

d-ylzbkpat = Lauf
    .doc = A run of the test: where it stands, what it has read so far, and why it stopped early.
    .noun = { $count ->
        [one] run
       *[other] runs
    }

d-fbbyyume = Phase
    .doc = Where the run stands: `ping`, `download`, `upload`, `complete` or `error`.

d-376ly3mp = Auftrag
    .doc = The device job measuring, while one runs; empty between runs.

d-vufvfoij = Aktuell
    .doc = The reading of the stage in progress: milliseconds while pinging, megabits per second after.

d-stw2oyj6 = Bytes
    .doc = The bytes the run moved so far.

d-nwtvekek = Server
    .doc = The server that answered.

d-wkhwlknc = Fehler
    .doc = Why the run stopped early, in the words of the stage that failed.

d-ylzbkpat-filter-live = Aktuell

d-ylzbkpat-filter-bytes = Bytes

d-bofeq5tj = Laufzustand
    .doc = What the device answers about a run (`engine.status`).
    .noun = { $count ->
        [one] run state
       *[other] run states
    }

d-4xzztkvp = Zustand

d-os4ndogp = Phase

d-7eyrzvhi = Aktuell

d-phil2ezm = Bytes

d-5szkqkfk = Server

d-hcm22tea = Ping

d-esi4hzhw = Jitter

d-gmjsrtmo = Download

d-q4qvdsia = Upload

d-eyu5ljwd = Gerät

d-c6eopm2r = Fehlschlag

d-bofeq5tj-filter-live = Aktuell

d-bofeq5tj-filter-bytes = Bytes

d-bofeq5tj-filter-ping = Ping

d-bofeq5tj-filter-jitter = Jitter

d-bofeq5tj-filter-download = Download

d-bofeq5tj-filter-upload = Upload

d-divjfr4m = Gestartet
    .doc = What the device answers when a run starts (`engine.start`).
    .noun = { $count ->
        [one] started
       *[other] starteds
    }

d-fltipwm6 = Auftrag

d-gs3ocynq = Test starten
    .doc = Starts a run: the device measures latency, download and upload against the nearest Cloudflare test server, and the window follows it as it goes.
    .did = hat einen Geschwindigkeitstest gemacht
    .w-a-test-is-running = Ein Test läuft gerade

d-eoox4ss7 = Test stoppen
    .doc = Stops the run in flight: the device stops measuring and the dial goes back to ready.
    .did = hat einen Geschwindigkeitstest gestoppt
    .w-no-test-is-running = Kein Test läuft gerade

d-crsodmy5 = Fragt, wie weit der Test ist
    .doc = Asks the device how far the run has got: the dial follows the reading of the stage, and what the run measured is kept when it ends.

d-zpm53plh = Messung speichern
    .doc = Records a finished measurement, newest first, and lets the oldest go beyond what the workspace keeps: an agent or a script may record one too.
    .did = hat eine Messung gespeichert

d-zpm53plh-ping = Ping

d-zpm53plh-jitter = Jitter

d-zpm53plh-download = Download

d-zpm53plh-upload = Upload

d-zpm53plh-server = Server

d-zpm53plh-device = Gerät

d-7joaum2r = Ergebnisse löschen
    .doc = Forgets every measurement.
    .did = hat die Messungen gelöscht
    .w-no-test-has-been-measured = Noch kein Test gemessen

d-apn5jyhr = Geschwindigkeitstest
    .w-idle = bereit
    .w-test-interrupted = Test unterbrochen

d-waf6xz4h = Bereite Seite
    .w-idle = bereit
    .w-ready-to-test = Bereit zum Testen
    .w-measures-against-the-nearest-cloudflare-server = Misst gegen den nächsten Cloudflare-Server
    .w-measures-against = Misst gegen { $server }

d-x2kfhyxc = Testspalte
    .w-measuring-latency = Latenz wird gemessen
    .w-measuring-download = Download wird gemessen
    .w-measuring-upload = Upload wird gemessen
    .w-test-complete = Test abgeschlossen
    .w-ready-to-try-again = Bereit für einen neuen Versuch
    .w-ready-to-test = Bereit zum Testen
    .w-ping = Ping
    .w-download = Download
    .w-upload = Upload

d-qy655f3m = Ergebnisspalte
    .w-results = Ergebnisse
    .w-ping = Ping
    .w-ms = ms
    .w-download = Download
    .w-mbps = Mbps
    .w-upload = Upload
    .w-connection = Verbindung
    .w-server = Server
    .w-jitter = Jitter
    .w-ms-2 = { $one } ms
    .w-data-used = Verbrauchte Daten
    .w-connection-quality = Verbindungsqualität
    .w-video-calls = Videoanrufe
    .w-4k-streaming = 4K-Streaming
    .w-cloud-gaming = Cloud-Gaming
    .w-recent-tests = Letzte Tests

d-ergxoae7 = Qualitätszeile
    .w-excellent = Sehr gut
    .w-limited = Begrenzt

d-l4t3t7cr = Letzte Zeile
    .w-mbps-down = { $one } Mbps herunter
    .w-mbps-up-ms-ping = { $one } Mbps hoch, { $whole } ms Ping

d-jsy7gexo = Zifferblatt
