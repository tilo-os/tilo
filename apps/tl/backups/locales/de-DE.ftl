# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-backups = Datensicherung

d-so-tilo-backups-keywords = Sicherung, Datensicherung, Stand, Wiederherstellen, Aufbewahrung, Archiv, Backup

d-lcjavy23 = Eintrag
    .doc = Ein Eintrag im Dateiverzeichnis eines Stands, wie die Wiederherstellung ihn zeigt.
    .noun = { $count ->
        [one] entry
       *[other] entries
    }

d-cvlieest = Name

d-dssaufce = Pfad
    .doc = Sein Pfad im Stand: woran sich ein Ordner von einem anderen gleichen Namens unterscheidet und wonach ein Fenster seine Inhalte einklappt.

d-c2m5wzf4 = Tiefe
    .doc = Wie tief der Eintrag im Stand liegt.

d-pfl5jyx2 = Größe
    .doc = Größe in Bytes; null für einen Ordner.

d-tzyurmps = Ordner

d-lcjavy23-filter-depth = Tiefe

d-lcjavy23-filter-size = Größe

d-lcjavy23-filter-folder = Ordner

d-cffyj3mc = Eingeklappt
    .doc = Ein Ordner aus dem Dateiverzeichnis eines Stands, den die Person eingeklappt hat.
    .noun = { $count ->
        [one] eingeklappter Ordner
       *[other] eingeklappte Ordner
    }

d-yp5mxjwc = Pfad
    .doc = Der Pfad des Ordners im Stand.

d-euqtsjtw = Stand
    .doc = Ein gespeicherter Stand des Ordners, den dieses Gerät schützt.
    .noun = { $count ->
        [one] snapshot
       *[other] snapshots
    }
    .untitled = Stand

d-dallfwqf = Bezeichnung
    .doc = Der Name, den eine Person ihm gab, falls sie einen gab.

d-ngqbekfc = Zeitpunkt
    .doc = Die Sekunde, in der er entstand, gezählt ab 1970.

d-hydekdnu = Art

d-yffctwuv = Aufbewahrt

d-6kvlgzq4 = Zustand

d-ljb7imfg = Bytes

d-vcgdjszb = Geänderte Bytes

d-yr6zgb2x = Geänderte Dateien

d-cyesgrji = Fehler
    .doc = Warum es fehlschlug, falls es das tat.

d-5nvgiivd = Dateien

d-sikyuvd6 = Eingeklappt
    .doc = Die Ordner seines Dateiverzeichnisses, die die Person eingeklappt hat.

d-wzd6czrl = Aufbewahrung
    .noun = { $count ->
        [one] policy
       *[other] policies
    }

d-cyugw7lh = Stündlich
    .doc = Behaltene Stände je Klasse.

d-avot6p6r = Täglich

d-ywba2jzd = Wöchentlich

d-3miz6dsu = Monatlich

d-wzd6czrl-filter-hourly = Stündlich

d-wzd6czrl-filter-daily = Täglich

d-wzd6czrl-filter-weekly = Wöchentlich

d-wzd6czrl-filter-monthly = Monatlich

d-v7z4w6ml = Ort
    .doc = Der Ordner, den eine Person in der Auswahl wählte, bevor er zum Ordner der Sicherung wird.
    .noun = { $count ->
        [one] place
       *[other] places
    }

d-dho3qqg3 = Name

d-f3pl3mol = Pfad

d-igeecx5d = Datensicherung
    .doc = Die Datensicherung des Arbeitsbereichs: der Ordner, den dieses Gerät schützt, wohin seine Stände gehen, die Stände darin und die Aufbewahrungsregel, die sie ausdünnt.
    .noun = { $count ->
        [one] backup
       *[other] backups
    }
    .untitled = Datensicherung

d-p2imk5l5 = Aufbewahrung
    .doc = Wie viele Stände je Klasse eine Sicherung behält.

d-srrmxp5i = Quelle
    .doc = Der Ordner, den dieses Gerät schützt; leer, bis eine Person einen wählt.

d-6e5yqotr = Ziel
    .doc = Der Ordner, in den Stände geschrieben werden; leer, bis eine Person einen wählt.

d-62wijgsn = Quellname
    .doc = Der Name des Quellordners.

d-x6ca2mpx = Quellherkunft

d-gfsjtev7 = Quellrhythmus

d-3wbouwis = Quelle gestört

d-ymfvuwvo = Nächste Ausführung
    .doc = Wann die nächste Sicherung läuft, gezählt ab 1970; null, wenn nichts sie plant.

