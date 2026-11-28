# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-containers = Container

d-so-tilo-containers-keywords = Container, podman, docker, Images, Volumes, Distrobox, container, virtualisierung

d-u3plltps = Entwurf
    .doc = Das Formular für einen neuen Container: was die Person getippt hat, ein Text je Feld, gemeinsam von allen bearbeitet, die es ansehen.
    .noun = { $count ->
        [one] Entwurf
       *[other] Entwürfe
    }

d-mnoqvwjm = Name

d-kc5y7sq2 = Image

d-gkz666im = Ports

d-7khtp7xl = Umgebung

d-utqr7b5l = Volumes

d-3jkgyx2g = Befehl

d-uk5fnglc = Download
    .doc = Ein Image, das gerade geladen wird, und was die Engine dabei geschrieben hat: das der Person, auf ihrem Gerät, damit es niemand anderen im Arbeitsbereich erreicht.
    .noun = { $count ->
        [one] Download
       *[other] Downloads
    }

d-shb65p47 = Image

d-gubc3b62 = Auftrag
    .doc = Der Auftrag der Engine, leer bevor er beginnt.

d-hhqtcu4c = Zeilen
    .doc = Die Ausgabe der Engine, die neueste zuletzt.

d-jgmjny63 = Fertig

d-m3krkqoe = Gestoppt
    .doc = Die Person hat es gestoppt; das bereits Geladene bleibt im Image-Speicher.

d-wektgccg = Fehlgeschlagen

d-uk5fnglc-filter-done = Fertig

d-uk5fnglc-filter-stopped = Gestoppt

d-et2qb7wr = Container
    .doc = Der eigene Eintrag der App: der Entwurf für einen neuen Container und die hier gestarteten Downloads.
    .noun = { $count ->
        [one] Container
       *[other] Container
    }

d-svwiioos = Entwurf

d-ilelnqwj = Downloads

d-et2qb7wr-filter-draft = Entwurf

d-putmiceq = Download-Anfrage
    .doc = Was einen Download beginnt, sagt welches Image.
    .noun = { $count ->
        [one] Download-Anfrage
       *[other] Download-Anfragen
    }

d-tukmkidh = Image

d-xjgsomkl = Download-Details
    .doc = Was die Engine über einen laufenden Download sagt: die Ausgabe des Images bisher.
    .noun = { $count ->
        [one] Download-Detail
       *[other] Download-Details
    }

d-bldlgtsf = Zeilen

d-xcaqps73 = Entwurf leeren
    .doc = Leert das Formular für einen neuen Container.

d-3zircjtu = Füllen
    .doc = Füllt ein Feld des Formulars für einen neuen Container, wie es ein Agent oder ein Skript täte: `name`, `image`, `ports`, `env`, `volumes` oder `command`.

d-3zircjtu-field = Feld

d-3zircjtu-text = Text

d-ojpmjsas = Container anlegen
    .doc = Führt ein Image als neuen Container aus dem Entwurf aus und leert das Formular, wenn die Engine ihn angenommen hat.
    .w-an-image-is-needed-to-run = Ein Image wird gebraucht, um einen Container auszuführen

d-pgwyi64n = Container zeigen
    .doc = Zeigt, was die Engine von einer Art hält.

d-ijt6urg5 = Images zeigen

d-k2tjtdvx = Volumes zeigen

d-syp6cif7 = Distroboxen zeigen

d-3uis6yzh = Neuer Container
    .doc = Öffnet das Formular für einen neuen Container, wie es `n` aus dem Fenster tut.

d-sv4joym2 = Volume anlegen
    .doc = Legt ein Volume an.
    .w-a-volume-needs-a-name = Ein Volume braucht einen Namen

d-sv4joym2-name = Name

d-w5cuwhcr = Image taggen
    .doc = Gibt einem Image eine weitere Referenz.
    .w-a-tag-needs-a-name = Ein Tag braucht einen Namen

d-w5cuwhcr-image = Image

d-w5cuwhcr-tag = Tag

d-dq4m27xc = Download beginnen
    .doc = Beginnt, ein Image zu laden, und behält seine Ausgabe, wie sie kommt.
    .w-there-is-no-image-to-pull = Es gibt kein Image zu laden

d-dq4m27xc-image = Image

d-ickgbq77 = Download meldet Fortschritt
    .doc = Der Download ist weitergegangen: was die Engine bisher geschrieben hat.

d-ickgbq77-j = J

d-5sku455z = Download beendet
    .doc = Der Download ist beendet, fertig oder fehlgeschlagen; er sagt, wie, und bleibt.

