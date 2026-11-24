# PDF-Betrachter auf Deutsch. Keys sind die stabilen Ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-pdf-viewer = PDF-Betrachter

d-so-tilo-pdf-viewer-keywords = pdf, reader, document, annotate, highlight, sign, form, dokument, lesen, kommentieren, markieren, unterschreiben, formular, pdf-betrachter

d-typz3vku = Dokument
    .doc = Ein PDF, das der Arbeitsbereich liest: die Seiten, die es zeigt, und jede Markierung, jeden Kommentar und jede Unterschrift, die seine Leser darauf hinterlassen haben.
    .noun = { $count ->
        [one] Dokument
       *[other] Dokumente
    }
    .untitled = Unbenanntes Dokument

d-peajfzyh = Quelle
    .doc = Die Adresse der Bytes des PDFs, `tilo-blob://…`.

d-mxgrycmo = Quellenname
    .doc = Wie die Datei heißt.

d-ebwmtj4p = Störung
    .doc = Warum die Seiten nicht angezeigt werden können, in den Worten der Engine; leer, solange sie es können.

d-2n6kpf64 = Importierte Quelle
    .doc = Die Quelle, deren eigene Markierungen einmal übernommen wurden.

d-zst2moht = Speicherstörung
    .doc = Warum keine Kopie gespeichert werden konnte, in den Worten der Engine; leer, bis eine versucht wird.

d-vhujxgcb = Seiten

d-zhtkbfeh = Bilder
    .doc = Eine Seite, einmal in einer Breite gezeichnet, als Adresse ihres Bildes: was der Betrachter zeigt.

d-22zxqkpo = Gliederung
    .doc = Der Inhalt des Dokuments, so wie die Engine ihn liest.

d-oushu5xx = Felder
    .doc = Die Formularfelder, die die Datei trägt, wo sie stehen und was sie enthalten.

d-ilzd6zzn = Markierungen

d-7zfoarqk = Kommentare

d-s7stggro = Unterschriften

d-uwnyzedo = Formulartext
    .doc = Was Menschen in das Formular eingetragen haben, nach Feldnamen.

d-zoztpthf = Formularauswahl

d-2waifcc4 = Formular angekreuzt

d-kew5sddy = Formularexport

d-cvhgaqat = Übernommen
    .doc = Die Markierungen, die die Datei selbst trug, nach der Id, die jede davon erhielt: der Fingerabdruck, den sie beim Übernehmen hatte, damit ein Speichern eine unveränderte als eigenes Objekt der Datei behält.

d-hflhpsk4 = Leseseite
    .doc = Die Seite, auf der der Leser war, wie weit unten darauf und der Zoom in Prozent.

d-au5nrsiz = Lesefortschritt

d-fg6xtkn7 = Lesezoom

d-44bcedlj = Seite
    .doc = Eine Seite eines Dokuments: welche Seite welcher Bytes sie zeigt, wie groß sie ist und wie weit sie gedreht ist.
    .noun = { $count ->
        [one] Seite
       *[other] Seiten
    }

d-oedigi3h = Quellseite
    .doc = Die Seite der Datei, von der sie kommt, ab 1 gezählt.

d-vphbhfba = Importiert
    .doc = Die Adresse der Bytes einer importierten Seite; leer für die eigene Datei des Dokuments.

d-5c4rqnpi = Art
    .doc = Was diese Bytes sind, `pdf` oder `image`; leer für die eigene Datei des Dokuments.

d-qxyy4u3k = Breite
    .doc = Ihre Breite und Höhe, wie die Datei sie zeichnet, in Punkten.

d-bepcv7nh = Höhe

d-vlm7goob = Drehung
    .doc = Eine Vierteldrehung zusätzlich zur eigenen der Seite.

d-fwoikm62 = Wörter
    .doc = Ihre Wörter, wie die Engine sie liest; leer, bis sie gelesen sind.

d-kdrgc55v = Gelesen
    .doc = Ob ihre Wörter gelesen wurden: eine Seite aus Bildern ist auch gelesen und enthält keine.

d-bgi4vhjd = Bild
    .doc = Eine Seite, einmal in einer Breite gezeichnet, als Adresse ihres Bildes behalten: jede Seite bei jedem Blick neu zu zeichnen kostet die Engine je ein Rendern, und eine Seite ist dasselbe Bild, bis sich ihre Drehung, ihre Quelle oder die angefragte Breite ändert.
    .noun = { $count ->
        [one] Bild
       *[other] Bilder
    }
    .untitled = Seitenbild

d-e5fx4rry = Seite

d-lps7jwgj = Breite
    .doc = Die Breite, in der es gezeichnet wurde, in Pixeln.

d-ht6kgmaq = Url

d-bgi4vhjd-filter-page = Seite

d-bgi4vhjd-filter-width = Breite

d-dsgjr5bj = Inhaltseintrag
    .doc = Ein Eintrag des Dokumentinhalts, wie die Engine ihn auflistet, flachgeklopft.
    .noun = { $count ->
        [one] Inhaltseintrag
       *[other] Inhaltseinträge
    }

d-qfml2pla = Titel

d-34ukafjp = Seite
    .doc = Die Seite, zu der er führt, ab 1 gezählt; 0 für einen Eintrag, der nirgendwohin führt.

