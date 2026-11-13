# Aufgaben auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-lang-tasks = Aufgaben

d-75ojxgs3 = Aufgabe
    .doc = Etwas zu tun: was, bis wann, von wem und in welchem Projekt.
    .noun = { $count ->
        [one] Aufgabe
       *[other] Aufgaben
    }

d-oidkt4g5 = Titel

d-fgvmlm5n = Erledigt

d-g6kykjp2 = Fällig
    .doc = Der Tag, bis zu dem sie fällig ist, falls es einen gibt.

d-qadph346 = Um
    .doc = Der Zeitpunkt an diesem Tag, zu dem sie fällig ist, falls sie einen hat.

d-aeilucpg = Erinnern
    .doc = Wann wer sie erledigt an ihren Zeitpunkt erinnert wird, falls eine Erinnerung gesetzt ist.

d-ftjnco6i = Notizen
    .doc = Alles, was sich über sie zu merken lohnt.

d-2eyau2d6 = Zuständig
    .doc = Wer sie erledigt.

d-fbioszhs = Projekt
    .doc = Das Projekt, zu dem sie gehört; keines, solange sie im Eingang wartet.

d-v56iknh2 = Abschnitt
    .doc = Die Spur im Board ihres Projekts, in der sie steht; keine für die erste Spur.

d-4sriymst = Priorität

d-3pf74ifm = Übergeordnet
    .doc = Die Aufgabe, deren Teilaufgabe sie ist, beliebig tief; keine für eine eigene Aufgabe.

d-75ojxgs3-filter-inbox = Eingang

d-75ojxgs3-filter-today = Heute

d-75ojxgs3-filter-upcoming = Anstehend

d-75ojxgs3-filter-done = Erledigt

d-75ojxgs3-filter-assigned_to_me = Mir zugewiesen

d-75ojxgs3-filter-due = Fällig

d-75ojxgs3-filter-project = Projekt

d-75ojxgs3-filter-priority = Priorität

d-75ojxgs3-filter-parent = Übergeordnet

d-75ojxgs3-filter-assignee = Zuständig

d-bfqi332m = Projekt
    .doc = Ein Projekt: Aufgaben, die auf ein Ergebnis hinarbeiten.
    .noun = { $count ->
        [one] Projekt
       *[other] Projekte
    }

d-sgnau7vd = Name

d-c5pgzhqt = Notizen
    .doc = Wofür es da ist, und alles, was sich darüber zu merken lohnt.

d-5wdrqcpl = Abschnitt
    .doc = Ein Abschnitt eines Projekts: eine Spur seines Boards, etwa „In Arbeit“.
    .noun = { $count ->
        [one] Abschnitt
       *[other] Abschnitte
    }

d-yfd266wj = Name

d-ohbjjsu7 = Projekt
    .doc = Das Projekt, zu dem der Abschnitt gehört.

d-5wdrqcpl-filter-project = Projekt

d-fjqi4yze = Aufgaben
    .doc = Die Aufgaben und Projekte des Arbeitsbereichs.
    .noun = { $count ->
        [one] Aufgabe
       *[other] Aufgaben
    }

d-lg5gedbh = Einträge
    .doc = Jede Aufgabe, in der Reihenfolge, in die Menschen sie gebracht haben.

d-mdhllpbo = Projekte

d-nk5542m7 = Abschnitte
    .doc = Die Abschnitte jedes Projekts, in der Reihenfolge, in der sein Board sie zeigt.

d-q6rs4dka = Priorität
    .doc = Wie wichtig eine Aufgabe ist, das Dringendste zuletzt.

d-itg256eu = Keine Priorität

d-mq2uqfpl = Niedrig

d-iubwjt4e = Mittel

d-clxahlh2 = Hoch

d-d6avlyc5 = Dringend

d-rs3ubifz = Neue Aufgabe
    .doc = Legt eine Aufgabe im Eingang oder in `p` an, in der Spur `s` seines Boards: der Eintrag am Ende einer Liste füllt `title` mit den Worten, die sonst niemand dort aufnimmt, dazu den Tag, den Zeitpunkt und die Person, die er nennt, sodass „Vermieter fr 10 Uhr anrufen @Bo“ eine Aufgabe ist, fällig Freitag um zehn, für Bo.