d-5sku455z-j = J

d-muuf5usk = Download abbrechen
    .doc = Beendet einen Download und den Prozess der Engine mit ihm.

d-muuf5usk-p = P

d-rov5aa6o = Download schließen
    .doc = Nimmt einen fertigen oder fehlgeschlagenen Download aus dem Fenster.

d-rov5aa6o-p = P

d-2wpoozqo = Shell öffnen
    .doc = Öffnet eine Shell in einem laufenden Container: die von podman, im Tilo-Terminal.

d-2wpoozqo-container = Container

d-krroi7ts = Box betreten
    .doc = Öffnet eine Shell in einer Distrobox.

d-krroi7ts-box = Box

d-odguvwgd = Eingabe ausführen
    .doc = Führt aus, wonach die Eingabe fragte, sobald die Person ihr Verb gedrückt hat.

d-odguvwgd-prompt = Eingabe

d-odguvwgd-typed = Getippt

d-jghrbzbr = Bestätigung ausführen
    .doc = Tut, was die Bestätigung nannte.

d-jghrbzbr-confirm = Bestätigung

d-lvoxmxjw = Ortsname
    .w-containers = Container
    .w-images = Images
    .w-volumes = Volumes
    .w-distrobox = Distrobox

d-nqlyjo45 = Standeswörter
    .w-running = Läuft
    .w-exited = Beendet
    .w-created = Angelegt
    .w-paused = Pausiert
    .w-restarting = Startet neu
    .w-needs-attention = Braucht Aufmerksamkeit

d-wkkxefqe = Größenwörter
    .w-b = { $bytes } B
    .w-kb = { $value } KB
    .w-mb = { $value } MB
    .w-gb = { $value } GB

d-lexzqqtm = Verwendet von
    .w-used-by-no-container = Von keinem Container verwendet
    .w-used-by-1-container = Von 1 Container verwendet
    .w-used-by-containers = Von { $count } Containern verwendet

d-2q7ehgyb = Image-Übersicht
    .w-one-image-uses-of-disk-space = Ein Image belegt { $size_words } Speicherplatz.
    .w-images-use-of-disk-space = { $count } Images belegen { $size_words } Speicherplatz.

d-6inrmjr5 = Volume-Übersicht
    .w-1-of-volumes-is-in-use = 1 von { $total } Volumes wird verwendet.
    .w-of-volumes-are-in-use = { $in_use } von { $total } Volumes werden verwendet.

d-l5th2rd2 = Eingabetitel
    .w-new-volume = Neues Volume
    .w-pull-an-image = Ein Image laden
    .w-add-a-tag-to = Tag zu { $slice } hinzufügen
    .w-remove = { $slice } entfernen?

d-bcreuc7u = Eingabebeschriftung
    .w-the-volume-s-name = Der Name des Volumes
    .w-the-image-to-pull = Das Image zum Laden
    .w-the-new-tag = Der neue Tag

d-juqarauo = Eingabeverb
    .w-create-volume = Volume anlegen
    .w-pull-image = Image laden
    .w-add-tag = Tag hinzufügen
    .w-remove = Entfernen
    .w-ok = OK

d-vqxvzopi = Container anlegen~present

d-b6lko7ef = Volume anlegen~present

d-ecmxyqle = Image taggen~present

d-6sqyauxv = Download beginnen~present