d-v7hjvwyk = Ebene
    .doc = Wie tief er steht: 0 ganz oben.

d-dsgjr5bj-filter-page = Seite

d-dsgjr5bj-filter-level = Ebene

d-drvz3law = Feld
    .doc = Ein Formularfeld, das die Datei trägt, wo sein Widget auf einer Seite steht und was es enthält.
    .noun = { $count ->
        [one] Feld
       *[other] Felder
    }

d-5ghgpzwq = Name

d-4mifdrqw = Art
    .doc = `text`, `checkbox`, `radio` oder `choice`.

d-enpzi5ta = Wert

d-f6evcr4f = Angekreuzt

d-jjifolfg = Export

d-ya7ugvvf = Optionen

d-h2kxabyf = Seite
    .doc = Die Seite, auf der sein Widget steht, ab 1 gezählt; 0 für eines ohne Widget.

d-ijs3t4gj = Rechteck
    .doc = Die Fläche seines Widgets in Punkten: x, y, Breite, Höhe.

d-eh3gs42n = Schreibgeschützt

d-lxh5hh6y = Erforderlich

d-drvz3law-filter-checked = Angekreuzt

d-drvz3law-filter-page = Seite

d-drvz3law-filter-read_only = Schreibgeschützt

d-drvz3law-filter-required = Erforderlich

d-w2mt4umt = Markierung
    .doc = Eine Markierung, die ein Leser auf einer Seite hinterlassen hat.
    .noun = { $count ->
        [one] Markierung
       *[other] Markierungen
    }
    .untitled = Markierung

d-4x7a7juf = Seite

d-n23wswru = Art

d-ikltq7t2 = Farbe
    .doc = Die Farbe, in der sie gezeichnet wird, `#rrggbb`.

d-35epyc2h = Stil
    .doc = Was eine Textmarkierung zeichnet: `highlight`, `underline`, `strikeout` oder `squiggly`.

d-qtn6mlbl = Rechtecke
    .doc = Die Rechtecke, die sie bedeckt, `x, y, Breite, Höhe` nacheinander, in Punkten der Seite, wie die Datei sie zeichnet: die Zeilen einer Textmarkierung, der Kasten eines Textfelds.

d-crim3nye = X
    .doc = Wo eine Haftnotiz steht, in Punkten.

d-3rusqrrq = Y

d-agxxncq4 = Wörter
    .doc = Die Wörter eines Textfelds oder einer Notiz.

d-xt4alsjd = Größe
    .doc = Die Buchstaben eines Textfelds, in Punkten.

d-fz2kryor = Striche
    .doc = Die Striche, aus denen Tinte oder eine gezeichnete Unterschrift besteht, als Punkte x und y nacheinander in Bruchteilen des Kastens der Seite.

d-2a5limjf = Verfasser
    .doc = Wer sie hinterlassen hat, wenn die Datei es sagt; leer für eine hier gemachte Markierung, deren Verfasser ihre Änderung ist.

d-fkkbarxd = Übernommene Id
    .doc = Die eigene Markierung der Datei, von der diese übernommen wurde, nach der Id, die die Engine ihr gibt; leer für eine hier gemachte Markierung.

d-w2mt4umt-filter-page = Seite

d-w2mt4umt-filter-kind = Art

d-w2mt4umt-filter-x = X

d-w2mt4umt-filter-y = Y

d-w2mt4umt-filter-size = Größe

d-xfqcfhkv = Strich
    .doc = Ein Strich einer Zeichnung: seine Punkte, x und y nacheinander.
    .noun = { $count ->
        [one] Strich
       *[other] Striche
    }

d-4dzcakfh = Punkte

d-rpajrhcr = Kommentar
    .doc = Was Menschen über eine Seite oder über eine ihrer Markierungen gesagt haben.
    .noun = { $count ->
        [one] Kommentar
       *[other] Kommentare
    }

d-e5qxkctu = Markierung
    .doc = Die Markierung, um die es geht; keine für einen über die Seite selbst.

d-btdnd5pa = Seite

d-ogqe2kdm = Wörter

d-hcfcbfso = Text

d-hgps2s5i = Erledigt

d-thfbylkn = Antworten

d-rpajrhcr-filter-open = Offen

d-rpajrhcr-filter-resolved = Erledigt

d-7jxpvur2 = Antwort
    .doc = Eine Antwort in der Unterhaltung eines Kommentars.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }

d-47zrerme = Wörter

d-exvjyq6h = Text

d-wd5gpwkf = Unterschrift
    .doc = Eine Unterschrift, die auf einer Seite platziert ist.
    .noun = { $count ->
        [one] Unterschrift
       *[other] Unterschriften
    }

d-xudq4utx = Seite

d-uyofemys = X

d-mai2dzre = Y

d-tvp2usjk = Breite

d-iriyo77n = Höhe

d-ozrikra3 = Unterzeichner

d-k6r3zcgg = Datum
    .doc = Der Tag, an dem unterschrieben wurde, wie der Unterzeichner ihn schrieb.

d-5pbiosiz = Striche
    .doc = Die Striche einer gezeichneten Unterschrift, in Bruchteilen ihres Kastens; keine für eine getippte, die der Name des Unterzeichners im Kasten ist.

d-wd5gpwkf-filter-page = Seite

d-wd5gpwkf-filter-x = X

d-wd5gpwkf-filter-y = Y