d-rnz74k4z = Zeile der nächsten Ausführung
    .w-by-hand = Von Hand
    .w-not-known = Nicht bekannt

d-v3i3cihq = Zielname
    .doc = Der Name des Zielordners und was er enthält.

d-kqbve5pf = Belegte Bytes

d-mdbssite = Gesamtbytes
    .doc = Die Kapazität des Laufwerks; null, wenn sie nicht genannt wird.

d-kz5xznku = Verschlüsselt

d-hlhhrwm3 = Ziel gestört

d-n7gmur3t = Stände
    .doc = Die gespeicherten Stände, neueste zuerst.

d-ik3j63rv = Ort
    .doc = Der Ort, den die Seitenleiste zeigt: `all`, `source`, `destination` oder `retention`.

d-6fg5mkqa = Neueste zuerst
    .doc = Ob der Verlauf die neuesten zuerst listet.

d-sxzlbnam = Störung
    .doc = Der Blick ins Depot ging schief, in den Worten des Geräts.

d-yelwker2 = Fehler
    .doc = Die Arbeit schlug fehl oder wurde abgelehnt, in den Worten des Geräts; ein Blick ins Depot löscht das nicht.

d-6rkekawk = Hinweis
    .doc = Eine Zeile unter der Liste: was getan oder beendet wurde.

d-bluheyea = Auftrag
    .doc = Die laufende Arbeit auf diesem Gerät, nach dem Auftrag, den das Gerät gab.

d-wmrb4vzi = Arbeit
    .doc = Welche Arbeit der Auftrag ist: `backup`, `verify` oder `restore`.

d-v7lhpobw = Fortschritt
    .doc = Wie weit die Arbeit ist, in Dateien und Bytes.

d-xsai3dea = Ergebnis
    .doc = Was eine Prüfung oder Wiederherstellung fand, als sie fertig war.

d-fihmdmmi = Betreff
    .doc = Der Stand, der geprüft oder wiederhergestellt wird, nach seiner Sekunde.

d-q6dvqz3z = Auswählen
    .doc = Welchen Ordner die Person wählt: `source`, `destination` oder `restore`.

d-vlgd4tne = Auswahl
    .doc = Der Ordner, den die Auswahl durchläuft, und die Ordner darin.

d-6nds5bnc = Gefunden

d-5dqejapv = Wiederherstellen nach
    .doc = Der Ordner, in den eine Wiederherstellung schreibt, und ob sie ersetzen darf, was dort liegt.

d-azcuku5y = Ersetzen

d-igeecx5d-filter-policy = Aufbewahrung

d-igeecx5d-filter-source_origin = Quellherkunft

d-igeecx5d-filter-source_cadence = Quellrhythmus

d-igeecx5d-filter-source_failing = Quelle gestört

d-igeecx5d-filter-next_run = Nächste Ausführung

d-igeecx5d-filter-used_bytes = Belegte Bytes

d-igeecx5d-filter-total_bytes = Gesamtbytes

d-igeecx5d-filter-encrypted = Verschlüsselt

d-igeecx5d-filter-destination_failing = Ziel gestört

d-igeecx5d-filter-newest_first = Neueste zuerst

d-igeecx5d-filter-subject = Betreff

d-igeecx5d-filter-overwrite = Ersetzen

d-v6ydjahg = Katalogauftrag
    .noun = { $count ->
        [one] catalog ask
       *[other] catalog asks
    }

d-gbluxeo3 = Quelle

d-ixnr6cnx = Ziel

d-zzjhdxbx = Sicherungsauftrag
    .noun = { $count ->
        [one] backup ask
       *[other] backup asks
    }

d-w7elqfbk = Quelle

d-ebybhxkw = Ziel

d-qqrrtmrs = Aufbewahrung stündlich

d-qrbyjmwo = Aufbewahrung täglich

d-jd3hqbyz = Aufbewahrung wöchentlich

d-6z7komoa = Aufbewahrung monatlich

d-zzjhdxbx-filter-policy_hourly = Aufbewahrung stündlich

d-zzjhdxbx-filter-policy_daily = Aufbewahrung täglich

d-zzjhdxbx-filter-policy_weekly = Aufbewahrung wöchentlich

d-zzjhdxbx-filter-policy_monthly = Aufbewahrung monatlich

d-kjitlism = Standauftrag
    .noun = { $count ->
        [one] snapshot ask
       *[other] snapshot asks
    }

d-2stygd3d = Ziel

d-vfb2hw7j = Stand

d-3pulbfdz = Wiederherstellungsauftrag
    .noun = { $count ->
        [one] restore ask
       *[other] restore asks
    }

