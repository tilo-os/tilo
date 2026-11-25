# Aufgaben auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-tasks = Aufgaben

d-so-tilo-tasks-keywords = aufgaben, tickets, problemverfolgung, kanban, board, to-do, aufgabenliste, projekte, agentenaufgaben

d-snj4onfb = Person
    .doc = Jemand, der an einem Projekt beteiligt ist, mit der Rolle dort: wem eine Aufgabe gegeben werden kann, ohne ihn erneut einzutippen.
    .noun = { $count ->
        [one] Person
       *[other] Personen
    }

d-ktw2jimm = Name

d-hgptkohm = Rolle

d-bfqi332m = Projekt
    .doc = Ein Projekt: Aufgaben, die auf ein Ergebnis hinarbeiten.
    .noun = { $count ->
        [one] Projekt
       *[other] Projekte
    }

d-sgnau7vd = Name

d-rsoasw3n = Art

d-c5pgzhqt = Notizen
    .doc = Wofür es da ist, und alles, was sich darüber zu merken lohnt.

d-mboq6kfe = Personen

d-bfqi332m-filter-kind = Art

d-7jh5u6dc = Wartezeit
    .doc = Worauf eine Aufgabe wartet, in wenigen Worten.
    .noun = { $count ->
        [one] Wartezeit
       *[other] Wartezeiten
    }

d-4fz6j3rg = Art

d-4ijm32x7 = Was
    .doc = Worauf gewartet wird: eine Antwort, ein Zeitpunkt, eine Ressource, die Teile.

d-dy6nui5s = Bis
    .doc = Der späteste Zeitpunkt, an dem das Warten endet, ob das Erwartete kam oder nicht.

d-osxm3ega = Ereignis
    .doc = Die Automatisierung, die das Warten vorzeitig beendet, nach ihrer Kennung.

d-7jh5u6dc-filter-kind = Art

d-7jh5u6dc-filter-until = Bis

d-p634b5uh = Angehalten
    .doc = Warum die Arbeit an einer Aufgabe aufhörte.
    .noun = { $count ->
        [one] angehalten
       *[other] angehalten
    }

d-ps5lbw2w = Grund

d-5yqkyqzr = Worte
    .doc = Die schlichte Zeile, die ihre Person liest.

d-kdugvhf2 = Um

d-p634b5uh-filter-why = Grund

d-p634b5uh-filter-at = Um

d-3bdn2bkx = Fortgesetzt
    .doc = Worauf die Arbeit an einer Aufgabe wartete und was sie weitermachen ließ.
    .noun = { $count ->
        [one] fortgesetzt
       *[other] fortgesetzt
    }

d-66a5tqcs = Ursache

d-ys7neba6 = Um

d-3bdn2bkx-filter-at = Um

d-pwfklw2l = Arbeit
    .doc = Was eine Aufgabe behält, während ein Agent über viele Sitzungen an ihr arbeitet: wo ihr Arbeitsmaterial liegt, worauf sie wartet, was sie kosten darf und wie lange gearbeitet wurde.
    .noun = { $count ->
        [one] Arbeit
       *[other] Arbeiten
    }

d-finhtl5k = Ordner
    .doc = Der Ordner des Arbeitsbereichs, in dem ihr Material liegt, nach seiner Adresse.

d-ckijp22f = Notiz
    .doc = Die Notiz, die ihre Sitzungen schreiben, nach ihrer Adresse.

d-3hituttb = Wartephasen
    .doc = Worauf sie wartet: nichts, solange sie auf nichts wartet.

d-fxyk36kk = Bis
    .doc = Der späteste Zeitpunkt, an dem ihr letztes Warten endet.

d-pcmbdaod = Seit
    .doc = Wann das Warten begann.

d-lkkui5te = Budget in Millionstel
    .doc = Was sie ausgeben darf, in einem Millionstel des Geldes des Arbeitsbereichs; nichts für kein Limit.

d-o5tuxt2b = Budget in ms
    .doc = Wie lange sie arbeiten darf, in Millisekunden; nichts für kein Limit.

d-byxiukp2 = Budget für
    .doc = Wessen Budget die Limits belasten.

d-4c4aw4xd = Ausgegeben in Millionstel
    .doc = Was ihre Sitzungen ausgegeben haben, in einem Millionstel des Geldes des Arbeitsbereichs.

d-virxpj23 = Gearbeitet in ms
    .doc = Wie lange ihre Sitzungen gearbeitet haben, in Millisekunden.

d-hztvc5dj = Sitzungen
    .doc = Wie viele Sitzungen an ihr gearbeitet haben.

d-nuikd6fz = Sitzung seit
    .doc = Wann die Sitzung begann, die jetzt an ihr arbeitet.

d-qublsm3d = Verloren
    .doc = Wie viele Sitzungen endeten, ohne fertig zu werden.

d-nax7z7kw = Angehalten
    .doc = Warum ihre Arbeit aufhörte, mit den Worten, die es sagten.

d-55waklcc = Fortgesetzt
    .doc = Was sie nach einem Warten weitermachen ließ.

