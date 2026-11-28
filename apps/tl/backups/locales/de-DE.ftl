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

d-cflircwr = Quellzeit
    .doc = Die Tageszeit, zu der sie sich sichert, als `18:00`; leer, wenn sie von Hand läuft.

d-grbmwjgb = Quellwochentag
    .doc = Der Tag, an dem ein wöchentlicher Zeitplan läuft.

d-5wqonrze = Eingetippte Zeit
    .doc = Was die Person als Zeit des Zeitplans getippt hat, bevor sie verwendet wird.

d-62wijgsn = Quellname
    .doc = Der Name des Quellordners.

d-x6ca2mpx = Quellherkunft

d-gfsjtev7 = Quellrhythmus

d-3wbouwis = Quelle gestört

d-ymfvuwvo = Nächste Ausführung
    .doc = Wann die nächste Sicherung läuft, gezählt ab 1970; null, wenn nichts sie plant.

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

d-ylkcsrzf = Zeitplanauftrag
    .doc = Was die Engine behalten soll: wann die Quelle sich sichert.
    .noun = { $count ->
        [one] Zeitplanauftrag
       *[other] Zeitplanaufträge
    }
        [one] schedule ask
       *[other] schedule asks
    }

d-gbbg7sps = Quelle

d-tezmcqsr = Ziel

d-qxyq3nbt = Zeitplanart
    .doc = `on_demand`, `hourly`, `daily` oder `weekly`.

d-jl5fmocj = Zeitplanzeit
    .doc = Die Tageszeit als `18:00`; leer für eine Quelle, die von Hand läuft.

d-xs3bnmyr = Zeitplanwochentag
    .doc = Der Tag, an dem ein wöchentlicher Zeitplan läuft, Montag als null.

d-ylkcsrzf-filter-schedule_weekday = Zeitplanwochentag

d-xzbqloor = Arbeitsdetails
    .doc = Was das Gerät über eine laufende Arbeit sagt und wenn sie endet.
    .noun = { $count ->
        [one] Arbeitsdetail
       *[other] Arbeitsdetails
    }

d-m7xwwoxh = Fortschritt

d-lwahnmcm = Antwort

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

d-v5ylljvo = Wiederherstellen
    .doc = Stellt den Stand in dem Ordner wieder her, den die Person wählte.
    .w-choose-a-folder-to-restore-into = Wähle einen Ordner zum Wiederherstellen
    .w-one-piece-of-work-is-running = Es läuft bereits eine Arbeit

d-v5ylljvo-s = S

d-pz4zxkew = Arbeit anhalten
    .doc = Hält die laufende Arbeit an; was sie schrieb, wird entfernt.

d-ijjf4th6 = Arbeit meldet Fortschritt
    .doc = Die Arbeit ist weitergegangen: wie weit sie gekommen ist, in Dateien und Bytes.

d-ijjf4th6-j = J

d-vxnmsby4 = Arbeit beendet
    .doc = Die Arbeit ist beendet: das Repository, wie es jetzt steht, was eine Prüfung gefunden hat, oder warum sie fehlgeschlagen ist.

d-vxnmsby4-kind = Art

d-vxnmsby4-j = J

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

d-pqix6wmj = Zeitplan setzen
    .doc = Legt fest, wann die Quelle sich sichert: Die Engine behält es und antwortet mit dem Repository, sodass Fenster und Agent denselben Zeitplan lesen.
    .w-choose-a-folder-to-protect-first = Wähle zuerst einen Ordner zum Schützen
    .w-choose-where-snapshots-go-first = Wähle zuerst, wohin die Momentaufnahmen gehen

d-pqix6wmj-kind = Art

d-pqix6wmj-at = Zeit

d-pqix6wmj-weekday = Wochentag

d-x5h6br5g = Ausblenden
    .doc = Blendet die Zeile unter der Liste aus.

d-w4peltry = Quelle setzen
    .doc = Schützt `path` auf diesem Gerät, wenn er außerhalb liegt, wohin die Momentaufnahmen geschrieben werden.

d-w4peltry-path = Pfad

d-f4hzpl6v = Ziel setzen
    .doc = Schreibt Momentaufnahmen nach `path`, wenn der Ordner außerhalb des geschützten liegt.

d-f4hzpl6v-path = Pfad

d-my75cr27 = Wiederherstellungsziel setzen
    .doc = Spielt die Momentaufnahme nach `path` zurück.

d-my75cr27-path = Pfad

d-e7w6p5gh = Arbeit beginnt
    .doc = Leert, was die letzte Arbeit sagte, wenn eine neue beginnt.

d-ifgic4vt = Arbeit starten
    .doc = Startet eine Sicherung oder Prüfung und merkt sich den Auftrag, den das Gerät gab.

d-ifgic4vt-kind = Art

d-ifgic4vt-ask = Auftrag

d-vq7l4nar = Stand schließen

d-apgumkkc = Ersetzen erlauben

d-7rulekdp = Ordner einklappen
    .doc = Klappt einen Ordner aus dem Dateiverzeichnis des Stands ein oder wieder aus.

d-7rulekdp-s = Stand

d-7rulekdp-path = Pfad

d-7rulekdp-folded = Eingeklappt

