# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-typst-editor = Typst

d-so-tilo-typst-editor-keywords = typst, typesetting, typeset, document, paper, pdf, editor, markup, satz, dokument, schreiben, typst-editor

d-j7sx7zxi = Problem
    .doc = Ein Problem, das der Schriftsatz gemeldet hat: was es sagt und wo im Quelltext es steht, Zeilen und Spalten ab 1 (0, wo der Übersetzer keine nennt).
    .noun = { $count ->
        [one] Problem
       *[other] Problem
    }

d-yt5nyujj = Schwere

d-i3zcji5u = Meldung

d-od6pdrjz = Zeile

d-xrgk5gr2 = Spalte

d-f7mcrdxf = Endzeile

d-7j5cjuet = Endspalte

d-u6wlx35y = Hinweise

d-ds45ukw2 = Seite
    .doc = Eine Seite des gesetzten Dokuments: wie groß sie ist, eine Kennung für das, was sie zeichnet, und die Bilder, durch die sie gezeigt wird: die Seite in der Breite, die das Fenster verlangt, und ihr Vorschaubild.
    .noun = { $count ->
        [one] Seite
       *[other] Seite
    }
    .untitled = Seite

d-cgrnobye = Breite
    .doc = Ihre Breite und Höhe, wie das Dokument sie anlegt, in Punkten.

d-r4wxummm = Höhe

d-6r2kgenm = Aussehen
    .doc = Was sie zeichnet, wie der Schriftsatz es benennt: eine Seite, deren Kennung bleibt, behält ihr Bild über ein Setzen hinweg, das sie unverändert lässt.

d-7ry6v2yt = Bild
    .doc = Die Adresse ihres Bildes in der Breite, die das Fenster verlangt, `tilo-blob://…`; leer, bis es gezeichnet ist.

d-ycgenfy7 = Bildbreite
    .doc = Die Breite, in der ihr Bild gezeichnet wurde, in Pixeln; 0, bis es gezeichnet ist.

d-wfjgjbj4 = Vorschaubild
    .doc = Die Adresse ihres Vorschaubildes; leer, bis es gezeichnet ist.

d-yusfx3a6 = Vorschaubildbreite
    .doc = Die Breite, in der ihr Vorschaubild gezeichnet wurde, in Pixeln; 0, bis es gezeichnet ist.

d-jnlperg3 = Überschrift
    .doc = Eine Überschrift des Dokuments, aus der Syntax seines Quelltextes gelesen: ihre Zeile ab 1, wie tief sie steht und ihre Wörter.
    .noun = { $count ->
        [one] Überschrift
       *[other] Überschrift
    }

d-ougql7d2 = Titel

d-ucauvawo = Zeile

d-t2356zsr = Ebene

d-jnlperg3-filter-line = Line

d-jnlperg3-filter-level = Level

d-ks73ekfk = Dokument
    .doc = Ein Typst-Dokument: sein Quelltext, die Datei, als die es gespeichert wird, was der Schriftsatz dazu sagte und die Seiten, die es anlegt.
    .noun = { $count ->
        [one] Dokument
       *[other] Dokument
    }
    .untitled = Unbenannt.typ

d-ojyy5fog = Name
    .doc = Wie das Dokument heißt.

d-wigkyjqk = Quelltext
    .doc = Sein Quelltext: der Typst-Text, Zeichen für Zeichen zusammengeführt.

d-zlwvg46c = Setzen
    .doc = Der Quelltext, den das letzte Setzen gelesen hat; einer, dessen Quelltext darüber hinausging, wird erneut gesetzt.

d-s3mabp2s = Setzt
    .doc = Ob das letzte Setzen ein Dokument fand (falsch, solange es sich nicht setzen lässt).

d-d7qirnkw = Dauer in ms
    .doc = Wie lange das letzte Setzen dauerte, in Millisekunden.

d-hk3xko3g = Problem
    .doc = Warum es sich nicht setzen lässt, in den Worten des Schriftsatzes; leer, solange es geht.

d-rrdheos7 = Geschrieben
    .doc = Der Quelltext, der zuletzt in eine vom Menschen gewählte Datei geschrieben wurde.

d-bxbra2li = Probleme
    .doc = Was der Schriftsatz fand, blockierende Probleme zuerst.

d-jsru5ywo = Seiten
    .doc = Was das Dokument anlegt, in seiner Reihenfolge.

d-cltpfy4y = Überschriften
    .doc = Seine Überschriften, aus der Syntax des Quelltextes gelesen.

d-xn4pkllr = PDF
    .doc = Die Adresse des PDFs, das das letzte Setzen schrieb, und wie es heißt; leer, bis eins es tat.