d-huwemkpv = Teile gleichzeitig
    .doc = Wie viele ihrer Teile gleichzeitig in Arbeit sein dürfen.

d-pwfklw2l-filter-until = Bis

d-pwfklw2l-filter-since = Seit

d-pwfklw2l-filter-budget_micros = Budget in Millionstel

d-pwfklw2l-filter-budget_ms = Budget in ms

d-pwfklw2l-filter-budget_by = Budget für

d-pwfklw2l-filter-spent_micros = Ausgegeben in Millionstel

d-pwfklw2l-filter-worked_ms = Gearbeitet in ms

d-pwfklw2l-filter-sessions = Sitzungen

d-pwfklw2l-filter-session_since = Sitzung seit

d-pwfklw2l-filter-lost = Verloren

d-pwfklw2l-filter-stopped = Angehalten

d-pwfklw2l-filter-resumed = Fortgesetzt

d-pwfklw2l-filter-pieces_at_once = Teile gleichzeitig

d-2nkvzgb3 = Herkunft
    .doc = Wo es zuerst geschrieben wurde, als es aus einem anderen Tracker oder Dienst hierher kam: die Person dort, der Dienst, wo es dort steht und seine Referenz, damit eine Aufgabe oder ein Thread von anderswo sagt, woher er oder sie kam.
    .noun = { $count ->
        [one] Herkunft
       *[other] Herkünfte
    }

d-3saq3nij = Autor
    .doc = Wer es dort schrieb, unter dem Namen von dort.

d-duzjakqx = Kürzel
    .doc = Der Name, unter dem die Person dort bekannt war, wenn er abweicht.

d-g6jrgwt6 = Quelle
    .doc = Der Dienst, aus dem es kam.

d-gncjc3dz = Adresse
    .doc = Wo es dort zu lesen ist.

d-qg5qvqsi = Referenz
    .doc = Seine Referenz dort, etwa seine Nummer.

d-xuslp5pg = Erstellt
    .doc = Wann es dort zuerst geschrieben wurde.

d-no47vg4q = Aktualisiert
    .doc = Wann es sich dort zuletzt änderte.

d-2nkvzgb3-filter-created = Erstellt

d-2nkvzgb3-filter-updated = Aktualisiert

d-75ojxgs3 = Aufgabe
    .doc = Etwas zu tun: was, bis wann, von wem und in welchem Projekt.
    .noun = { $count ->
        [one] Aufgabe
       *[other] Aufgaben
    }

d-2y3yxgz4 = Nummer
    .doc = Ihre Nummer im Arbeitsbereich (`#12`), einmal vergeben, wenn sie entsteht.

d-oidkt4g5 = Titel

d-ftjnco6i = Notizen

d-2xssorsk = Status

d-fgvmlm5n = Erledigt
    .doc = Ob sie erledigt ist, was ihr Status sagt: das Kästchen einer Zeile ändert ein Merkzeichen, also steht das Merkzeichen, das der Status meint, daneben, und jede Aktion, die den Status schreibt, schreibt es mit.

d-g6kykjp2 = Fällig
    .doc = Der Tag, bis zu dem sie fällig ist, falls es einen gibt.

d-qadph346 = Um
    .doc = Der Zeitpunkt an diesem Tag, zu dem sie fällig ist, falls sie einen hat.

d-aeilucpg = Erinnern
    .doc = Wann wer sie erledigt an ihren Zeitpunkt erinnert wird, falls eine Erinnerung gesetzt ist.

d-2eyau2d6 = Zuständig
    .doc = Wer sie erledigt.

d-ptyivutv = Zugewiesen von
    .doc = Wer sie dem Agenten gab, der sie hat.

d-r5bd6ss3 = Zugewiesen am
    .doc = Wann sie diesem Agenten gegeben wurde.

d-tj32pwsz = Etiketten
    .doc = Die Wörter, nach denen Menschen sie über Projekte hinweg sortieren, etwa "Bug" oder "Besorgung".

d-fbioszhs = Projekt
    .doc = Das Projekt, zu dem sie gehört; keines, solange sie im Eingang wartet.

d-4sriymst = Priorität

d-nc5peol7 = Schätzung
    .doc = Wie viel Arbeit sie ist, in Punkten.

d-3pf74ifm = Übergeordnet
    .doc = Die Aufgabe, deren Teilaufgabe sie ist, beliebig tief; keine für eine eigene Aufgabe.

d-zjuuqsza = Anhänge
    .doc = Daran angehängte Dateien.

d-sqy7cbtd = Herkunft
    .doc = Wo es zuerst geschrieben wurde, als es aus einem anderen Tracker hierher kam.

d-o6ijslz7 = Adresse
    .doc = Die Seite, auf der es anderswo gelesen wird.

d-3uvt7tnr = Arbeit
    .doc = Was sie behält, während ein Agent an ihr arbeitet.

d-uxlestrl = Wartet auf
    .doc = Worauf sie wartet, in wenigen Worten: leer, solange sie auf nichts wartet.