d-42zsjizr = Größe als Text
    .w-b = { $bytes } B
    .w-kb = { $tenths } kB
    .w-mb = { $tenths } MB
    .w-gb = { $tenths } GB
    .w-tb = { $tenths } TB

d-y33jk65h = Dateien als Text
    .w-1-file = 1 Datei
    .w-files = { $n } Dateien

d-fod4tqpx = Aufnahmewörter
    .w-automatic = Automatisch
    .w-manual = Manuell
    .w-unknown = Unbekannt

d-7dqa36tk = Aufbewahrungswörter
    .w-hourly = Stündlich
    .w-daily = Täglich
    .w-weekly = Wöchentlich
    .w-monthly = Monatlich
    .w-until-removed = Bis entfernt
    .w-unknown = Unbekannt

d-ho6s7tjb = Gesundheitswörter
    .w-verifying = Wird geprüft
    .w-failed = Fehlgeschlagen
    .w-complete = Vollständig

d-tnno3myx = Zeitplanwörter
    .w-when-you-back-up = Wenn du sicherst
    .w-every-hour-on-the-hour = Jede Stunde zur vollen Stunde
    .w-every-hour-at-past = Jede Stunde um { $minute_of } nach
    .w-every-day-at = Jeden Tag um { $source_at }
    .w-every-at = Jeden { $source_weekday } um { $source_at }

d-v7fwyqef = Herkunftswörter
    .w-home-folder = Home-Ordner
    .w-system-files = Systemdateien
    .w-workspace = Arbeitsbereich
    .w-custom-folder = Eigener Ordner

d-mskukut2 = Gesundheitszeile
    .w-no-failed-backups = Keine fehlgeschlagenen Sicherungen
    .w-one-backup-needs-attention = Eine Sicherung braucht Aufmerksamkeit
    .w-backups-need-attention = { $failed } Sicherungen brauchen Aufmerksamkeit

d-tgaw76i6 = Entfernungszeile
    .w-nothing-would-be-removed-the-next = Nichts würde entfernt: Die nächste Sicherung behält jede Momentaufnahme.
    .w-the-next-backup-would-remove-one = Die nächste Sicherung würde eine Momentaufnahme entfernen, aufgenommen { $when }.
    .w-the-next-backup-would-remove-snapshots = Die nächste Sicherung würde { $len } Momentaufnahmen entfernen: { $join }.

d-iadmcosj = Letzte Sicherung
    .w-never = Nie

d-rnz74k4z = Zeile der nächsten Ausführung
    .w-by-hand = Von Hand
    .w-not-known = Nicht bekannt

d-4qyas4qh = Belegungszeile
    .w-used = { $size_text } belegt
    .w-of = { $size_text } von { $size_text2 }

d-ucmdxqos = Seitentitel
    .w-this-device = Dieses Gerät
    .w-destination = Ziel
    .w-retention = Aufbewahrung
    .w-all-backups = Alle Sicherungen

d-t5f6lf3u = Jetzt gesichert

d-ngp7g7pa = Geprüft

d-s2kwexka = Wiederherstellen angefragt

d-4vnqjv4m = Wiederhergestellt

d-gaktgwrl = Zeitplan setzen~present

d-yeswuxpe = Fenster

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
    .w-destination = Ziel
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
    .w-stored-on = Gespeichert auf
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
    .w-the-whole-snapshot-is-restored-into = Die ganze Momentaufnahme wird in einen neuen oder leeren Ordner deiner Wahl zurückgespielt, und jede Datei wird geprüft. Nichts, was schon existiert, wird ersetzt.
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
    .w-when-it-backs-up = Wann sie sich sichert
    .w-when-you-back-up = Wenn du sicherst
    .w-every-hour = Jede Stunde
    .w-every-day = Jeden Tag
    .w-every-week = Jede Woche
    .w-time = Zeit
    .w-use-this-time = Diese Zeit verwenden
    .w-its-snapshots = Seine Stände

d-6lykvoui = Zielseite
    .w-folder = Ordner
    .w-called = Name
    .w-used = Belegt
    .w-capacity = Kapazität
    .w-free = Frei
    .w-encrypted = Verschlüsselt
    .w-yes = Ja
    .w-no = Nein
    .w-state = Zustand
    .w-needs-a-look = Braucht einen Blick
    .w-fine = In Ordnung
    .w-choose-another-folder = Anderen Ordner wählen
    .w-its-snapshots = Seine Stände

d-76vcytvk = Aufbewahrungsseite
    .w-snapshots-to-keep = Aufzubewahrende Momentaufnahmen
    .w-hourly = stündlich
    .w-hourly-snapshots-to-keep = Stündliche Momentaufnahmen aufbewahren
    .w-daily = täglich
    .w-daily-snapshots-to-keep = Tägliche Momentaufnahmen aufbewahren
    .w-weekly = wöchentlich
    .w-weekly-snapshots-to-keep = Wöchentliche Momentaufnahmen aufbewahren
    .w-monthly = monatlich
    .w-monthly-snapshots-to-keep = Monatliche Momentaufnahmen aufbewahren
    .w-each-source-keeps-at-most-snapshots = Jede Quelle behält höchstens { $policy_total } Momentaufnahmen, plus die bis zum Entfernen aufbewahrten

d-lxppnx67 = Aufbewahrungszeile
    .w-one-fewer = Einen { $label } weniger
    .w-one-more = Einen { $label } mehr