d-wd5gpwkf-filter-width = Breite

d-wd5gpwkf-filter-height = Höhe

d-a5isswfb = Bibliothek
    .doc = Die PDFs des Arbeitsbereichs und die Unterschrift, die eine Person auf jedem Dokument wiederverwendet, das sie unterschreibt.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-bqmqo56y = Dokumente

d-u2q7brpp = Unterzeichner
    .doc = Der Name, den eine getippte Unterschrift trägt; leer, bis eine Person einen setzt.

d-bny56wzk = Striche
    .doc = Die Striche einer gezeichneten Unterschrift, damit das nächste Dokument mit einem Druck unterschrieben ist; leer, bis eine Person eine zeichnet.

d-p5eyqgoi = Seitenzeile
    .doc = Eine Seite, wie die Engine sie beantwortet: ihre Nummer und die Größe, in der sie steht.
    .noun = { $count ->
        [one] Seitenzeile
       *[other] Seitenzeilen
    }

d-2xoqlfit = Seite

d-hsgsnasu = Breite

d-vmsxv3ma = Höhe

d-p5eyqgoi-filter-page = Seite

d-p5eyqgoi-filter-width = Breite

d-p5eyqgoi-filter-height = Höhe

d-tkibkym4 = Glyphenzeile
    .doc = Ein Zeichen des Textes einer Seite, im Anzeigeraum der Seite.
    .noun = { $count ->
        [one] Glyphenzeile
       *[other] Glyphenzeilen
    }

d-e35nwhcd = Text

d-owthh7c4 = Zeile

d-at7kirp3 = X

d-w53k2oqb = Y

d-7ufljz44 = B

d-showgoq3 = H

d-5u27ffde = Platziert

d-tkibkym4-filter-line = Zeile

d-tkibkym4-filter-x = X

d-tkibkym4-filter-y = Y

d-tkibkym4-filter-w = B

d-tkibkym4-filter-h = H

d-tkibkym4-filter-placed = Platziert

d-ybwcg2lj = Importzeile
    .doc = Was eine Datei als Seiten enthält.
    .noun = { $count ->
        [one] Importzeile
       *[other] Importzeilen
    }

d-b35hfv2r = Art

d-m2v5uip7 = Seiten

d-u36zis4z = Inhaltszeile
    .doc = Ein Eintrag des Inhalts eines Dokuments, wie die Engine ihn auflistet.
    .noun = { $count ->
        [one] Inhaltszeile
       *[other] Inhaltszeilen
    }

d-rl5gr6nc = Titel

d-mtayx6nb = Seite

d-ewudh3r2 = Ebene

d-u36zis4z-filter-page = Seite

d-u36zis4z-filter-level = Ebene

d-rqrdcezf = Markierungszeile
    .doc = Eine Markierung, die die Bytes der Datei selbst tragen.
    .noun = { $count ->
        [one] Markierungszeile
       *[other] Markierungszeilen
    }

d-s3btzxsu = Seite

d-5x3rjw33 = Index

d-l6esysss = Name

d-i3qsks26 = Art

d-qca4t246 = Farbe

d-5ddf4ghi = Inhalt

d-ejgwsaae = Verfasser

d-4chyadse = Erstellt

d-sw7xlda5 = Geändert

d-xsmehh7m = Rechteck

d-v7q3ceeu = Quads

d-pv225oqp = Striche

d-yrxpeq4o = Größe

d-xkarfjs4 = Antwort auf

d-rwsq3hzc = Antwort

d-udw6i6fp = Versteckt

d-nuljshqh = Lesbar

d-rqrdcezf-filter-page = Seite

d-rqrdcezf-filter-index = Index

d-rqrdcezf-filter-size = Größe

d-rqrdcezf-filter-in_reply_to = Antwort auf

d-rqrdcezf-filter-reply = Antwort

d-rqrdcezf-filter-hidden = Versteckt

d-rqrdcezf-filter-readable = Lesbar

d-dtgx4nex = Markup-Zeile
    .doc = Die Markierungen und das Formular, die eine Datei trägt.
    .noun = { $count ->
        [one] Markup-Zeile
       *[other] Markup-Zeilen
    }

d-6cyewmj4 = Markierungen

d-dk6bxvq5 = Felder

d-ec3hou66 = Feldzeile
    .doc = Ein Formularfeld einer Datei.
    .noun = { $count ->
        [one] Feldzeile
       *[other] Feldzeilen
    }

d-havty2vp = Name

d-d4cnpeq3 = Art

d-br4tfro7 = Wert

d-jyx27y2l = Angekreuzt

d-tfvicybt = Export

d-66vdxlss = Optionen

d-puoybohy = Seite

d-ip3ujn6w = Rechteck

d-mneasolx = Schreibgeschützt

d-i46gksbq = Erforderlich

d-ec3hou66-filter-checked = Angekreuzt

d-ec3hou66-filter-page = Seite

d-ec3hou66-filter-read_only = Schreibgeschützt

d-ec3hou66-filter-required = Erforderlich

d-c2khvmu2 = Seitenausgabe
    .doc = Eine Seite einer Kopie, wie ein Speichern sie nennt.
    .noun = { $count ->
        [one] Seitenausgabe
       *[other] Seitenausgaben
    }

d-d56ntmtj = Quelle

d-5y2litv2 = Seite