d-m6raucmj = Wartet höchstens bis
    .doc = Der späteste Zeitpunkt, an dem ihr Warten endet; es endet, wenn er kommt.

d-vtgi2fsa = Warten endet mit
    .doc = Die Automatisierung, die ihr Warten vorzeitig beendet, wenn eine es tut.

d-75ojxgs3-filter-tasks = Aufgaben

d-75ojxgs3-filter-inbox = Eingang

d-75ojxgs3-filter-today = Heute

d-75ojxgs3-filter-upcoming = Anstehend

d-75ojxgs3-filter-todos = Zu erledigen

d-75ojxgs3-filter-done = Erledigt

d-75ojxgs3-filter-assigned_to_me = Mir zugewiesen

d-75ojxgs3-filter-nobody_yet = Noch niemand

d-75ojxgs3-filter-agent_work = Agentenarbeit

d-75ojxgs3-filter-waiting = Wartend

d-75ojxgs3-filter-due = Fällig

d-75ojxgs3-filter-project = Projekt

d-75ojxgs3-filter-priority = Priorität

d-75ojxgs3-filter-parent = Übergeordnet

d-75ojxgs3-filter-labels = Etiketten

d-75ojxgs3-filter-status = Status

d-75ojxgs3-filter-assignee = Zuständig

d-fjqi4yze = Aufgaben
    .doc = Die Aufgaben und Projekte des Arbeitsbereichs.
    .noun = { $count ->
        [one] Aufgabe
       *[other] Aufgaben
    }

d-lg5gedbh = Einträge
    .doc = Jede Aufgabe, in der Reihenfolge, in die Menschen sie gebracht haben.

d-mdhllpbo = Projekte

d-c7peh6l3 = Herkünfte
    .doc = Wo die Aufgaben und Threads hier zuerst geschrieben wurden: eine je Ort, aus dem sie kamen.

d-auinwnh3 = Nummernstand
    .doc = Die höchste bisher vergebene Nummer: die nächste Aufgabe bekommt die nächste.

d-fjqi4yze-filter-numbers = Nummernstand

d-q6rs4dka = Priorität
    .doc = Wie wichtig eine Aufgabe ist, das Dringendste zuletzt.

d-itg256eu = Keine Priorität

d-mq2uqfpl = Niedrig

d-iubwjt4e = Mittel

d-clxahlh2 = Hoch

d-d6avlyc5 = Dringend

d-lst5n5qw = Status
    .doc = Wo eine Aufgabe in ihrer Arbeit steht: die Spalten eines Boards, fest, damit jede Aufgabe des Arbeitsbereichs an derselben Stelle derselben Spalte steht.

d-gifhllm2 = Backlog

d-ecladcck = Todo

d-743ubdnw = In Arbeit

d-uoo2c3nt = In Prüfung

d-s2aii2do = Erledigt

d-m3y6yj4c = Abgebrochen

d-bknmpaiw = Art
    .doc = Welche Art Projekt das ist.

d-y4ia6lqx = Projekt

d-erfezq3t = Bereich

d-i4njiseg = Budget für
    .doc = Wessen Budget die Arbeit einer Aufgabe ausgibt.

d-k2qa6dfv = Die eigene Person

d-x5hbaxqb = Der Agent

d-awbl26xb = Art des Wartens
    .doc = Worauf ein Warten wartet: die Arten, die eine Sitzung eines Agenten unterscheidet.

d-gmuoiqeq = Ein Zeitpunkt

d-ce3ohtpj = Eine Antwort

d-wfyj2bwt = Eine Änderung

d-bv56uipx = Eine Antwort der Person

d-37r6d2ov = Antworten

d-xmhsnlsk = Die Teile

d-vcxpkpb6 = Grund des Anhaltens
    .doc = Warum die Arbeit an einer Aufgabe aufhörte.

d-zpiqqdxk = Fehlgeschlagen

d-rvil2upx = Kein Geld mehr

d-3xiu65sl = Keine Zeit mehr

d-jipsu7zw = Unterbrochen

d-odjzaby6 = Von einer Person gestoppt

d-rs3ubifz = Neue Aufgabe
    .doc = Legt eine Aufgabe im Eingang oder in `p` an, in der Liste `s` seines Boards: der Eintrag am Ende einer Liste füllt `title` mit den Worten, die sonst niemand dort aufnimmt, dazu den Tag, den Zeitpunkt und die Person, die er nennt, sodass „Vermieter fr 10 Uhr anrufen @Bo“ eine Aufgabe ist, fällig Freitag um zehn, für Bo.
    .w-is-not-in-this-workspace = { $value } ist nicht in diesem Arbeitsbereich

d-rs3ubifz-p = P

d-rs3ubifz-status = Status

d-rs3ubifz-due = Fällig

d-rs3ubifz-at = Wann

d-rs3ubifz-assignee = Zuständig

d-rs3ubifz-title = Titel

d-rs3ubifz-remind = Erinnern

