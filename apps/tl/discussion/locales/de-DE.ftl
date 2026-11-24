# Aufgaben auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-lang-discussion = Diskussion

d-afjtpur7 = Herkunft
    .doc = Wo es zuerst geschrieben wurde, als es aus einem anderen Tracker oder Dienst hierher kam: die Person dort, der Dienst, wo es dort steht und seine Referenz, damit eine Aufgabe oder ein Thread von anderswo sagt, woher er oder sie kam.
    .noun = { $count ->
        [one] Herkunft
       *[other] Herkünfte
    }

d-xxyuyumk = Autor
    .doc = Wer es dort schrieb, unter dem Namen von dort.

d-gmvkzzuz = Kürzel
    .doc = Der Name, unter dem die Person dort bekannt war, wenn er abweicht.

d-nfpru2aa = Quelle
    .doc = Der Dienst, aus dem es kam.

d-wuizz4mi = Adresse
    .doc = Wo es dort zu lesen ist.

d-semowww2 = Referenz
    .doc = Seine Referenz dort, etwa seine Nummer.

d-3wgbb6lz = Erstellt
    .doc = Wann es dort zuerst geschrieben wurde.

d-2wr4psic = Aktualisiert
    .doc = Wann es sich dort zuletzt änderte.

d-afjtpur7-filter-created = Erstellt

d-afjtpur7-filter-updated = Aktualisiert

d-kj3aantt = Kommentar
    .doc = Was jemand zu einer Ressource gesagt hat: zu einer Aufgabe, einer Notiz, einer Mail, einem Tag.
    .noun = { $count ->
        [one] Kommentar
       *[other] Kommentare
    }

d-5q4mbecw = Text

d-c3oavkxa = Von einem Agenten
    .doc = Ob ein Agent es schrieb: sein Autor ist der Agent, und wer es liest, sieht es.

d-uiyftp3p = Herkunft
    .doc = Wo es zuerst geschrieben wurde, als es aus einem anderen Tracker hierher kam.

d-f6337kvt = Schritt
    .doc = Der Schritt eines Agentenlaufs, der es schrieb: die Aktion, die er aufrief, oder was er tat.

d-hqpdw4ih = Nachgeschlagen
    .doc = Die Ressource, die der Schritt nachschlug, wenn er eine las.

d-tovzcpp5 = Übernommen
    .doc = Ob die Worte einer späteren Sitzung es in die Notiz der Ressource übernommen haben.

d-kj3aantt-filter-by_agent = Von einem Agenten

d-kj3aantt-filter-origin = Herkunft

d-kj3aantt-filter-incorporated = Übernommen

d-d6k3e7fz = Kommentare
    .doc = Jeder Kommentar des Arbeitsbereichs, in der Reihenfolge, in der sie gemacht wurden.
    .noun = { $count ->
        [one] Kommentar
       *[other] Kommentare
    }

d-3g3duquj = Einträge

d-l7s2czjp = Kommentieren
    .doc = Sagt etwas zu der Ressource, aus der der Kommentar gemacht wird: der Thread unter einer Aufgabe, einer Notiz oder einer Mail sagt es mit `.made_from(r)`, sodass der Kommentar festhält, worum es geht.
    .did = kommentierte
    .w-write-a-comment-first = Schreib zuerst einen Kommentar

d-l7s2czjp-text = Text

d-l7s2czjp-origin = Herkunft

d-l7s2czjp-step = Schritt

d-l7s2czjp-lookup = Nachgeschlagen

d-l7s2czjp-incorporated = Übernommen

d-iae4o5k3 = Bearbeiten
    .doc = Ändert, was ein Kommentar sagt.
    .w-only-its-author-can-edit-a = Nur wer ihn geschrieben hat, kann einen Kommentar bearbeiten
    .w-write-a-comment-first = Schreib zuerst einen Kommentar

d-iae4o5k3-text = Text

d-l3wyj42h = Löschen
    .doc = Löscht einen Kommentar.
    .w-only-its-author-can-delete-a = Nur wer ihn geschrieben hat, kann einen Kommentar löschen

d-l3wyj42h-c = C

d-vzn4frig = Kommentiert

d-4mzlhyvw = Bearbeitet

d-kflwfuct = Gelöscht

d-upy33rnt = Thread
    .w-discussion = Diskussion
    .w-edit-comment = Kommentar bearbeiten
    .w-cancel = Abbrechen
    .w-save = Speichern
    .w-edit = Bearbeiten
    .w-reply = Antworten
    .w-open-again = Wieder öffnen
    .w-resolve = Erledigen
    .w-write-a-reply = Antwort schreiben
    .w-send = Senden
    .w-add-a-comment = Kommentar hinzufügen

d-nsv2jvpu = Geschrieben
    .w-edited = bearbeitet

d-vp5uwkal = Kürzlich
    .w-nothing-said-yet = Noch nichts gesagt

d-3666szuv = Kommentarseite

d-jxt3set4 = Bemerkung

d-ng7oourf = Antwort auf
    .doc = Der Kommentar, den er beantwortet, wenn er einen beantwortet: eine Antwort steht unter der Bemerkung, die sie beantwortet.

d-lz4a7tbk = Erledigt
    .doc = Ob das Verlangte getan ist: eine erledigte Bemerkung klappt zusammen, und ihr Autor oder wer sie liest kann sie wieder öffnen.

d-kj3aantt-filter-reply_to = Antwort auf

d-kj3aantt-filter-resolved = Erledigt

d-l7w5vz3v = Antworten
    .doc = Beantwortet einen Kommentar: die Antwort steht unter der Bemerkung, die sie beantwortet, wie die Antwort eines Gesprächs, und worum es geht, ist das, worum es in der Bemerkung geht.
    .did = hat geantwortet
    .w-write-a-reply-first = Schreiben Sie zuerst eine Antwort

d-l7w5vz3v-text = Text

d-5i4uizbb = Erledigen
    .doc = Markiert einen Kommentar als erledigt oder öffnet ihn wieder.
    .w-resolve = Erledigen
    .w-open-again = Wieder öffnen

d-5i4uizbb-resolved = Erledigt

d-cr62rvlw = Antworten~present

d-5fveadpi = Erledigen~present