d-e3bwnsci = PDF-Name

d-akp7jws5 = Angezeigte Breite
    .doc = Die Breite, in der Seiten gezeichnet werden, in Pixeln: die Vergrößerung der Vorschau, 0 für die Anpassung des Fensters.

d-sfn7naaa = Ansicht
    .doc = Was das Fenster zeigt: `split` (den Quelltext neben der Seite), `editor`, `preview`, `problems` oder `outline`.

d-g6gxruuq = Sprung
    .doc = Wohin der Editor springt, als `line:column:count` ab 1; leer für nirgendwohin.

d-xad4v4m3 = Sprünge
    .doc = Wie viele Stellen angesprungen wurden: jede Bitte zählt, damit dieselbe Stelle erneut erscheint.

d-fijcb57x = Leseseite
    .doc = Die Seite, die der Mensch liest, damit er zu ihr zurückkehrt.

d-tbdqtt5g = Veraltet
    .doc = Ob die gezeigten Seiten die letzten gesetzten sind, während der Quelltext es nicht mehr ist.

d-alhhxqdi = Bibliothek
    .doc = Die Typst-Dokumente des Arbeitsbereichs.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliothek
    }

d-jdegntgs = Dokumente

d-pzdvdyxx = Setzbitte
    .doc = Die Bitte, die ein Setzen stellt.
    .noun = { $count ->
        [one] compile ask
       *[other] compile asks
    }

d-d2jzgpyx = Quelltext

d-w5eejkzm = Seitenbitte
    .doc = Die Bitte, die das Bild einer Seite stellt.
    .noun = { $count ->
        [one] page ask
       *[other] page asks
    }

d-a66w2zy6 = Quelltext

d-6jynr6xk = Seite

d-m7iybhvb = Breite

d-w5eejkzm-filter-page = Page

d-w5eejkzm-filter-width = Width

d-iyeymqh4 = Textbitte
    .doc = Die Bitte, die die Wörter einer Seite stellt.
    .noun = { $count ->
        [one] text ask
       *[other] text asks
    }

d-rydvb6fi = Quelltext

d-rqodn5jw = Seite

d-iyeymqh4-filter-page = Page

d-3naxgrbf = Gliederungsbitte
    .doc = Die Bitte, die die Gliederung stellt.
    .noun = { $count ->
        [one] outline ask
       *[other] outline asks
    }

d-wevbiksf = Quelltext

d-zjs45sex = Seitenzeile
    .doc = Was der Schriftsatz über eine Seite antwortete.
    .noun = { $count ->
        [one] page row
       *[other] page rows
    }

d-mtlxa3ps = Breite

d-ay4kqc7o = Höhe

d-ehra7vlr = Aussehen

d-zjs45sex-filter-width = Width

d-zjs45sex-filter-height = Height

d-36735ayx = Problemzeile
    .doc = Was der Schriftsatz über ein Problem antwortete.
    .noun = { $count ->
        [one] problem row
       *[other] problem rows
    }

d-7vzeic6n = Art

d-55dyzse3 = Meldung

d-bzomsyem = Zeile

d-vwara56h = Spalte

d-lewaebu2 = Endzeile

d-unj2zbun = Endspalte

d-wakty2b2 = Hinweise

d-36735ayx-filter-line = Line

d-36735ayx-filter-column = Column

d-36735ayx-filter-end_line = End line

d-36735ayx-filter-end_column = End column

d-kc7rlieo = Setzergebnis
    .doc = Was der Schriftsatz über ein Setzen antwortete.
    .noun = { $count ->
        [one] compiled row
       *[other] Setzergebnisse
    }

d-ffvtdeuv = Fertig

d-t5b6wta3 = Dauer in ms

d-siiy7bea = Pdf url
    .doc = The address of the PDF the typeset wrote, and what it is called; none while it wrote none.

d-htehc63o = PDF-Name

d-63pe7jqy = Pdf size

d-uryvrrzv = PDF-Problem

d-kc7rlieo-filter-ok = Ok

d-kc7rlieo-filter-elapsed_ms = Elapsed ms

d-kc7rlieo-filter-pdf_size = Pdf size

d-ymzwzbqi = Überschriftszeile
    .doc = Was der Schriftsatz über die Gliederung antwortete.
    .noun = { $count ->
        [one] heading row
       *[other] heading rows
    }

d-3olgjoh6 = Zeile

d-lcdznshm = Ebene

d-pq3mu5rf = Titel

d-ymzwzbqi-filter-line = Line

d-ymzwzbqi-filter-level = Level