d-z2pcuhft = Ziel

d-qz4acm4a = Stand

d-3hxnk3mo = Ziel

d-lbwhdcbx = Ersetzen

d-3pulbfdz-filter-overwrite = Ersetzen

d-66xmnr3b = Arbeitszustand
    .noun = { $count ->
        [one] work state
       *[other] work states
    }

d-ptww3noe = Zustand

d-pnevrulc = Fortschritt

d-lrfiygoo = Antwort

d-apwu74vg = Fehler

d-znoewa74 = Geprüft
    .noun = { $count ->
        [one] checked
       *[other] checkeds
    }

d-lg6ud7lc = Dateien

d-deudvrko = Bytes

d-znoewa74-filter-files = Dateien

d-znoewa74-filter-bytes = Bytes

d-tfxlhxcp = Art
    .doc = Wurde ein Stand vom Zeitplan oder von Hand angelegt?

d-ijjeki2d = Automatisch

d-yu3bengf = Von Hand

d-dczpbrdi = Unbekannt

d-4nziw5wl = Aufbewahrt
    .doc = Die Aufbewahrungsklasse, in die ein Stand zählt.

d-guchm5y3 = Stündlich

d-7ehlxq37 = Täglich

d-dbtfb3es = Wöchentlich

d-gdrmnn2g = Monatlich

d-ib2ogkjg = Aufbewahrt

d-5ew2vbah = Unbekannt

d-nyssh3ek = Zustand
    .doc = Ob ein gespeicherter Stand vollständig, in Prüfung oder bekannt defekt ist.

d-z5ipmi4c = Vollständig

d-d57dpcpd = Prüfe

d-cdszgdvi = Fehlgeschlagen

d-y7yrr6jo = Herkunft
    .doc = Was eine Quelle ist, so weit ihr Pfad es sagt, ohne ihn zu nennen.

d-2owudtrn = Home

d-cmidvqwr = Systemdateien

d-wzgcvto4 = Arbeitsbereich

d-z6vj6vhj = Eigener Ordner

d-qxqolkzh = Rhythmus
    .doc = Wann sich eine Quelle selbst sichert.

d-wpyvyoau = Auf Abruf

d-67d5hncl = Stündlich

d-jjyafzb3 = Täglich

d-fzew3dgc = Wöchentlich

d-3fzr3jov = Störung
    .doc = Warum der letzte Blick schiefging.

d-d2scemnx = Keiner

d-3ahku3ip = Unlesbar

d-xazatgpx = Fehlend

d-67y2enqb = Erneut nachsehen
    .doc = Sieht im Depot erneut nach: die Quelle, das Ziel und die Stände darin.

d-rvp6xxbz = Katalog behalten
    .doc = Hält fest, was das Gerät über sein Depot geantwortet hat, damit Fenster und Agent dieselben Stände lesen.

d-rvp6xxbz-answer = Antwort

d-ekhujpxs = Jetzt sichern
    .doc = Sichert den Ordner jetzt und behält, was die Aufbewahrungsregel behält.
    .w-choose-a-folder-to-protect-first = Wähle zuerst einen Ordner zum Schützen
    .w-choose-where-the-snapshots-go-first = Wähle zuerst, wohin die Stände gehen
    .w-one-piece-of-work-is-running = Es läuft bereits eine Arbeit

d-fmzgim3f = Stand prüfen
    .doc = Prüft, ob jede Datei eines Stands noch dem entspricht, was der Stand festhielt.
    .w-one-piece-of-work-is-running = Es läuft bereits eine Arbeit

d-fmzgim3f-s = S

d-ttl3xkqx = Stand wiederherstellen
    .doc = Öffnet das Blatt, das fragt, wohin ein Stand zurückgeht.
    .w-one-piece-of-work-is-running = Es läuft bereits eine Arbeit

d-ttl3xkqx-s = S

d-4v3uefx5 = Wiederherstellen beginnen
    .doc = Beginnt, den Ordner für die Wiederherstellung zu suchen, ohne schon etwas auszuführen.

d-v5ylljvo = Wiederherstellen
    .doc = Stellt den Stand in dem Ordner wieder her, den die Person wählte.
    .w-choose-a-folder-to-restore-into = Wähle einen Ordner zum Wiederherstellen
    .w-one-piece-of-work-is-running = Es läuft bereits eine Arbeit

d-v5ylljvo-s = S

d-pz4zxkew = Arbeit anhalten
    .doc = Hält die laufende Arbeit an; was sie schrieb, wird entfernt.

