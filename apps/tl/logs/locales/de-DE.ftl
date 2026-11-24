# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-logs = Logs

d-so-tilo-logs-keywords = protokolle, journal, journalctl, systemd, fehler, warnungen, diagnostik, logs

d-ydaofznc = Logs
    .doc = Der gemeinsame Journalfilter: was der Arbeitsbereich vom Journal dieses Geräts zeigt.
    .noun = { $count ->
        [one] Protokoll
       *[other] Protokolle
    }

d-6e5izmwp = Schweregrad
    .doc = Der am wenigsten schwere gezeigte Eintrag.

d-lq27tlwx = Unit
    .doc = Eine Unit oder leer für jede Unit.

d-vzywk5iy = Start
    .doc = Die ID eines Starts oder leer für jeden Start.

d-yuygrdvc = Suche
    .doc = Wörter, die in einer Meldung, einer Unit oder einem Programm gesucht werden.

d-rumvop3c = Folgen
    .doc = Ob das Journal dieses Geräts live verfolgt wird.

d-ydaofznc-filter-following = Folgen

d-hitfexjm = Exporteintrag
    .doc = Ein Eintrag, wie ihn der Export schreibt.
    .noun = { $count ->
        [one] Exporteintrag
       *[other] Exporteinträge
    }

d-37kvzek7 = Echtzeit in µs

d-v3wlyikw = Unit

d-ur4aajno = Kennung

d-wsg4pkbu = Priorität

d-cd6taqqw = Meldung

d-xkxlpyrt = PID

d-5wwthj2f = Rechnername

d-odiofhx6 = Start-ID

d-3ra7d3km = Transport

d-djjluyru = Extras

d-hitfexjm-filter-realtime_us = Echtzeit in µs

d-hitfexjm-filter-priority = Priorität

d-hitfexjm-filter-pid = PID

d-siu4bjad = Exportfeld
    .noun = { $count ->
        [one] Exportfeld
       *[other] Exportfelder
    }

d-mj25pae6 = Name

d-pnqueidz = Wert

d-o7nl26fl = Schweregrad
    .doc = Der am wenigsten schwere Eintrag, den das Fenster zeigt.

d-cjghwldx = Nur Notfälle
    .doc = Nur Notfälle.

d-rtsnau5j = Alarme und schwerer
    .doc = Alarme und schwerer.

d-yqzshsup = Kritisch und schwerer
    .doc = Kritisch und schwerer.

d-g4l2hdej = Fehler und schwerer
    .doc = Fehler und schwerer.

d-lnknco5l = Warnungen und schwerer
    .doc = Warnungen und schwerer.

d-77mgxiik = Hinweise und schwerer
    .doc = Hinweise und schwerer.

d-e3vdpsxj = Informationen und schwerer
    .doc = Informationen und schwerer.

d-hjvfczr3 = Jeder Eintrag, auch Debug
    .doc = Jeder Eintrag, auch Debug.

d-h7ux6zuj = Export erstellt
    .doc = Die Einträge, die ein Export schreibt, ein JSON-Objekt je Zeile.

d-fslvwdeu = Schweregrad setzen
    .doc = Zeigt Einträge ab dem Schweregrad `value`.

d-fslvwdeu-value = Wert

d-qfveahvf = Unit setzen
    .doc = Zeigt die Einträge einer Unit oder, bei leerem `value`, jeder Unit.

d-qfveahvf-value = Wert

d-e24v4l2c = Start setzen
    .doc = Zeigt die Einträge eines Starts oder, bei leerem `value`, jedes Starts.

d-e24v4l2c-value = Wert

d-5rvl6tfm = Suche setzen
    .doc = Engt die Einträge auf die ein, die `value` enthalten.

d-5rvl6tfm-value = Wert

d-u6of4uob = Filter zurücksetzen
    .doc = Setzt jeden Journalfilter zurück.

d-ac3g3mvu = Journal aktualisieren
    .doc = Liest die neuesten Einträge des Journals dieses Geräts.

d-equcewsy = Folgen
    .doc = Verfolgt neue Einträge, sobald sie kommen, oder hört damit auf.

d-equcewsy-on = An

d-liurooe5 = Nachlesen
    .doc = Liest die Einträge, die seit dem letzten geschrieben wurden, während es folgt.

d-v3wcvt43 = Eintrag kopieren
    .doc = Kopiert einen Eintrag mit Zeit, Unit, Programm und Meldung.

d-v3wcvt43-entry = Eintrag

d-jnxcvadn = Meldung kopieren
    .doc = Kopiert die Meldung eines Eintrags.

d-jnxcvadn-entry = Eintrag

d-xwaqirn3 = Schweregradworte
    .w-emergency = Notfall
    .w-alert = Alarm
    .w-critical = Kritisch
    .w-error = Fehler
    .w-warning = Warnung
    .w-notice = Hinweis
    .w-info = Info
    .w-debug = Debug

d-yij3luub = Fenster
    .w-logs = Logs
    .w-filter = Filter
    .w-emerg = emerg
    .w-alert = alert
    .w-crit = crit
    .w-err = err
    .w-warning = warning
    .w-notice = notice
    .w-info = info
    .w-debug = debug
    .w-severity = Schweregrad
    .w-unit = Unit
    .w-boot = Start
    .w-clear-filters = Filter zurücksetzen
    .w-following-the-journal = Dem Journal wird gefolgt
    .w-reading-the-journal = Das Journal wird gelesen…
    .w-follow-live = Live folgen
    .w-refresh-journal = Journal aktualisieren
    .w-export-filtered-logs = Gefilterte Logs exportieren
    .w-no-records-match = Keine Einträge passen
    .w-the-filter-hides-every-record-this = Der Filter verbirgt jeden Eintrag dieses Journals.
    .w-records-of = { $len } records of { $len2 }
    .w-show-this-record = Diesen Eintrag zeigen
    .w-this-record-is-already-shown = Dieser Eintrag wird bereits gezeigt
    .w-no-record-selected = Kein Eintrag ausgewählt
    .w-choose-a-record-to-see-its = Wähle einen Eintrag, um seine Felder zu sehen.

d-p3un3cpt = Nicht verfügbar
    .w-the-journal-is-not-available = Das Journal ist nicht verfügbar
    .w-this-computer-s-system-journal-could = Das Systemjournal dieses Rechners konnte nicht gelesen werden.
    .w-try-again = Erneut versuchen

d-cxvsq2r6 = Leeres Journal
    .w-no-records = Keine Einträge
    .w-this-device-s-journal-holds-no = Das Journal dieses Geräts enthält noch keine Einträge.
    .w-read-the-journal = Das Journal lesen

d-wfe6eitp = Eintragsdetail
    .w-none = Keine
    .w-record = Eintrag
    .w-time = Zeit
    .w-severity = Schweregrad
    .w-unit = Unit
    .w-program = Programm
    .w-process-id = Prozess-ID
    .w-host = Rechner
    .w-boot-id = Start-ID
    .w-transport = Transport
    .w-fields = Felder
    .w-copy-message = Meldung kopieren
    .w-copy-entry = Eintrag kopieren