d-2rpw5inw = Neue Aufgaben
    .doc = Fügt alle Aufgaben aus `titles` auf einmal hinzu, als eine Änderung: keine entsteht, wenn eine nicht entstehen kann (ein leerer Titel), also ist die Sammlung einer Person ganz oder gar nicht. Mit `done` sind sie schon erledigte Arbeit, festgehalten zum Aufbewahren: erledigt, wie Abhaken jede hinterlässt.
    .did = Aufgaben hinzugefügt
    .w-a-task-needs-a-title = Eine Aufgabe braucht einen Titel

d-2rpw5inw-titles = Titel

d-2rpw5inw-p = P

d-2rpw5inw-done = Erledigt

d-zjuy7y7f = Neues Projekt
    .doc = Beginnt ein Projekt.
    .w-new-project = Neues Projekt

d-cando5zh = Art
    .doc = Setzt, ob ein Projekt eines mit Ende oder eine laufende Verantwortung ist.
    .w-it-has-that-kind-already = Sie hat diese Art bereits

d-cando5zh-kind = Art

d-kjbukd2o = Umbenennen
    .doc = Benennt ein Projekt um.

d-kjbukd2o-name = Name

d-5bh4papz = Person hinzufügen
    .doc = Fügt jemanden hinzu, der an einem Projekt beteiligt ist, mit der Rolle dort.
    .w-name-the-person = Nenne die Person

d-5bh4papz-name = Name

d-5bh4papz-role = Rolle

d-yilfqr2i = Person entfernen
    .doc = Nimmt jemanden von einem Projekt.

d-yilfqr2i-person = Person

d-2xixz4sk = Projekt löschen
    .doc = Löscht ein Projekt und seine Abschnitte; seine Aufgaben gehen zurück in den Eingang.

d-pcn3xgni = Neue Aufgabe
    .doc = Fügt einem Projekt eine Aufgabe hinzu, am Ende der Liste `s` seines Boards (keine für die erste).

d-pcn3xgni-title = Titel

d-pcn3xgni-status = Status

d-bz2p62js = Umbenennen
    .doc = Legt fest, wie eine Aufgabe heißt.
    .w-a-task-needs-a-title = Eine Aufgabe braucht einen Titel

d-bz2p62js-title = Titel

d-berwb3de = Verschieben nach
    .doc = Verschiebt eine Aufgabe an eine Stelle in der Gliederung: ans Ende der Teilaufgaben von `parent` (die oberste Ebene für keine), oder dorthin, wo `before` steht.
    .w-a-task-cannot-go-inside-itself = Eine Aufgabe kann nicht in sich selbst wandern

d-berwb3de-parent = Übergeordnet

d-berwb3de-before = Davor

d-he6qkhh4 = Einrücken
    .doc = Verschiebt eine Aufgabe unter die Aufgabe, die die Ansicht direkt über ihr in ihrer Gruppe zeigt, wie es `Ctrl+]` tut.
    .w-nothing-over-it-to-move-under = Nichts darüber, worunter sie passt

d-he6qkhh4-above = Darüber

d-khy6tj7s = Ausrücken
    .doc = Verschiebt eine Aufgabe unter ihrer übergeordneten hervor, direkt hinter sie, wie es `Ctrl+[` in einer Gliederung tut.
    .w-it-is-at-the-top-level = Sie ist schon auf der obersten Ebene

d-o3lvuj36 = Status
    .doc = Stellt eine Aufgabe in eine Spalte des Boards, an die Stelle `at` unter den Aufgaben, die schon dort stehen: was das Ziehen einer Karte benennt. In der eigenen Spalte verschiebt sie das Ablegen nur unter deren Aufgaben.

d-o3lvuj36-status = Status

d-o3lvuj36-at = Um

d-zzrudiur = Verschieben nach
    .doc = Stellt alle gewählten Aufgaben auf einmal in eine Spalte (oder ein Projekt), als eine Änderung.
    .did = Aufgaben verschoben
    .w-choose-the-tasks-to-move = Wähle die zu verschiebenden Aufgaben

d-zzrudiur-chosen = Auswahl

d-zzrudiur-status = Status

d-zzrudiur-at = Um

d-vwgro4iv = Abschließen
    .doc = Hakt eine Aufgabe mit jeder Aufgabe unter ihr ab, oder öffnet sie wieder mit den Aufgaben, unter denen sie steht.
    .w-complete = Abschließen
    .w-reopen = Wieder öffnen

d-vwgro4iv-done = Erledigt

d-fykojijd = Teilaufgabe hinzufügen
    .doc = Legt eine Teilaufgabe unter einer Aufgabe an, in ihrem Projekt und Abschnitt, und öffnet eine erledigte: jetzt steht Arbeit darunter.
    .did = fügte eine Teilaufgabe hinzu

d-fykojijd-title = Titel

d-w2qapnpp = Leeren Schritt entfernen
    .doc = Nimmt den Teilschritt weg, den das Feld leer ließ: ein Feld, das einen begann und leer blieb, ist kein Schritt der Arbeit, also steht nichts Leeres unter der Aufgabe.
    .did = einen leeren Schritt entfernt