d-hgb4qbox = Drehung

d-oaf2yyke = Primär

d-c2khvmu2-filter-source = Quelle

d-c2khvmu2-filter-page = Seite

d-c2khvmu2-filter-rotation = Drehung

d-c2khvmu2-filter-primary = Primär

d-aa3m6nmt = Markierungsausgabe
    .doc = Eine Markierung einer Kopie, im Anzeigeraum der Ausgabeseite.
    .noun = { $count ->
        [one] Markierungsausgabe
       *[other] Markierungsausgaben
    }

d-qlypcvpd = Seite

d-jfdwouoh = Art

d-4aptjhj3 = Farbe

d-5vi7wvna = Verfasser

d-wt44cnbi = Inhalt

d-cv6deozk = Name

d-uzy5hqqe = Erstellt ms

d-qnpoauzu = Rechtecke

d-ilro56v5 = Striche

d-slisyq2r = Rechteck

d-u3zrbin4 = Größe

d-aa3m6nmt-filter-page = Seite

d-aa3m6nmt-filter-created_ms = Erstellt ms

d-aa3m6nmt-filter-size = Größe

d-a5kt32h3 = Antwortausgabe
    .doc = Eine Antwort, geschrieben mit der Anmerkung, die sie beantwortet.
    .noun = { $count ->
        [one] Antwortausgabe
       *[other] Antwortausgaben
    }

d-7ckypqja = Seite

d-ksvfd5se = X

d-g6nqwvtx = Y

d-qs6kxe3w = Verfasser

d-vqovoaba = Text

d-kcbfphy7 = Name

d-qdwom5mm = Antwortet auf

d-a5kt32h3-filter-page = Seite

d-a5kt32h3-filter-x = X

d-a5kt32h3-filter-y = Y

d-suymio4x = Wertausgabe
    .doc = Ein Formularwert einer Kopie.
    .noun = { $count ->
        [one] Wertausgabe
       *[other] Wertausgaben
    }

d-4uvljgge = Name

d-tlqdrzox = Art

d-fp6projh = Text

d-oquhk6ja = An

d-mlfgloi5 = Export

d-suymio4x-filter-on = An

d-h2aiklu2 = Übernommenausgabe
    .doc = Eine eigene Markierung der Datei, die eine Kopie behält, ersetzt oder entfernt.
    .noun = { $count ->
        [one] Übernommenausgabe
       *[other] Übernommenausgaben
    }

d-yzggzvtt = Seite

d-jxgttint = Index

d-k3tlktwg = Name

d-ua2gidzw = Art

d-jlsyhy65 = Unverändert

d-h2aiklu2-filter-page = Seite

d-h2aiklu2-filter-index = Index

d-h2aiklu2-filter-unchanged = Unverändert

d-j6fkbfbv = Speicheranfrage
    .doc = Alles, was ein Speichern schreibt.
    .noun = { $count ->
        [one] Speicheranfrage
       *[other] Speicheranfragen
    }

d-l5cfhg2u = Seiten

d-egrqnnmk = Markierungen

d-fy7mbgs7 = Antworten

d-q2jlnwmi = Werte

d-igdolmln = Übernommen

d-c7m3hmjb = Markierungsfarbe
    .doc = Eine Markierungsfarbe, nach dem Namen, unter dem sie gewählt wird, und der Farbe, die sie zeichnet.
    .noun = { $count ->
        [one] Markierungsfarbe
       *[other] Markierungsfarben
    }

d-yr5hwqqr = Name

d-fmr3zdmh = Wert

d-k7x4fj7s = Markierungsart
    .doc = Was eine Markierung auf einer Seite tut.

d-p5ikccc5 = Mit einem Textmarker, einer Unterstreichung, einem Durchstreichen oder einer Wellenlinie markierter Text
    .doc = Mit einem Textmarker, einer Unterstreichung, einem Durchstreichen oder einer Wellenlinie markierter Text.

d-6jhaxxim = Freihandtinte
    .doc = Freihandtinte.

d-dfpdvuf4 = Eine Haftnotiz
    .doc = Eine Haftnotiz.

d-3kjya37k = Ein Kasten voller Wörter
    .doc = Ein Kasten voller Wörter.

d-sb3liwex = PDF öffnen
    .doc = Ein PDF öffnet sich im Betrachter: Dateien, das eigene Dateisystem der Person und die Suche des Systems reichen es als die Datei herein, die es ist, und es öffnet sich auf dem Dokument, das diese Bytes bereits sind.
    .did = hat ein PDF geöffnet

d-sb3liwex-file = Datei

d-q6ctwqv7 = Ein PDF aus dem Inhalt des Arbeitsbereichs öffnen
    .doc = Öffnet das PDF, dessen Bytes der Arbeitsbereich unter `address` hält: das Dokument, das diese Bytes sind, wenn eins sie hält, sonst ein neues.
    .did = hat ein PDF geöffnet
    .w-a-document-is-opened-from-the = Ein Dokument wird über die Adresse seiner Bytes geöffnet

d-q6ctwqv7-address = Adresse

d-q6ctwqv7-name = Name

d-svkv5kmu = Die Seiten anordnen
    .doc = Ordnet ein Dokument über seinen eigenen Bytes an: eine Seite für jede Seite der Datei, in ihrer Reihenfolge, in der Größe, in der die Datei sie zeichnet, ihren Inhalt und ihr Formular.
    .did = hat die Seiten angeordnet