d-eq6dkezf = Abfragen
    .doc = Fragt das Gerät einmal pro Sekunde, wie weit die Arbeit ist, solange sie läuft.

d-nrshrfas = Stand öffnen
    .doc = Zeigt das Blatt eines Stands: was er enthält und was damit möglich ist.
    .did = hat einen Stand geöffnet

d-nrshrfas-s = S

d-5gdsin6x = Reihenfolge
    .doc = Listet den Verlauf neueste zuerst, oder älteste zuerst.

d-5gdsin6x-newest_first = Neueste zuerst

d-w7nn64aa = Ort anzeigen
    .doc = Zeigt einen der Orte in der Seitenleiste.

d-w7nn64aa-place = Ort

d-vap65q73 = Einen mehr behalten
    .doc = Behält einen Stand einer Klasse mehr.

d-vap65q73-which = Welche

d-h6ethwbw = Einen weniger behalten
    .doc = Behält einen Stand einer Klasse weniger.

d-h6ethwbw-which = Welche

d-x5h6br5g = Ausblenden
    .doc = Blendet die Zeile unter der Liste aus.

d-z6m2czb6 = Ordner zum Schützen wählen
    .doc = Fragt die Person, welchen Ordner dieses Gerät schützt.

d-ioanf4us = Wählen, wohin Stände gehen
    .doc = Fragt die Person, wohin die Stände geschrieben werden.

d-54fhu267 = Öffnen
    .doc = Geht in der Auswahl in einen anderen Ordner und zeigt die Ordner darin.

d-54fhu267-path = Pfad

d-k5qqlbcc = Nach oben
    .doc = Geht in der Auswahl einen Ordner nach oben.

d-nj4ehqok = Diesen Ordner verwenden
    .doc = Nimmt den Ordner, den die Auswahl zeigt: als geschützten Ordner, als Ziel der Stände oder als Ziel einer Wiederherstellung.

d-egcbexsk = Abbrechen
    .doc = Verlässt die Auswahl, ohne etwas zu ändern.

d-5b35wzq6 = Auswahl lesen
    .doc = Liest die Ordner des Ordners, den die Auswahl zeigt.

d-ifgic4vt = Arbeit starten
    .doc = Startet eine Arbeit und merkt sich den Auftrag, den das Gerät gab.

d-ifgic4vt-kind = Art

d-ifgic4vt-ask = Auftrag

d-vq7l4nar = Stand schließen

d-apgumkkc = Ersetzen erlauben

d-t5f6lf3u = Jetzt gesichert

d-ngp7g7pa = Geprüft

d-s2kwexka = Wiederherstellen angefragt

d-4vnqjv4m = Wiederhergestellt

d-yeswuxpe = Fenster

d-5kspbgsu = Auswahl
    .w-choose-the-folder-to-protect = Ordner zum Schützen wählen
    .w-choose-where-snapshots-go = Wählen, wohin Stände gehen
    .w-choose-the-folder-to-restore-into = Ordner zum Wiederherstellen wählen
    .w-up = Nach oben
    .w-restore-into-this-folder = In diesen Ordner wiederherstellen
    .w-use-this-folder = Diesen Ordner verwenden
    .w-cancel = Abbrechen
    .w-that-folder-could-not-be-read = Der Ordner ließ sich nicht lesen
    .w-folders = Ordner
    .w-no-folders-inside = Keine Ordner darin

d-xpn57dne = Einrichtung
    .w-set-up-backups = Datensicherung einrichten
    .w-choose-a-folder-to-protect-and = Wähle einen Ordner zum Schützen und wohin seine Stände gehen.
    .w-the-folder-to-protect = Der zu schützende Ordner
    .w-nothing-chosen-yet = Noch nichts gewählt
    .w-snapshots-copy-this-folder-as-it = Stände kopieren diesen Ordner, wie er jetzt ist.
    .w-choose-the-folder-to-protect = Ordner zum Schützen wählen
    .w-folder = Ordner
    .w-called = Name
    .w-kind = Art
    .w-choose-another-folder = Anderen Ordner wählen
    .w-where-snapshots-go = Wohin Stände gehen
    .w-each-snapshot-is-written-here-outside = Jeder Stand wird hier geschrieben, außerhalb des kopierten Ordners.
    .w-choose-where-snapshots-go = Wählen, wohin Stände gehen
    .w-used = Belegt
    .w-that-could-not-be-done = Das ließ sich nicht tun
    .w-back-up-now = Jetzt sichern