d-zjuy7y7f = Neues Projekt
    .doc = Beginnt ein Projekt.
    .w-new-project = Neues Projekt

d-vwgro4iv = Abschließen
    .doc = Hakt eine Aufgabe mit jeder Aufgabe unter ihr ab, oder öffnet sie wieder mit den Aufgaben, unter denen sie steht.
    .w-complete = Abschließen
    .w-reopen = Wieder öffnen

d-fykojijd = Teilaufgabe hinzufügen
    .doc = Legt eine Teilaufgabe unter einer Aufgabe an, in ihrem Projekt und Abschnitt, und öffnet eine erledigte: jetzt steht Arbeit darunter.
    .did = fügte eine Teilaufgabe hinzu

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

d-x5xnqddo = Kein Datum
    .doc = Nimmt den Tag, bis zu dem eine Aufgabe fällig ist, wieder weg: sie wartet undatiert im Eingang.
    .w-it-has-no-date = Sie hat kein Datum

d-3qq23ybf = Erinnern
    .doc = Setzt den Zeitpunkt, an den wer eine Aufgabe erledigt, an sie erinnert wird.

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

d-laxsa3x6 = Projekt
    .doc = Legt eine Aufgabe in einem Projekt ab, das die Person wählt.

d-dshqxolf = Eingang
    .doc = Nimmt eine Aufgabe aus ihrem Projekt, zurück in den Eingang.
    .w-it-is-in-the-inbox-already = Sie ist schon im Eingang

d-c6y4cxdd = Zuweisen
    .doc = Gibt eine Aufgabe jemandem, oder niemandem.

d-berwb3de = Verschieben nach
    .doc = Verschiebt eine Aufgabe an eine Stelle in der Gliederung: ans Ende der Teilaufgaben von `parent` (die oberste Ebene für keine), oder dorthin, wo `before` steht.
    .w-a-task-cannot-go-inside-itself = Eine Aufgabe kann nicht in sich selbst wandern

d-he6qkhh4 = Einrücken
    .doc = Verschiebt eine Aufgabe unter die Aufgabe, die die Ansicht direkt über ihr in ihrer Gruppe zeigt, wie es `Ctrl+]` tut.
    .w-nothing-over-it-to-move-under = Nichts über ihr, worunter sie wandern könnte

d-khy6tj7s = Ausrücken
    .doc = Verschiebt eine Aufgabe unter ihrer übergeordneten hervor, direkt hinter sie, wie es `Ctrl+[` in einer Gliederung tut.
    .w-it-is-at-the-top-level = Sie ist schon auf der obersten Ebene

d-23y3zdgu = Duplizieren
    .doc = Kopiert eine Aufgabe, mit jeder Aufgabe unter ihr, direkt hinter die kopierte.

d-ojzyzzxu = Löschen
    .doc = Löscht eine Aufgabe und ihre Teilaufgaben.

d-rjpcv6es = Erledigte Aufgaben löschen
    .doc = Löscht jede erledigte Aufgabe und antwortet, wie viele gingen: ein erledigter Zweig geht ganz, und eine erledigte Aufgabe, unter der noch ein Schritt offen ist, bleibt.
    .w-nothing-is-finished-yet = Noch ist nichts erledigt

d-2xixz4sk = Projekt löschen
    .doc = Löscht ein Projekt und seine Abschnitte; seine Aufgaben gehen zurück in den Eingang.

d-2pra5lmb = Neuer Abschnitt
    .doc = Fügt am Ende des Boards eines Projekts einen Abschnitt hinzu.
    .w-new-section = Neuer Abschnitt

d-xmsyxvog = Abschnitt umbenennen
    .doc = Benennt einen Abschnitt um.

d-vi6srojh = Abschnitt löschen
    .doc = Löscht einen Abschnitt; seine Aufgaben gehen in die erste Spur des Projekts.