d-47sfve6u = Den Inhalt lesen
    .doc = Liest den Inhalt des Dokuments, damit der Prüfbereich ihn auflistet, ohne die Engine jedes Mal zu fragen, wenn er gezeigt wird.

d-3lai6j3z = Das Formular lesen
    .doc = Liest die Formularfelder des Dokuments, damit ihre Widgets über die Seiten gezeichnet und an Ort und Stelle ausgefüllt werden.

d-ejxksddn = Die Wörter einer Seite lesen
    .doc = Liest die Wörter der Seite einmal, damit die Suche des Arbeitsbereichs die Seite über sie findet.

d-ejxksddn-p = P

d-g5hnjkuf = Eine Seite zeichnen
    .doc = Zeichnet eine Seite als Bild mit `width` Pixeln, behalten, damit eine Person, die sie erneut ansieht, oder ein anderes Mitglied des Arbeitsbereichs nicht auf die Engine wartet.

d-g5hnjkuf-p = P

d-g5hnjkuf-width = Breite

d-hztfw3kc = Die Wörter der nächsten Seite lesen
    .doc = Liest die Wörter der nächsten Seite, die niemand gelesen hat.

d-4sqh76et = Die nächste Seite zeichnen
    .doc = Zeichnet die nächste Seite, die niemand im Zoom des Lesers gezeichnet hat.

d-nlhwr2fh = Seite drehen
    .doc = Dreht eine Seite eine Vierteldrehung im Uhrzeigersinn.

d-nlhwr2fh-p = P

d-73dv3j6a = Seite verschieben
    .doc = Setzt eine Seite hinter eine andere oder an den Anfang.
    .w-a-page-cannot-go-after-itself = Eine Seite kann nicht hinter sich selbst stehen

d-73dv3j6a-p = P

d-73dv3j6a-after = Nach

d-7ibbaapq = Seite duplizieren
    .doc = Kopiert eine Seite direkt hinter sich selbst.

d-7ibbaapq-p = P

d-rjejnje3 = Seite löschen
    .doc = Nimmt eine Seite aus dem Dokument; ein Dokument behält mindestens eine.
    .w-a-document-keeps-at-least-one = Ein Dokument behält mindestens eine Seite

d-rjejnje3-p = P

d-newzwxq5 = Seiten importieren
    .doc = Nimmt die Seiten eines anderen PDFs oder eines Bildes in das Dokument auf, hinter eine Seite (an den Anfang, wenn keine genannt ist).
    .did = hat Seiten importiert

d-newzwxq5-after = Nach

d-newzwxq5-file = Datei

d-ocfsjb5a = Text markieren
    .doc = Markiert Text auf einer Seite: eine Markierung, eine Unterstreichung, ein Durchstreichen oder eine Wellenlinie über den Zeilen, die eine Auswahl bedeckt, in der Farbe des Lesers.
    .did = hat Text markiert
    .w-a-mark-covers-something = Eine Markierung bedeckt etwas

d-ocfsjb5a-p = P

d-ocfsjb5a-style = Stil

d-ocfsjb5a-colour = Farbe

d-ocfsjb5a-rects = Rechtecke

d-6tod6rqt = Die ausgewählten Wörter markieren
    .doc = Markiert genau die Wörter, die eine Auswahl bedeckt: die Abschnitte von `p` von `first` bis `last`, beide gezählt, als Markierung, Unterstreichung, Durchstreichen oder Wellenlinie in der Farbe, die der Betrachter hält. So markiert eine Person Zeilen, die sie ausgewählt hat, so wie die Wörter gelesen werden; ein gezogener Kasten markiert weiterhin die Wörter, die der Kasten bedeckt.
    .did = hat Text markiert
    .w-a-mark-covers-the-words-a = Eine Markierung bedeckt die Wörter einer Auswahl

d-6tod6rqt-p = P

d-6tod6rqt-style = Stil

d-6tod6rqt-colour = Farbe

d-6tod6rqt-first = Erste

d-6tod6rqt-last = Letzte

d-kc6nzzkh = Zeichnen
    .doc = Zeichnet Tinte auf eine Seite: eine Punktliste ein Strich, seine Punkte als x und y nacheinander in Bruchteilen des Kastens der Seite.
    .did = hat auf eine Seite gezeichnet

d-kc6nzzkh-p = P

d-kc6nzzkh-colour = Farbe

d-kc6nzzkh-strokes = Striche

d-vug2naba = Notiz hinzufügen
    .doc = Setzt eine Haftnotiz auf eine Seite, wo die Person gedrückt hat, noch ohne etwas darin: was sie sagt, wird in der Unterhaltung geschrieben, die sie beginnt.
    .did = hat eine Notiz hinzugefügt

d-vug2naba-p = P

d-vug2naba-x = X

d-vug2naba-y = Y

d-vug2naba-colour = Farbe

d-vnlqtooa = Notiz hinzufügen
    .doc = Fügt oben auf einer Seite eine Notiz mit diesen Worten hinzu: für die Palette, ein Skript und einen Agenten.
    .did = hat eine Notiz hinzugefügt

d-vnlqtooa-p = P

d-vnlqtooa-words = Wörter