d-pvbigiav = Fenster
    .w-backups = Datensicherung
    .w-all-backups = Alle Sicherungen
    .w-this-device = Dieses Gerät
    .w-where-they-go = Wohin sie gehen
    .w-retention = Aufbewahrung
    .w-snapshots = Stände
    .w-failed = Fehlgeschlagen { $when }
    .w-stop = Anhalten
    .w-back-up-now = Jetzt sichern
    .w-look-again = Erneut nachsehen
    .w-work-under-way = Laufende Arbeit
    .w-something-needs-a-look = Etwas braucht einen Blick
    .w-dismiss = Ausblenden

d-sqdhwi4z = Arbeitszeile
    .w-backing-up = Sichere
    .w-verifying-the-snapshot = Prüfe den Stand
    .w-restoring-the-snapshot = Stelle den Stand wieder her
    .w-stop = Anhalten

d-anqvfopn = Verlauf
    .w-last-backup = Letzte Sicherung
    .w-next-run = Nächste Ausführung
    .w-snapshots = Stände
    .w-protected = Geschützt
    .w-need-a-look = Braucht einen Blick
    .w-newest-first = Neueste zuerst
    .w-oldest-first = Älteste zuerst
    .w-newest = Neuester { $when }
    .w-no-snapshots-yet = Noch keine Stände
    .w-back-up-the-folder-to-make = Sichere den Ordner, um den ersten Stand anzulegen.

d-xm44zffk = Standzeile
    .w-changed = { $size_text }, { $files_text } geändert

d-qrinzowj = Standblatt
    .w-restore = Wiederherstellen…
    .w-verify = Prüfen
    .w-taken = Art
    .w-how = Art
    .w-kept-as = Aufbewahrt als
    .w-size = Größe
    .w-changed = Geändert
    .w-state = Zustand
    .w-why-it-failed = Warum es fehlschlug
    .w-files = Dateien
    .w-the-snapshot-holds-no-files = Der Stand enthält keine Dateien
    .w-the-snapshot-s-files = Die Dateien des Stands
    .w-restore-into = Wiederherstellen nach { $restore_into }
    .w-what-the-folder-holds-is-replaced = Was der Ordner enthält, wird durch den Stand ersetzt.
    .w-the-folder-must-be-new-or = Der Ordner muss neu oder leer sein.
    .w-restore-here = Hierhin wiederherstellen
    .w-choose-another-folder = Anderen Ordner wählen
    .w-choose-the-folder-to-restore-into = Ordner zum Wiederherstellen wählen
    .w-replace-what-the-folder-holds = Ersetzen, was der Ordner enthält

d-xs435fkb = Dateizeile

d-nel5mdzq = Quellseite
    .w-folder = Ordner
    .w-called = Name
    .w-kind = Art
    .w-schedule = Zeitplan
    .w-state = Zustand
    .w-needs-a-look = Braucht einen Blick
    .w-fine = In Ordnung
    .w-choose-another-folder = Anderen Ordner wählen
    .w-its-snapshots = Seine Stände

d-6lykvoui = Zielseite
    .w-folder = Ordner
    .w-called = Name
    .w-used = Belegt
    .w-encrypted = Verschlüsselt
    .w-yes = Ja
    .w-no = Nein
    .w-state = Zustand
    .w-needs-a-look = Braucht einen Blick
    .w-fine = In Ordnung
    .w-choose-another-folder = Anderen Ordner wählen
    .w-its-snapshots = Seine Stände

d-76vcytvk = Aufbewahrungsseite
    .w-retention = Aufbewahrung
    .w-a-backup-keeps-this-many-snapshots = Eine Sicherung behält so viele Stände je Klasse, und den neuesten von allen.
    .w-kept = Aufbewahrt
    .w-hourly = stündlich
    .w-hourly-2 = Stündlich
    .w-daily = täglich
    .w-daily-2 = Täglich
    .w-weekly = wöchentlich
    .w-weekly-2 = Wöchentlich
    .w-monthly = monatlich
    .w-monthly-2 = Monatlich
    .w-kept-at-most = Höchstens { $policy_total } aufbewahrt
    .w-the-newest-snapshot-always-stays = Der neueste Stand bleibt immer

d-lxppnx67 = Aufbewahrungszeile
    .w-one-fewer = Einen { $label } weniger
    .w-one-more = Einen { $label } mehr

d-7rulekdp = Ordner einklappen
    .doc = Klappt einen Ordner aus dem Dateiverzeichnis des Stands ein oder wieder aus.

d-7rulekdp-s = Stand

d-7rulekdp-path = Pfad

d-7rulekdp-folded = Eingeklappt