d-tmwdrpev = Heute
    .doc = Setzt den Tag, bis zu dem eine Aufgabe fällig ist, aus den Vorgaben oder einem Tag, den die Person wählt.
    .w-it-is-done-already = Sie ist schon erledigt

d-wclcgiyt = Morgen
    .doc = Macht eine Aufgabe morgen fällig.
    .w-it-is-done-already = Sie ist schon erledigt

d-tg5yjcju = Nächste Woche
    .doc = Macht eine Aufgabe zum Beginn der nächsten Woche fällig.
    .w-it-is-done-already = Sie ist schon erledigt

d-jqhwyztw = Tag wählen
    .doc = Macht eine Aufgabe an einem Tag fällig, den die Person wählt.

d-jqhwyztw-due = Fällig

d-dewj7mzg = Uhrzeit setzen
    .doc = Setzt die Uhrzeit, zu der eine Aufgabe fällig ist: der Tag, an dem sie steht, bleibt, und eine Aufgabe ohne Tag ist an dem Tag fällig, den der Zeitpunkt nennt.

d-dewj7mzg-at = Um

d-x5xnqddo = Kein Datum
    .doc = Nimmt den Tag, bis zu dem eine Aufgabe fällig ist, wieder weg: sie wartet undatiert im Eingang.
    .w-it-has-no-date = Sie hat kein Datum

d-3qq23ybf = Erinnern
    .doc = Setzt den Zeitpunkt, an den wer eine Aufgabe erledigt, an sie erinnert wird.

d-3qq23ybf-when = Wann

d-vjsjvx7u = Keine Erinnerung
    .doc = Nimmt die Erinnerung einer Aufgabe weg: an sie wird nicht mehr erinnert.
    .w-it-has-no-reminder = Sie hat keine Erinnerung

d-7vjyuy7c = Erinnern
    .doc = Sagt wer eine Aufgabe erledigt, oder wer sie angelegt hat, was sie ist und wann sie fällig ist: einmal, und nicht an eine schon erledigte Aufgabe.
    .w-due = Fällig { $d }

d-w43ij7uu = Keine Priorität
    .doc = Setzt, wie wichtig eine Aufgabe ist.
    .w-it-has-no-priority = Sie hat keine Priorität

d-pdc7ukxu = Niedrig
    .doc = Markiert eine Aufgabe als niedrig priorisiert.
    .w-it-is-low-already = Sie ist schon niedrig

d-cpcxkauo = Mittel
    .doc = Markiert eine Aufgabe als mittel priorisiert.
    .w-it-is-medium-already = Sie ist schon mittel

d-kglmmfa7 = Hoch
    .doc = Markiert eine Aufgabe als hoch priorisiert.
    .w-it-is-high-already = Sie ist schon hoch

d-lrdm4l3c = Dringend
    .doc = Markiert eine Aufgabe als dringend.
    .w-it-is-urgent-already = Sie ist schon dringend

d-xgmuq3oy = Schätzung
    .doc = Setzt die Punkte, die eine Aufgabe wert ist, oder keine.

d-xgmuq3oy-points = Punkte

d-laxsa3x6 = Projekt
    .doc = Legt eine Aufgabe in einem Projekt ab, das die Person wählt.

d-laxsa3x6-p = P

d-dshqxolf = Eingang
    .doc = Nimmt eine Aufgabe aus ihrem Projekt, zurück in den Eingang.
    .w-it-is-in-the-inbox-already = Sie ist schon im Eingang

d-c6y4cxdd = Zuweisen
    .doc = Gibt eine Aufgabe jemandem, oder niemandem.
    .w-the-pieces-in-work-already-fill = Die Teile in Arbeit füllen schon, was diese Aufgabe gleichzeitig arbeiten lässt
    .w-it-is-theirs-already = Sie gehört bereits dieser Person
    .w-a-task-is-a-member-s = Eine Aufgabe gehört einem Mitglied oder einem Kontakt, nicht beiden
    .w-is-not-in-this-workspace = { $value } ist nicht in diesem Arbeitsbereich

d-c6y4cxdd-who = Wer

d-jwva2bcv = Zuweisen
    .doc = Gibt alle gewählten Aufgaben einer Person (oder niemandem), als eine Änderung.
    .did = Aufgaben zugewiesen
    .w-choose-the-tasks-to-assign = Wähle die zuzuweisenden Aufgaben
    .w-is-not-in-this-workspace = { $value } ist nicht in diesem Arbeitsbereich

d-jwva2bcv-chosen = Auswahl

d-jwva2bcv-who = Wer

d-fs7nzreh = Etikett…
    .doc = Gibt einer Aufgabe ein Etikett.
    .w-name-the-label = Nenne das Etikett

d-fs7nzreh-name = Name

d-eocwppga = Etikett entfernen
    .doc = Nimmt einer Aufgabe ein Etikett ab.
    .w-it-has-no-such-label = Sie hat dieses Etikett nicht