d-khvbvqhn = Fenster
    .w-containers = Container
    .w-this-computer = Dieser Computer
    .w-images = Images
    .w-volumes = Volumes
    .w-distrobox = Distrobox
    .w-podman-isn-t-answering = Podman antwortet nicht
    .w-new-container = Neuer Container
    .w-read-the-engine-again = Die Engine neu lesen
    .w-no-containers = Keine Container
    .w-create-a-container-from-an-image = Lege aus einem Image einen Container an, um ihn hier auszuführen.
    .w-remove = Entfernen
    .w-detail = Detail
    .w-logs = Protokoll
    .w-stats = Auslastung
    .w-inspect = Details
    .w-terminal = Terminal
    .w-no-container-selected = Kein Container ausgewählt
    .w-choose-a-container-to-see-its = Wähle einen Container, um sein Protokoll, seine Auslastung und seine Einstellungen zu sehen.
    .w-no-images = Keine Images
    .w-pull-an-image-to-create-containers = Lade ein Image, um daraus Container anzulegen.
    .w-pull-again = Erneut laden
    .w-add-a-tag = Tag hinzufügen
    .w-pull-an-image = Ein Image laden
    .w-no-volumes = Keine Volumes
    .w-a-volume-keeps-a-container-s = Ein Volume behält die Daten eines Containers über Neustarts hinweg.
    .w-new-volume = Neues Volume
    .w-runs-an-image-as-a-container = Führt ein Image als Container auf diesem Computer aus.
    .w-create = Anlegen
    .w-cancel = Abbrechen
    .w-image = Image
    .w-docker-io-library-nginx-alpine = docker.io/library/nginx:alpine
    .w-name = Name
    .w-empty-for-a-name-podman-picks = Leer für einen Namen, den podman wählt.
    .w-my-service = mein-dienst
    .w-ports = Ports
    .w-host-and-container-port-separated-by = Host- und Container-Port, durch Kommas getrennt.
    .w-environment = Umgebung
    .w-key-value-pairs-separated-by-commas = KEY=Wert-Paare, durch Kommas getrennt.
    .w-tz-utc-log-level-info = TZ=UTC, LOG_LEVEL=info
    .w-volume-and-path-separated-by-commas = Volume und Pfad, durch Kommas getrennt.
    .w-pgdata-var-lib-postgresql-data = pgdata:/var/lib/postgresql/data
    .w-command = Befehl
    .w-empty-to-run-the-image-s = Leer, um den Standard des Images auszuführen.
    .w-remove-this-container = Diesen Container entfernen?
    .w-the-container-and-the-changes-made = Der Container und die darin gemachten Änderungen werden gelöscht. Seine Volumes bleiben.
    .w-the-image-is-deleted-from-this = Das Image wird von diesem Computer gelöscht. Du kannst es später erneut laden.
    .w-the-volume-and-the-data-stored = Das Volume und die darin gespeicherten Daten werden gelöscht.

d-c5zef5ks = Engine fehlt
    .w-podman-isn-t-available = Podman ist nicht verfügbar
    .w-install-podman-or-start-its-user = Installiere Podman oder starte seinen Benutzerdienst, um hier Container zu verwalten.
    .w-try-again = Erneut versuchen

d-omimf7ko = Lebenslauf-Schaltflächen
    .w-stop = Stoppen
    .w-restart = Neu starten
    .w-pause = Pausieren
    .w-resume = Fortsetzen
    .w-start = Starten

d-7iv57xex = Protokollbereich
    .w-this-container-hasn-t-written-anything = Dieser Container hat noch nichts geschrieben.
    .w-error = Fehler
    .w-warning = Warnung

d-dyv52unk = Auslastungsbereich
    .w-start-the-container-to-see-what = Starte den Container, um zu sehen, was er nutzt.
    .w-podman-returned-no-sample-for-this = Podman hat für diesen Container keine Messung geliefert.
    .w-usage = Nutzung
    .w-processor = Prozessor
    .w-of-one-processor = { $value } % eines Prozessors
    .w-memory = Speicher
    .w-of = { $size_words } von { $size_words2 }
    .w-processes = Prozesse
    .w-activity = Aktivität
    .w-received = Empfangen
    .w-sent = Gesendet
    .w-read-from-disk = Von der Platte gelesen
    .w-written-to-disk = Auf die Platte geschrieben

d-hpakp53f = Detailbereich
    .w-details = Details
    .w-id = ID
    .w-image = Image
    .w-command = Befehl
    .w-created = Angelegt
    .w-state = Stand
    .w-ports = Ports
    .w-mounts = Einhängepunkte
    .w-read-only = { $source } (nur lesen)
    .w-environment = Umgebung
    .w-labels = Labels

d-p3ynizc6 = Terminalbereich
    .w-opens-bin-sh-inside-the-container = Öffnet /bin/sh im Container, in einem Terminalfenster.
    .w-open-a-shell = Eine Shell öffnen
    .w-start-the-container-to-open-a = Starte den Container, um eine Shell darin zu öffnen.
    .w-start = Starten

d-72k7h2qu = Download-Banner
    .w-pulling = { $image } wird geladen
    .w-stopped-what-had-been-fetched-stays = Gestoppt. Was bereits geladen wurde, bleibt im Image-Speicher.
    .w-the-image-is-ready-to-create = Das Image ist bereit, um Container daraus anzulegen.
    .w-the-engine-is-fetching-the-image = Die Engine lädt das Image.
    .w-stop-this-pull = Diesen Download stoppen
    .w-close = Schließen

d-vrra64ga = Distrobox-Seite
    .w-no-distroboxes = Keine Distroboxen
    .w-create-a-box-with-distrobox-to = Lege mit distrobox eine Box an, um sie hier auszuführen.
    .w-enter = Betreten