d-ewmnrcj6 = Glyphenzeile
    .doc = Eine platzierte Glyphe einer Seite.
    .noun = { $count ->
        [one] cell row
       *[other] cell rows
    }

d-dsvn4b2o = Text

d-erwowgdw = X0

d-txaxsk54 = X1

d-vobo3z4e = Oben

d-w6ydh2nv = Unten

d-73rf4vlf = Zeile

d-ewmnrcj6-filter-x0 = X0

d-ewmnrcj6-filter-x1 = X1

d-ewmnrcj6-filter-top = Top

d-ewmnrcj6-filter-bottom = Bottom

d-ewmnrcj6-filter-line = Line

d-5ox5sdk3 = Schwere
    .doc = Wie sehr ein Problem wiegt, das der Schriftsatz gefunden hat.

d-6c2uqjwj = Fehler

d-gj5raevk = Warnung

d-6wkf2pdj = Ein Typst-Dokument öffnen
    .doc = Eine Typst-Datei öffnet im Editor: Files, das Dateisystem des Menschen und die Suche des Systems reichen sie als die Datei weiter, die sie ist, und sie öffnet als das Dokument, das diese Wörter sind.
    .did = hat ein Typst-Dokument geöffnet

d-6wkf2pdj-file = File

d-5ipxgvaw = Neues Typst-Dokument
    .doc = Ein neues, leeres Dokument.
    .did = hat ein Typst-Dokument erstellt

d-5ipxgvaw-name = Name

d-5ipxgvaw-source = Source

d-jdb3eb6g = Dokument schließen
    .doc = Ein Dokument verlässt den Arbeitsbereich.
    .did = hat ein Dokument geschlossen

d-jdb3eb6g-d = D

d-gxzucf5i = Das Dokument setzen
    .doc = Setzt das Dokument, wie es das Fenster und der Hintergrund tun.
    .did = hat ein Dokument gesetzt

d-tqoyg4ac = Das Dokument setzen, wenn es sich ändert
    .doc = Setzt das Dokument, wenn sein Quelltext über das letzte Setzen hinausging.

d-6eqv5vrp = Take problems
    .doc = Lays the problems a typeset reported over the ones the document held, blocking ones first.

d-6eqv5vrp-rows = Rows

d-sk6tqdpi = Seiten übernehmen
    .doc = Legt die gesetzten Seiten über die, die das Dokument hielt: eine Seite, die zeichnet, was sie zeichnete, behält ihr Bild, und eine, die anderes zeichnet, wird neu gezeichnet.

d-sk6tqdpi-rows = Rows

d-cxvgocg6 = Gliederung lesen
    .doc = Liest die Überschriften des Dokuments aus der Syntax seines Quelltextes.

d-2pimchjc = Eine Seite zeichnen
    .doc = Zeichnet eine Seite `width` Pixel breit und ihr Vorschaubild.

d-2pimchjc-page = Page

d-2pimchjc-width = Width

d-ebsbeihs = Die nächste Seite zeichnen
    .doc = Zeichnet die nächste Seite in der Breite, die das Fenster verlangt, eine pro Sekunde: ein langes Dokument füllt sich Seite für Seite, ohne dass jemand darauf wartet.

d-63azmqnp = Den Quelltext neben der Seite zeigen
    .doc = Zeigt den Quelltext neben der Seite.

d-2hjahx4h = Den Quelltext zeigen
    .doc = Zeigt den Quelltext allein.

d-ghqidq45 = Die Seite zeigen
    .doc = Zeigt die Seite allein.

d-iv4urjhq = Probleme
    .doc = Zeigt, was der Schriftsatz fand, oder kehrt zur Seite zurück.

d-qpw3jv76 = Gliederung
    .doc = Zeigt die Überschriften des Dokuments oder kehrt zur Seite zurück.

d-qwut2qdz = Vergrößern
    .doc = Zeichnet die Seiten eine Stufe breiter.

d-abulifuu = Verkleinern
    .doc = Zeichnet die Seiten eine Stufe schmaler.

d-p2dixf6c = An die Seitenbreite anpassen
    .doc = Zeichnet die Seiten so breit wie der Raum, in dem sie gezeigt werden.

d-p2dixf6c-width = Width

d-nsttu26n = Zu einer Seite blättern
    .doc = Blättert zu einer Seite, damit der Mensch zu ihr zurückkehrt.

d-nsttu26n-page = Page

d-2jljvrjq = Zum Problem springen
    .doc = Springt zu der Stelle, die ein Problem nennt.

d-2jljvrjq-problem = Problem

d-ftnuj3xb = Zur Überschrift springen
    .doc = Springt zu der Zeile, auf der eine Überschrift steht.