d-eocwppga-name = Name

d-pk6rzcqw = Etikett…
    .doc = Gibt allen gewählten Aufgaben ein Etikett, als eine Änderung.
    .did = Aufgaben mit einem Etikett versehen
    .w-name-the-label = Nenne das Etikett

d-pk6rzcqw-chosen = Auswahl

d-pk6rzcqw-name = Name

d-3h5dpt4p = Datei anhängen
    .doc = Hängt eine Datei an eine Aufgabe.
    .did = eine Datei angehängt

d-3h5dpt4p-file = Datei

d-uqpc3g6w = Anhang entfernen
    .doc = Nimmt einer Aufgabe einen Anhang ab.

d-uqpc3g6w-attachment = Anhang

d-xrr33hkc = Dateien importieren
    .doc = Importiert Dateien als Aufgaben: eine Aufgabe je Datei, im Projekt `p`, wenn eines genannt ist, mit der Datei daran.
    .did = Dateien importiert
    .w-choose-the-files-to-import = Wähle die zu importierenden Dateien

d-xrr33hkc-files = Dateien

d-xrr33hkc-p = P

d-6tlw3ran = Herkunft hinzufügen
    .doc = Beginnt die Arbeit an einem Vorgang eines anderen Trackers: seine Herkunft, seine Seite dort und seine Nummer; eine Aufgabe derselben Herkunft ist schon hier, also entsteht beim erneuten Import keine.
    .did = genannt, woher sie kam

d-6tlw3ran-author = Autor

d-6tlw3ran-source = Quelle

d-6tlw3ran-handle = Kürzel

d-6tlw3ran-url = Adresse

d-6tlw3ran-reference = Referenz

d-6tlw3ran-created = Erstellt

d-6tlw3ran-updated = Aktualisiert

d-jfoiwgwq = Vorgang importieren
    .did = einen Vorgang importiert
    .w-an-imported-issue-needs-a-key = Ein importierter Vorgang braucht einen Schlüssel

d-jfoiwgwq-title = Titel

d-jfoiwgwq-key = Schlüssel

d-jfoiwgwq-status = Status

d-p4wi6ohi = Notiz schreiben
    .doc = Schreibt die Notiz einer Aufgabe in Notizen: eine Notiz wie die Aufgabe benannt, mit dem, was sie merkt, aus der Aufgabe gemacht, damit sie unter ihr steht.
    .did = eine Notiz geschrieben

d-3ncicl2v = Warten auf…
    .doc = Legt eine Aufgabe auf Warten, bis das Erwartete kommt oder spätestens bis `until`: wer sie macht, eine Person wie ein Agent, macht dann weiter.
    .did = auf Warten gesetzt
    .w-it-is-done-already = Sie ist schon erledigt
    .w-say-what-it-waits-for = Sage, worauf sie wartet
    .w-the-wait-ends-in-the-past = Das Ende der Wartezeit liegt in der Vergangenheit

d-3ncicl2v-what = Was

d-3ncicl2v-until = Bis

d-3ncicl2v-kind = Art

d-3ncicl2v-on = An

d-ab4v7pl5 = Nicht mehr warten
    .doc = Beendet jedes Warten einer Aufgabe: das Erwartete kam, was `cause` in wenigen Worten sagt, und wer sie macht, macht weiter.
    .did = weitergearbeitet
    .w-it-waits-for-nothing-and-none = Sie wartet auf nichts, und keine ihrer Sitzungen ging verloren

d-ab4v7pl5-cause = Ursache

d-tji6xijt = Arbeitsplatz
    .doc = Setzt, wo das Arbeitsmaterial einer Aufgabe liegt: der Ordner ihrer Arbeit und die Notiz, die ihre Sitzungen schreiben.
    .did = den Arbeitsplatz gesetzt

d-tji6xijt-folder = Ordner

d-tji6xijt-note = Notiz

d-l7p7vczl = Budget
    .doc = Setzt, was die Arbeit einer Aufgabe ausgeben darf und wie lange sie arbeiten darf, und wessen Budget sie belastet.
    .did = ein Budget gesetzt

d-l7p7vczl-micros = Millionstel

d-l7p7vczl-millis = Millisekunden

d-l7p7vczl-by = Für

d-bx72tblu = Teile gleichzeitig
    .doc = Wie viele Teile einer Aufgabe gleichzeitig in Arbeit sein dürfen.
    .did = die Teile gleichzeitig gesetzt

d-bx72tblu-pieces = Teile

d-6k5mm32z = Sitzung begonnen
    .doc = Sagt, dass eine Sitzung an einer Aufgabe begann: gezählt, und ihr Beginn festgehalten für die Dauer der Arbeit.
    .did = Arbeit begonnen
    .w-i-stopped-the-working-time-this = Ich habe angehalten: die Arbeitszeit, die diese Aufgabe bekam, ist aufgebraucht. Gib ihr mehr Zeit, damit ich weitermachen kann.
    .w-i-stopped-this-task-s-budget = Ich habe angehalten: das Budget dieser Aufgabe ist aufgebraucht. Erhöhe es, damit ich weitermachen kann.
    .w-i-stopped-my-work-on-this = Ich habe angehalten: meine Arbeit daran wurde zu oft hintereinander unterbrochen. Sag, dass ich weitermachen soll, und ich beginne wieder bei seinen Notizen.