d-pcn3xgni = Neue Aufgabe
    .doc = Fügt einem Projekt eine Aufgabe hinzu, am Ende der Spur `s` seines Boards (keine für die erste).

d-scjlkb6d = Im Board platzieren
    .doc = Setzt eine Aufgabe in die Spur `s` des Boards ihres Projekts (keine für die erste), an die Stelle `at` unter den Aufgaben, die schon dort stehen: was das Ziehen einer Karte nennt.
    .w-it-is-in-no-project-s = Sie steht in keinem Board
    .w-that-lane-belongs-to-another-project = Diese Spur gehört zu einem anderen Projekt

d-kjbukd2o = Umbenennen
    .doc = Benennt ein Projekt um.

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

d-mg3c777a = Verschieben nach~present

d-tpvjqy5m = Erledigte löschen~present

d-rxpe5yqk = Platzieren~present

d-i2mhsb3h = Start
    .w-tasks = Aufgaben
    .w-new-task-in = Neue Aufgabe in { $name }
    .w-new-task-for-today = Neue Aufgabe für heute
    .w-new-task = Neue Aufgabe
    .w-projects = Projekte
    .w-no-tasks-here = Keine Aufgaben hier

d-doqleehr = Aufgabe
    .w-what-to-do = Was zu tun ist
    .w-anything-worth-remembering-about-it = Alles, was sich darüber zu merken lohnt.
    .w-inbox = Eingang
    .w-remind = Erinnern
    .w-reminder-set = Erinnerung gesetzt
    .w-remind-me = Erinnere mich …
    .w-no-reminder = Keine Erinnerung
    .w-subtasks = Teilaufgaben
    .w-no-subtasks-yet = Noch keine Teilaufgaben
    .w-add-a-subtask = Eine Teilaufgabe hinzufügen
    .w-open-as-page = Als Seite öffnen

d-7froalwb = Projektliste
    .w-new-task-in = Neue Aufgabe in { $name }
    .w-no-tasks-yet = Noch keine Aufgaben

d-gu3twlv2 = Projektboard
    .w-new-task = Neue Aufgabe

d-hc5ndp4x = Aufgabenkarte
    .w-remove-from-section = Aus dem Abschnitt nehmen
    .w-move-to = Verschieben nach { $name }

d-w63ba6ad = Aufgabenzeile

d-rj4l44km = Aufgabenzusammenfassung

d-ypbiaiq3 = Zeile der Aufgabengliederung

d-mtmufg3w = Aufgabenseite
    .w-what-to-do = Was zu tun ist
    .w-anything-worth-remembering-about-it = Alles, was sich darüber zu merken lohnt.
    .w-inbox = Eingang
    .w-remind = Erinnern
    .w-reminder-set = Erinnerung gesetzt
    .w-remind-me = Erinnere mich …
    .w-no-reminder = Keine Erinnerung
    .w-subtasks = Teilaufgaben
    .w-no-subtasks-yet = Noch keine Teilaufgaben
    .w-add-a-subtask = Eine Teilaufgabe hinzufügen

# What each argument is called where a person gives it.

d-rs3ubifz-p = P

d-rs3ubifz-s = S

d-rs3ubifz-due = Fällig

d-rs3ubifz-at = Wann

d-rs3ubifz-assignee = Zuständig

d-rs3ubifz-title = Titel

d-vwgro4iv-done = Erledigt

d-fykojijd-t = T

d-fykojijd-title = Titel

d-jqhwyztw-due = Fällig

d-3qq23ybf-when = Wann

d-laxsa3x6-p = P

d-c6y4cxdd-who = Wer

d-berwb3de-moved = Verschoben

d-berwb3de-parent = Übergeordnet

d-berwb3de-before = Davor

d-he6qkhh4-above = Darüber

d-23y3zdgu-t = T

d-ojzyzzxu-t = T

d-2xixz4sk-p = P

d-xmsyxvog-name = Name

d-pcn3xgni-title = Titel

d-pcn3xgni-s = S

d-scjlkb6d-s = S

d-scjlkb6d-at = Wann

d-kjbukd2o-name = Name