d-cuzfsube = Ein Textfeld hinzufügen
    .doc = Setzt einen Kasten voller Wörter auf eine Seite.
    .did = hat ein Textfeld hinzugefügt

d-cuzfsube-p = P

d-cuzfsube-x = X

d-cuzfsube-y = Y

d-cuzfsube-width = Breite

d-cuzfsube-height = Höhe

d-3g3j2j7f = Eine Markierung bearbeiten
    .doc = Ändert, was eine Markierung sagt, und ihren Kasten, wenn einer genannt ist.
    .did = hat eine Markierung bearbeitet

d-3g3j2j7f-m = M

d-3g3j2j7f-words = Wörter

d-3g3j2j7f-rects = Rechtecke

d-ohknsjyv = Eine Notiz verschieben
    .doc = Setzt eine Notiz dorthin, wo die Person sie bewegt hat.
    .did = hat eine Notiz verschoben

d-ohknsjyv-m = M

d-ohknsjyv-x = X

d-ohknsjyv-y = Y

d-fnztmqkv = Die Farbe einer Markierung ändern
    .doc = Gibt einer Markierung eine andere Farbe.
    .did = hat eine Markierung umgefärbt

d-fnztmqkv-m = M

d-fnztmqkv-colour = Farbe

d-hs5adic7 = Markierung löschen
    .doc = Nimmt eine Markierung weg und mit ihr, was darüber gesagt wurde.

d-hs5adic7-m = M

d-aoszgtah = Kommentieren
    .doc = Sagt etwas über eine Seite oder über eine ihrer Markierungen.
    .did = hat kommentiert

d-aoszgtah-p = P

d-aoszgtah-m = M

d-aoszgtah-words = Wörter

d-tvtgg3pl = Antworten
    .doc = Beantwortet einen Kommentar.
    .did = hat geantwortet

d-tvtgg3pl-c = C

d-tvtgg3pl-words = Wörter

d-exeplvmp = Einen Kommentar erledigen oder wieder öffnen
    .doc = Markiert eine Unterhaltung als erledigt oder öffnet sie wieder.
    .did = hat den Zustand eines Kommentars geändert

d-exeplvmp-c = C

d-exeplvmp-resolved = Erledigt

d-4mrvct5b = Kommentar bearbeiten
    .doc = Schreibt neu, was ein Kommentar sagt.
    .did = hat einen Kommentar bearbeitet

d-4mrvct5b-c = C

d-4mrvct5b-words = Wörter

d-ubslpwtp = Kommentar löschen
    .doc = Nimmt eine Unterhaltung weg; eine Unterhaltung über eine Haftnotiz nimmt die Notiz mit, wenn sie die letzte über sie ist.

d-ubslpwtp-c = C

d-xgbmalca = Antwort löschen
    .doc = Löscht eine Antwort einer Unterhaltung.

d-xgbmalca-c = C

d-xgbmalca-r = R

d-bjtdqwle = Unterschreiben als
    .doc = Der Name, den getippte Unterschriften von nun an tragen.
    .did = hat den Unterschriftsnamen gesetzt

d-bjtdqwle-name = Name

d-bxrvpnea = Eine Unterschrift zeichnen
    .doc = Behält die Unterschrift, die eine Person gezeichnet hat, damit das nächste Dokument mit einem Druck unterschrieben ist: die auf dem Feld gezeichneten Striche, x und y nacheinander in Bruchteilen des Kastens der Unterschrift.
    .did = hat die eigene Unterschrift gezeichnet

d-bxrvpnea-strokes = Striche

d-5it2mdqs = Unterschreiben
    .doc = Unterschreibt eine Seite dort, wo die Person gedrückt hat: die Unterschrift, die sie gezeichnet hat, sonst ihr Name im Kasten der Unterschrift.
    .did = hat eine Seite unterschrieben
    .w-type-the-name-to-sign-as = Tippe zuerst den Namen zum Unterschreiben oder zeichne eine Unterschrift

d-5it2mdqs-p = P

d-5it2mdqs-x = X

d-5it2mdqs-y = Y

d-5it2mdqs-width = Breite

d-5it2mdqs-height = Höhe

d-7672bnqp = Mit Namen unterschreiben
    .doc = Unterschreibt eine Seite mit einem getippten Namen in ihrer unteren rechten Ecke: für die Palette, ein Skript und einen Agenten.
    .did = hat eine Seite unterschrieben
    .w-a-signature-names-who-signed = Eine Unterschrift nennt, wer unterschrieben hat

d-7672bnqp-p = P

d-7672bnqp-signer = Unterzeichner

d-to7kq5we = Unterschrift löschen
    .doc = Nimmt eine Unterschrift von der Seite.

d-to7kq5we-s = S

d-agvv6awe = Ein Feld ausfüllen
    .doc = Füllt ein Formularfeld mit Wörtern.
    .did = hat das Formular ausgefüllt

d-agvv6awe-name = Name

d-agvv6awe-text = Text

d-l5ttiwdu = Eine Option eines Felds wählen
    .doc = Wählt eine Option einer Formularauswahl.
    .did = hat das Formular ausgefüllt

d-l5ttiwdu-name = Name

d-l5ttiwdu-choice = Auswahl

d-uohk6ns6 = Ein Kästchen ankreuzen
    .doc = Schaltet den Kasten eines Formulars ein oder aus.
    .did = hat das Formular ausgefüllt