d-lvgvolrt = Sitzung beendet
    .doc = Sagt, dass eine Sitzung an einer Aufgabe endete: was sie ausgab und wie lange sie arbeitete, wird der Aufgabe zugerechnet.
    .did = Arbeit beendet

d-lvgvolrt-spent_micros = Ausgegeben in Millionstel

d-lvgvolrt-worked_ms = Gearbeitet in ms

d-y5jf4p7n = Anhalten
    .doc = Sagt, dass die Arbeit an einer Aufgabe aufhörte, und warum, in den Worten, die ihre Person liest.
    .did = Arbeit angehalten

d-y5jf4p7n-why = Grund

d-y5jf4p7n-words = Worte

d-23y3zdgu = Duplizieren
    .doc = Kopiert eine Aufgabe, mit jeder Aufgabe unter ihr, direkt hinter die kopierte.

d-ojzyzzxu = Löschen
    .doc = Löscht eine Aufgabe und ihre Teilaufgaben.

d-rjpcv6es = Erledigte Aufgaben löschen
    .doc = Löscht jede erledigte Aufgabe und antwortet, wie viele gingen: ein erledigter Zweig geht ganz, und eine erledigte Aufgabe, unter der noch ein Schritt offen ist, bleibt.
    .w-nothing-is-finished-yet = Noch ist nichts erledigt

d-5ohbulhj = Teilen mit
    .doc = Gibt der Kopie `to`, was `from` in seinem Satz und seiner Sammlung hält: was nur eine Aktion ändern darf.
    .did = eine Aufgabe kopiert

d-5ohbulhj-from = Von

d-cfituto7 = fügt Aufgaben hinzu

d-vzdj67kf = fügt eine Aufgabe hinzu

d-r3lxwimd = setzt die Art

d-da66j22u = fügt jemanden hinzu

d-ym2iynlv = benennt um

d-mg3c777a = Verschieben nach~present

d-2vvqvs5e = setzt den Status mehrerer

d-qwizihx4 = Abhaken~present

d-mdzqe3gc = Heute fällig~present

d-rnagsir2 = Morgen fällig~present

d-fym3mxqc = Nächste Woche fällig~present

d-ohw2ks6t = Datum entfernen~present

d-g7kx77ng = Erinnerung entfernen~present

d-dpworviv = Priorität keine~present

d-vh3rraia = Priorität niedrig~present

d-ir7grgoz = Priorität mittel~present

d-g4wmwcnr = Priorität hoch~present

d-lvbdxuzj = Priorität dringend~present

d-day53wfn = Aus Projekt nehmen~present

d-r7b5neqa = weist zu

d-x3jqfzjh = weist mehreren zu

d-vtz6f5ah = gibt ein Etikett

d-szsksrsg = nimmt ein Etikett ab

d-m667p5oi = gibt Etiketten

d-2b46hb6u = importiert Dateien

d-tk5t43h6 = importiert einen Vorgang

d-fnc6zwh7 = wartet

d-pgzbwzsm = beginnt eine Sitzung

d-tpvjqy5m = Erledigte löschen~present

d-i2mhsb3h = Startseite
    .w-new-task-labelled = Neue Aufgabe mit Etikett { $label }
    .w-new-task-in = Neue Aufgabe in { $name }
    .w-new-task-for-today = Neue Aufgabe für heute
    .w-new-task = Neue Aufgabe
    .w-tasks = Aufgaben
    .w-projects = Projekte
    .w-labels = Etiketten
    .w-no-tasks-here = Keine Aufgaben hier
    .w-what-to-do = Was zu tun ist
    .w-from = Aus { $source } { $r }
    .w-from-2 = Aus { $source }
    .w-waiting-for-until = Wartet auf { $waiting_for }, bis { $u }
    .w-waiting-for = Wartet auf { $waiting_for }
    .w-stop-waiting = Nicht mehr warten
    .w-nobody = Niemand
    .w-anything-worth-remembering-about-it = Alles, was sich darüber zu merken lohnt.
    .w-inbox = Eingang
    .w-points = { $e } Punkte
    .w-estimate = Schätzung
    .w-no-estimate = Keine Schätzung
    .w-remind = Erinnern
    .w-reminder-set = Erinnerung gesetzt
    .w-remind-me = Erinnere mich …
    .w-no-reminder = Keine Erinnerung
    .w-related = Verbunden
    .w-work = Arbeit
    .w-sessions-min = { $sessions } Sitzungen, { $minutes } Min.
    .w-of-budget = von { $budget } Budget
    .w-waiting-for-until-2 = Wartet auf { $what }, bis { $u }
    .w-waiting-for-2 = Wartet auf { $what }
    .w-subtasks = Teilschritte
    .w-no-subtasks-yet = Noch keine Teilschritte
    .w-add-a-subtask = Teilschritt hinzufügen