d-ftnuj3xb-heading = Heading

d-pl3ixlbr = Springen
    .doc = Setzt die Schreibmarke auf `line`:`column` und zeigt sie, zählt jede Bitte, damit dieselbe Stelle erneut erscheint.

d-pl3ixlbr-line = Line

d-pl3ixlbr-column = Column

d-knbolpey = Das Dokument speichern
    .doc = Das Dokument als die Datei, als die es gespeichert wird, als geschrieben vermerkt.
    .did = hat ein Dokument gespeichert

d-ix3mfzzu = Das PDF exportieren
    .doc = Das PDF, das das letzte Setzen schrieb, als die Datei, die ein Mensch speichert.

d-uty64k76 = Die markierten Wörter kopieren
    .doc = Kopiert die Wörter, die ein Mensch auf einer Seite markiert hat.

d-uty64k76-page_at = Page at

d-uty64k76-from = From

d-uty64k76-to = To

d-jtqxb525 = Die Wörter der Seite kopieren
    .doc = Kopiert alle Wörter einer Seite.

d-jtqxb525-page_at = Page at

d-s3ah6rfa = Page text
    .doc = All of a page's words, as the typesetter read them.

d-s3ah6rfa-page_at = Page at

d-cczhygw4 = Selection text
    .doc = The words of a page inside the box from `start` to `end`.

d-cczhygw4-page_at = Page at

d-cczhygw4-start = Start

d-cczhygw4-end = End

d-f2a66q6a = Seitenwörter
    .doc = Die platzierten Glyphen einer Seite, wie der Schriftsatz sie las.

d-f2a66q6a-page_at = Page at

d-gqdd5xo6 = Seiten gesagt
    .w-1-page = 1 Seite
    .w-pages = { $len } Seiten

d-2gt3amcp = Gefundene Probleme
    .w-1-problem = 1 Problem
    .w-problems = { $errors } Probleme

d-gog7heib = Bibliothek
    .w-no-typst-documents-yet = Noch keine Typst-Dokumente
    .w-open-one-from-this-device-or = Öffne eins von diesem Gerät oder beginne ein neues.
    .w-new-document = Neues Dokument

d-5dt2n5ip = Dokumentzeile
    .w-not-typeset-yet = Noch nicht gesetzt

d-ntb4stnt = Bibliotheksleiste
    .w-open = Öffnen
    .w-new-document = Neues Dokument

d-zapqqrte = Editor
    .w-the-page-shown-is-from-before = Die gezeigte Seite stammt von vor dieser Änderung
    .w-this-document-does-not-typeset = Dieses Dokument lässt sich nicht setzen

d-32mzmsd6 = Editorleiste
    .w-open = Öffnen
    .w-save = Speichern
    .w-export-pdf = PDF exportieren
    .w-typeset = Setzen
    .w-problems = Probleme
    .w-outline = Gliederung
    .w-the-source-beside-the-page = Der Quelltext neben der Seite
    .w-the-source = Der Quelltext
    .w-the-page = Die Seite
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern

d-cuwdvehn = Quelltextbereich
    .w-write-a-typst-document = Schreibe ein Typst-Dokument

d-rn3rpf4r = Statuszeile
    .w-document-name = Dokumentname
    .w-typeset-in-ms = gesetzt in { $took_ms } ms
    .w-lines = { $len } Zeilen
    .w-pdf-ready = PDF bereit

d-rwylofzj = Seitenbereich
    .w-page-of = Seite { $value } von { $len }
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern
    .w-fit-width = Breite anpassen
    .w-the-pages-thumbnails = Die Vorschaubilder der Seiten
    .w-typesetting-the-document = Das Dokument wird gesetzt
    .w-this-document-lays-out-no-pages = Dieses Dokument legt keine Seiten an
    .w-pages = Seiten
    .w-page = Seite { $index }
    .w-page-of-2 = Seite { $index } von { $len }
    .w-page-its-words = Seite { $index }, ihre Wörter
    .w-words-selected = Wörter markiert
    .w-copy = Kopieren
    .w-copy-the-page = Die Seite kopieren
    .w-clear = Aufheben

d-p2hycvhx = Problembereich
    .w-typesetting-the-document = Das Dokument wird gesetzt
    .w-no-problems = Keine Probleme
    .w-the-document-typesets-cleanly = Das Dokument setzt sich fehlerfrei.
    .w-line = Zeile { $line }:{ $column }

d-7loyjosy = Gliederungsbereich
    .w-no-headings = Keine Überschriften
    .w-a-heading-starts-with = Eine Überschrift beginnt mit `=`.
    .w-line = Zeile { $line }