d-uohk6ns6-name = Name

d-uohk6ns6-on = An

d-uohk6ns6-export = Export

d-t5erm5jf = Die Leseposition merken
    .doc = Merkt, wo der Leser aufgehört hat.
    .w-a-reading-position-is-a-share = Eine Leseposition ist ein Anteil der Seite

d-t5erm5jf-p = P

d-t5erm5jf-fraction = Anteil

d-t5erm5jf-zoom = Zoom

d-g4qzysfp = Die eigenen Markierungen des PDFs übernehmen
    .doc = Übernimmt die Markierungen, die die Bytes des PDFs selbst tragen: seine Markierungen, Notizen mit ihren Antworten, freier Text und Tinte werden die dieses Dokuments, einmal je Quelle.
    .did = hat die Markierungen des PDFs übernommen

d-fuzv2bmi = Annotiertes PDF speichern
    .doc = Speichert eine Kopie des PDFs mit den Markierungen des Lesers darin: seine Seiten in der Reihenfolge des Lesers, seine Markierungen, was darüber gesagt wurde, die Werte des Formulars und die eigenen Markierungen der Datei, behalten, wo niemand sie geändert hat.
    .did = hat eine Kopie mit ihren Markierungen gespeichert

d-7662zeux = Zu einer Seite blättern
    .doc = Merkt, dass der Leser zur Seite `at` geblättert ist.

d-7662zeux-at = An

d-7662zeux-zoom = Zoom

d-zpwpewsv = Eine Seite verschieben
    .doc = Setzt die Seite `index` dorthin, wo die Leiste sie fallen ließ: hinter die Seite `at`.
    .did = hat eine Seite verschoben

d-zpwpewsv-index = Index

d-zpwpewsv-at = An

d-n5rcgbh4 = Seiten importieren
    .doc = Importiert Seiten hinter `p`: was das Hinzufügen der Leiste die Person fragt.

d-n5rcgbh4-p = P

d-n5rcgbh4-file = Datei

d-rrjfjggg = Eine Seite drücken
    .doc = Was ein Drücken auf eine Seite tut, je nach Werkzeug im Fenster: setzt eine Notiz, ein Textfeld oder eine Unterschrift dort, wo die Person gedrückt hat.

d-rrjfjggg-p = P

d-rrjfjggg-at = An

d-rrjfjggg-tool = Werkzeug

d-rrjfjggg-colour = Farbe

d-2uq5pptz = Eine Seite loslassen
    .doc = Was das Loslassen tut: ein Markierungswerkzeug markiert den gezogenen Kasten, und Tinte behält den Strich.

d-2uq5pptz-p = P

d-2uq5pptz-tool = Werkzeug

d-2uq5pptz-colour = Farbe

d-2uq5pptz-start = Anfang

d-2uq5pptz-end = Ende

d-hpdtxwj5 = Kommentieren
    .doc = Sagt etwas über die Seite, die das Fenster zeigt.

d-hpdtxwj5-words = Wörter

d-e5pfej3h = Quelle öffnen~present

d-4qnr3itw = Seite verschieben~present

d-3d6agjuu = Seite löschen~present

d-gzdwyydw = Text markieren~present

d-jvpw7pw2 = Die ausgewählten Wörter markieren~present

d-5i3rvbg2 = Unterschrift platzieren~present

d-xljvuoko = Mit Namen unterschreiben~present

d-o3qptln6 = Leseposition merken~present

d-ihyvwez3 = Bibliothek
    .w-no-pdfs-yet = Noch keine PDFs
    .w-open-one-from-this-device-or = Öffne eins von diesem Gerät oder aus den Dateien des Arbeitsbereichs.
    .w-open-pdf = PDF öffnen

d-agcyt7eg = Dokumentzeile

d-uh2bcexu = Bibliothekskopf
    .w-open-pdf = PDF öffnen

d-ibtlwzis = Betrachter
    .w-this-pdf-cannot-be-opened = Dieses PDF kann nicht geöffnet werden
    .w-laying-out-the-pages = Die Seiten werden angeordnet
    .w-previous = Zurück
    .w-next = Weiter
    .w-pages = Seiten
    .w-import = Importieren…
    .w-page = Seite { $source_page }
    .w-page-its-marks = Seite { $source_page }, ihre Markierungen
    .w-rotate = Drehen
    .w-more = Mehr
    .w-review = Prüfen
    .w-comments = Kommentare
    .w-contents = Inhalt
    .w-signing = Unterschreiben
    .w-this-pdf-has-no-contents = Dieses PDF hat keinen Inhalt.
    .w-page-2 = Seite { $page }
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern
    .w-fit-width = Breite einpassen
    .w-fit-page = Ganze Seite einpassen
    .w-found-in-pages = { $matches } auf { $pages_with } Seiten gefunden
    .w-reading-pages = · { $unread } Seiten werden gelesen
    .w-duplicate-this-page = Diese Seite duplizieren
    .w-move-to-the-front = Nach vorne verschieben
    .w-move-to-the-end = Nach hinten verschieben
    .w-delete-this-page = Diese Seite löschen
    .w-copy = Kopieren
    .w-select-all = Alles auswählen
    .w-dismiss = Verwerfen
    .w-links = Links
    .w-page-3 = Seite { $to }
    .w-page-4 = Seite { $page }
    .w-tools = Werkzeuge
    .w-select = Auswählen
    .w-highlight = Markieren
    .w-underline = Unterstreichen
    .w-strike-through = Durchstreichen
    .w-squiggle = Wellenlinie
    .w-draw = Zeichnen
    .w-note = Notiz
    .w-text-box = Textfeld
    .w-sign = Unterschreiben
    .w-colour = Farbe
    .w-rail = Seitenleiste
    .w-save-a-copy = Kopie speichern
    .w-page-of = Seite { $page_now } von { $page_count }
    .w-page-of-2 = Seite { $value } von { $len }