d-mikudv5u = Board
    .w-tasks = Aufgaben
    .w-new-task = Neue Aufgabe

d-q4eq7oqv = Zu erledigen
    .w-new-task = Neue Aufgabe
    .w-nothing-to-do = Nichts zu tun

d-doqleehr = Aufgabenzeile
    .w-open-as-page = Als Seite öffnen

d-7froalwb = Projektliste
    .w-new-task-in = Neue Aufgabe in { $name }
    .w-no-tasks-yet = Noch keine Aufgaben

d-gu3twlv2 = Projektboard
    .w-new-task = Neue Aufgabe

d-hc5ndp4x = Aufgabenkarte
    .w-move-to = Verschieben nach { $name }

d-w63ba6ad = Aufgabenzeile

d-rj4l44km = Aufgabenzusammenfassung

d-ypbiaiq3 = Zeile der Aufgabengliederung

d-mtmufg3w = Aufgabenseite
    .w-what-to-do = Was zu tun ist
    .w-from = Aus { $source } { $r }
    .w-from-2 = Aus { $source }
    .w-waiting-for-until = Wartet auf { $waiting_for }, bis { $u }
    .w-waiting-for = Wartet auf { $waiting_for }
    .w-stop-waiting = Nicht mehr warten
    .w-anything-worth-remembering-about-it = Alles, was sich darüber zu merken lohnt.
    .w-inbox = Eingang
    .w-points = { $e } Punkte
    .w-estimate = Schätzung
    .w-no-estimate = Keine Schätzung
    .w-remind = Erinnern
    .w-reminder-set = Erinnerung gesetzt
    .w-remind-me = Erinnere mich …
    .w-no-reminder = Keine Erinnerung
    .w-work = Arbeit
    .w-sessions-min = { $sessions } Sitzungen, { $minutes } Min.
    .w-of-budget = von { $budget } Budget
    .w-subtasks = Teilschritte
    .w-no-subtasks-yet = Noch keine Teilschritte
    .w-add-a-subtask = Teilschritt hinzufügen
    .w-nobody = Niemand
    .w-related = Verbunden
    .w-waiting-for-until-2 = Wartet auf { $what }, bis { $u }
    .w-waiting-for-2 = Wartet auf { $what }

d-fxb6wc2q = Projektseite
    .w-what-the-work-goes-towards = Worauf die Arbeit zielt
    .w-what-it-is-for = Wofür es gut ist.
    .w-people = Personen
    .w-add-someone = Jemanden hinzufügen
    .w-remove = Entfernen

d-j3fym73f = Kontakt
    .doc = Wer es tut, wenn die Person kein Mitglied ist: jemand, mit dem der Arbeitsbereich in Kontakt steht, aus seinem Adressbuch.

d-5ltuj6t2 = Verbunden
    .doc = Die Aufgaben, mit denen diese zusammenhängt, und wie: wo die Verbindungen zwischen Aufgaben des Arbeitsbereichs liegen.

d-ry4nfmba = Verbunden
    .doc = Eine Aufgabe, mit der eine andere zusammenhängt, und wie: `blocks`, `blocked by`, `duplicate of` und so weiter.
    .noun = { $count ->
        [one] Verbindung
       *[other] Verbindungen
    }

d-h62xalud = Aufgabe
    .doc = Die Aufgabe, mit der sie zusammenhängt.

d-27gmtaed = Wie
    .doc = Wie sie zusammenhängen, in den Worten des Dienstes, wenn es aus einem kam.

d-ry4nfmba-filter-task = Aufgabe

d-rs3ubifz-notes = Notizen

d-zjuy7y7f-notes = Notizen

d-c6y4cxdd-contact = Kontakt

d-al6y4s2g = Verbinden mit…
    .doc = Verbindet eine Aufgabe mit einer anderen und sagt, wie: die eigene Verbindung des Arbeitsbereichs zwischen zwei Aufgaben, die auch die Seite der verbundenen Aufgabe zeichnet.
    .did = hat eine Aufgabe verbunden
    .w-a-task-does-not-relate-to = Eine Aufgabe hängt nicht mit sich selbst zusammen
    .w-they-are-related-already = Sie hängen schon zusammen

d-al6y4s2g-other = Andere

d-al6y4s2g-how = Wie

d-bczoku6o = Verbindung lösen
    .doc = Löst eine Verbindung von einer Aufgabe.
    .w-it-has-no-such-relation = Sie hat keine solche Verbindung

d-bczoku6o-relation = Verbindung

d-7kv3m2xq = Anhang öffnen
    .doc = Öffnet den Anhang `attachment` in der App, die seine Art öffnet: die Datei einer Aufgabe erreicht diese App als derselbe Inhalt, den beide lesen, nie als ein Pfad, den keine hat.

d-7kv3m2xq-attachment = Anhang

d-s72maqeh = Verbinden~present

d-5kogwdim = Entbinden~present