d-lfkj2ymr = Feldzeile
    .w-the-document-locks-this-field = Das Dokument sperrt dieses Feld.
    .w-the-document-needs-this-field-filled = Das Dokument braucht dieses Feld ausgefüllt.
    .w-choose = Wählen

d-zcxne7qg = Kommentarbereich
    .w-nothing-is-said-about-this-pdf = Über dieses PDF ist noch nichts gesagt.
    .w-say-something-about-this-pdf = Sag etwas über dieses PDF

d-obtjfw3f = Kommentarkarte
    .w-a-note = Eine Notiz
    .w-resolved = Erledigt
    .w-more = Mehr
    .w-reopen = Wieder öffnen
    .w-resolve = Erledigen
    .w-delete = Löschen
    .w-page = Seite { $source_page }
    .w-reply = Antworten
    .w-edit-comment = Kommentar bearbeiten
    .w-edit-the-comment = Den Kommentar bearbeiten
    .w-save = Speichern
    .w-cancel = Abbrechen
    .w-delete-reply = Antwort löschen

d-ioakkrvn = Unterschriftsbereich
    .w-sign-as = Unterschreiben als
    .w-your-name = Dein Name
    .w-draw-a-signature = Eine Unterschrift zeichnen
    .w-sign-here = Hier unterschreiben
    .w-keep-this-signature = Diese Unterschrift behalten
    .w-clear = Leeren
    .w-place-it = Platzieren
    .w-choose-sign-then-press-the-page = Wähle Unterschreiben und drücke dann auf die Seite, auf die die Unterschrift soll.
    .w-placed = Platziert
    .w-no-signature-is-placed-yet = Es ist noch keine Unterschrift platziert.
    .w-on-page = { $signer } auf Seite { $value }, { $date }
    .w-delete-signature = Unterschrift löschen

d-7gcxsbu5 = Links
    .doc = Die Links auf seinen Seiten, wie die Datei sie nennt.

d-qko77qye = Abschnitte
    .doc = Seine Wörter, wie eine Person sie auswählt, ein Abschnitt einer Zeile nach dem anderen: der Rahmen jedes Abschnitts in Bruchteilen des Seitenrahmens (x, y, Breite, Höhe, je vier, in Lesereihenfolge) und seine Wörter.

d-mxsaptcd = Wörter der Abschnitte

d-mqzjtuwo = Link
    .doc = Ein Link auf einer Seite: die Fläche, die er abdeckt, und wohin er führt.
    .noun = { $count ->
        [one] link
       *[other] links
    }
    .untitled = Link

d-xivruibf = Seite

d-sepq4iwp = Rahmen
    .doc = Seine Fläche in Punkten der Seite, wie die Datei sie zeichnet: x, y, Breite, Höhe.

d-yfuvjgjt = Art
    .doc = Wohin er führt: zu einer Seite dieses Dokuments, einer Adresse oder einer Datei.

d-lzpsucsi = Ziel
    .doc = Die Seite, zu der er führt, ab 1 gezählt, oder die Adresse oder der Pfad, den er nennt.

d-mqzjtuwo-filter-page = Seite

d-ndpbjbuv = Abschnitte
    .doc = Die Wörter einer Seite als Abschnitte, die eine Person auswählt: der Rahmen und die Wörter jedes Abschnitts, die Rahmen in Bruchteilen des Seitenrahmens, in Lesereihenfolge.
    .noun = { $count ->
        [one] run
       *[other] runs
    }

d-fmkeszhi = Rahmen

d-awfhznwx = Texte

d-fk2fxwca = Links einer Seite lesen
    .doc = Liest die Links, die die Seite trägt: jede Fläche, die einer abdeckt, und wohin er führt.

d-fk2fxwca-p = P

d-35vrqgsr = In einem PDF suchen
    .doc = Sucht die Wörter `query` in den Seiten des Dokuments: jede Seite, die sie enthält, mit ihrer Häufigkeit, als `Page 3: 2`.
    .w-name-the-words-to-find = Gib die Wörter an, die gesucht werden sollen

d-35vrqgsr-query = Suche

d-ul7cj4d4 = Die Auswahl kopieren
    .doc = Kopiert die Wörter, die eine Seite zwischen zwei Abschnitten zeigt, so wie Auswählen und Kopieren es tut.

d-ul7cj4d4-p = P

d-ul7cj4d4-from = Von

d-ul7cj4d4-to = Bis

d-xd7zuh3x = Den Text der Seite kopieren
    .doc = Kopiert jedes Wort, das eine Seite zeigt.

d-xd7zuh3x-p = P

d-2uq5pptz-stroke = Strich

d-hyvoem6y = In einem PDF suchen~present
