# Writer auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-word-processor = Writer

d-so-tilo-word-processor-keywords = word, docs, dokument, dokumente, writer, microsoft word, docx, odt, rtf, libreoffice writer, wordpad, textverarbeitung, schreiben, brief, lebenslauf, bericht

d-swntfsuf = Dokument
    .doc = Ein Dokument: was eine Person schreibt, zusammen mit allen anderen daran.
    .noun = { $count ->
        [one] Dokument
       *[other] Dokumente
    }
    .untitled = Unbenanntes Dokument

d-76ypz5mr = Titel

d-myqdqp23 = Text

d-6dmyhevh = Oberer Rand
    .doc = Die Ränder der Seite in Punkten bei 100 %: wo der Text beginnt und endet.

d-5kuottk3 = Rechter Rand

d-u7wfrpxp = Unterer Rand

d-tim24jvi = Linker Rand

d-5lzglfac = Papier
    .doc = Das Papier, auf das die Seite gesetzt ist, wie es die Leiste anbietet: A4, Letter oder Legal.

d-wdffnjds = Begonnen
    .doc = Wie der Text begonnen wurde: als leere Seite, Brief, Bericht, Lebenslauf oder Besprechungsnotizen.

d-4hldj6md = Reaktionen
    .doc = Wer auf das Dokument mit welchem Emoji reagiert hat: die Person und der Agent, für die jede Pille in der Leiste des Fensters steht, und wessen eigene Reaktion ein Druck darauf zurücknimmt.

d-mgawklxl = Hinweis
    .doc = Warum das zuletzt vom Dokument Verlangte nicht geschah, etwa eine Datei, die kein Codec liest; leer, solange nichts schiefging.

d-mpu4ekps = Gewählt
    .doc = Die Adresse des Bildes, das die Person zuletzt zum Einfügen gewählt hat, und wie oft sie eines gewählt hat: die Seite gibt beides dem Editor, der das Bild dort einfügt, wo sie schreibt.

d-po3yh2xl = Gewählt-Zähler

d-swntfsuf-filter-chosen_count = Gewählt-Zähler

d-swntfsuf-filter-margin_top = Oberer Rand

d-swntfsuf-filter-margin_right = Rechter Rand

d-swntfsuf-filter-margin_bottom = Unterer Rand

d-swntfsuf-filter-margin_left = Linker Rand

d-bzk5q6y4 = Neues Dokument
    .doc = Beginnt das Dokument von vorn: eine leere Seite oder das Markdown, mit dem eine der Vorlagen beginnt.

d-bzk5q6y4-template = Vorlage

d-3wjnu2o4 = Hinweis ausblenden
    .doc = Nimmt den Grund weg, den die Seite über dem Text zeigt: die Person hat ihn gelesen.

d-ugxxfqvv = Absatz anhängen
    .doc = Hängt einen Absatz `text` an das Ende des Dokuments, wie Markdown ihn sagt.
    .w-the-paragraph-needs-text = Der Absatz braucht Text

d-ugxxfqvv-text = Text

d-wpgddfh2 = Inhalt ersetzen
    .doc = Ersetzt den ganzen Inhalt aus Markdown: Überschriften, Listen, Pipe-Tabellen, Betonung und Links.

d-wpgddfh2-markdown = Markdown

d-6ntiozxy = Das Dokument lesen
    .doc = Was das Dokument als Markdown sagt, bereit für einen Agenten oder eine Datei.

d-wzulaefw = Text ersetzen
    .doc = Findet `find` im Dokument und ersetzt das erste Vorkommen, oder jedes, durch `replace`.
    .w-name-the-text-to-find = Nennen Sie den zu findenden Text
    .w-the-document-does-not-hold = Das Dokument enthält { $find } nicht

d-wzulaefw-find = Suchen

d-wzulaefw-replace = Ersetzen

d-wzulaefw-all = Alle

d-zmtcru23 = Tabelle einfügen
    .doc = Fügt eine leere Tabelle mit `rows` Zeilen und `columns` Spalten ans Ende des Textes ein, ihre Kopfzeile die erste Zeile.

d-zmtcru23-rows = Zeilen

d-zmtcru23-columns = Spalten

d-lr3udps3 = Zeile hinzufügen
    .doc = Fügt eine Zeile wie die letzte der ersten Tabelle des Dokuments darunter ein.
    .w-the-document-holds-no-table = Das Dokument enthält keine Tabelle

d-vow4yaii = Spalte hinzufügen
    .doc = Fügt jeder Zeile der ersten Tabelle des Dokuments eine Zelle hinzu.
    .w-the-document-holds-no-table = Das Dokument enthält keine Tabelle

d-ch3lkk4w = Zeile löschen
    .doc = Löscht eine Zeile der ersten Tabelle des Dokuments, von oben ab 1 gezählt.

d-ch3lkk4w-row = Zeile

d-ak6dfurk = Spalte löschen
    .doc = Löscht eine Spalte der ersten Tabelle des Dokuments, von links ab 1 gezählt.

d-ak6dfurk-column = Spalte

d-jwhhfb6k = Tabelle löschen
    .doc = Löscht die erste Tabelle des Dokuments mit allem, was sie enthält.

d-vtp2tggc = Seitenränder setzen
    .doc = Setzt die Ränder der Seite in Punkten von ihren Kanten.

d-vtp2tggc-top = Oben

d-vtp2tggc-right = Rechts

d-vtp2tggc-bottom = Unten

d-vtp2tggc-left = Links

d-jm653hke = Papier setzen
    .doc = Setzt das Papier, auf das die Seite geschrieben ist: A4, Letter oder Legal.

d-jm653hke-paper = Papier

d-5en3y2gh = Inhaltsverzeichnis erzeugen
    .doc = Schreibt ein Inhaltsverzeichnis an den Anfang des Dokuments, jede Überschrift eine Zeile darin.
    .w-add-headings-first = Fügen Sie zuerst Überschriften hinzu

d-jni6g54e = Hyperlink setzen
    .doc = Gibt den Worten des Dokuments einen Link an seinem Ende, der `text` liest oder die Adresse selbst.
    .w-name-the-address-the-link-goes = Nennen Sie die Adresse, zu der der Link führt

d-jni6g54e-target = Ziel

d-jni6g54e-text = Text

d-exhyhsgg = Seitenumbruch einfügen
    .doc = Fügt am Ende des Textes einen Seitenumbruch ein.

d-fuzgbywh = Einfügen
    .doc = Setzt `markdown` ans Ende des Textes, wie ein Einfügen es tut.
    .w-nothing-to-paste = Nichts zum Einfügen

d-fuzgbywh-text = Text

d-fuzgbywh-markdown = Markdown

d-wnzg47ko = Auswahl ersetzen
    .doc = Ersetzt den ganzen Inhalt durch `text`, als Markdown.

d-wnzg47ko-text = Text

d-c3edrhdp = Bild einfügen
    .doc = Setzt ein Bild ins Dokument, wo der Text endet: die Person wählt die Datei, und das Dokument behält die Adresse ihres Inhalts statt des Bildes.

d-c3edrhdp-image = Bild

d-hi6fm3p5 = Bild teilen
    .doc = Das erste Bild, das das Dokument zeigt, als Inhalt, den eine andere App nehmen kann.
    .w-the-document-shows-no-picture = Das Dokument zeigt kein Bild

d-lxvnkazw = Bild löschen
    .doc = Löscht das Bild, dessen Adresse `image` ist.
    .w-name-the-picture-to-delete = Nennen Sie das zu löschende Bild

d-lxvnkazw-image = Bild

d-5huipo2c = Reagieren
    .doc = Fügt dem Dokument Ihre Reaktion mit einem Emoji hinzu oder nimmt sie zurück, wenn Sie schon damit reagiert hatten: ein Druck auf eine der Pillen in der Leiste oder die Auswahl, die die Hinzufügen-Schaltfläche geöffnet hat.

d-5huipo2c-emoji = Emoji

d-tb4ewbef = Kommentar
    .doc = Sagt etwas über das Dokument, im Gespräch unter seiner Seite.
    .did = kommentiert
    .w-write-a-comment-first = Schreiben Sie zuerst einen Kommentar

d-tb4ewbef-text = Text

d-efbp2k4u = Dokument öffnen
    .doc = Öffnet eine Datei im Dokument: eine Word-, OpenDocument-, RTF-, EPUB- oder HTML-Datei, die die Codecs des Geräts als das Markdown lesen, das der Editor zeigt, Markdown wie es steht, und reinen Text als einen Absatz je Zeile.

d-efbp2k4u-file = Datei

d-glswnmoj = Das Dokument exportieren
    .doc = Das Dokument als Markdown-Datei, nach seinem Titel benannt.

d-r5rzmgc2 = Als reinen Text exportieren
    .doc = Das Dokument als reine Textdatei: seine Worte, ein Absatz je Zeile.

d-cvk3spaq = Das Dokument exportieren
    .doc = Das Dokument als Word-, OpenDocument-, RTF-, EPUB- oder PDF-Datei, nach seinem Titel benannt: was eine Person jemandem schickt, der nicht in Tilo schreibt.

d-cvk3spaq-format = Format

d-fwmunpvi = Als HTML exportieren
    .doc = Das Dokument als eine HTML-Seite: seine Überschriften, Listen und Absätze und seine Worte.

d-yseg6olm = Eine Datei lesen
    .doc = Liest eine Datei und antwortet, was sie sagt, ohne das Dokument anzutasten: wie ein Agent ein Dokument liest, bevor er entscheidet, was damit zu tun ist.

d-yseg6olm-file = Datei

d-pnlur6tt = Absatz anhängen~present

d-ldyo3esg = Text ersetzen~present

d-53dvuo5l = Inhaltsverzeichnis erzeugen~present

d-axqdfnos = Hyperlink setzen~present

d-phfoyuxw = Einfügen~present

d-hmbclj7g = Bild teilen~present

d-acwew27t = Bild löschen~present

d-6mswb2vv = Kommentieren~present

d-xxexkmsk = Writer
    .w-bold = Fett
    .w-italic = Kursiv
    .w-underline = Unterstrichen
    .w-strikethrough = Durchgestrichen
    .w-superscript = Hochgestellt
    .w-subscript = Tiefgestellt
    .w-bulleted-list = Aufzählung
    .w-numbered-list = Nummerierte Liste
    .w-heading-1 = Überschrift 1
    .w-heading-2 = Überschrift 2
    .w-heading-3 = Überschrift 3
    .w-body-text = Fließtext
    .w-text = Text
    .w-insert-table = Tabelle einfügen
    .w-insert-image = Bild einfügen
    .w-insert-page-break = Seitenumbruch einfügen
    .w-table-of-contents = Inhaltsverzeichnis
    .w-styles = Stile
    .w-review = Prüfen
    .w-ruler = Lineal
    .w-outline = Gliederung
    .w-close = Schließen
    .w-margins = Ränder
    .w-set-up = Einrichtung
    .w-started-as-a-page = Begonnen als Seite „{ $started }“.
    .w-the-page-s-margins-and-paper = Die Ränder und das Papier der Seite gehören dem Dokument, auf jedem Gerät.
    .w-before = Davor
    .w-after = Danach
    .w-align = Ausrichtung
    .w-centre = Zentriert
    .w-justified = Blocksatz
    .w-indent = Einzug
    .w-face = Schriftart
    .w-sans = Serifenlos
    .w-serif = Serifen
    .w-mono = Dicktengleich
    .w-size = Größe
    .w-text-colour = Textfarbe
    .w-highlight = Markierung
    .w-margins-in-points = Ränder in Punkten
    .w-left = Links
    .w-top = Oben
    .w-right = Rechts
    .w-bottom = Unten
    .w-paper = Papier
    .w-a4 = A4
    .w-letter = Letter
    .w-legal = Legal
    .w-paragraph = Absatz
    .w-body = Fließtext
    .w-bulleted = Aufzählung
    .w-numbered = Nummeriert
    .w-the-document-could-not-be-opened = Das Dokument konnte nicht geöffnet werden
    .w-untitled-document = Unbenanntes Dokument
    .w-start-writing = Schreiben beginnen
    .w-image = Bild

d-qgcdvpjd = Dateimenü
    .w-file = Datei
    .w-open = Öffnen…
    .w-save-as-markdown = Als Markdown speichern…
    .w-save-as-plain-text = Als reinen Text speichern…
    .w-export-as-html = Als HTML exportieren…
    .w-save-as-word-docx = Als Word (.docx) speichern…
    .w-save-as-opendocument-odt = Als OpenDocument (.odt) speichern…
    .w-save-as-rich-text-rtf = Als Rich Text (.rtf) speichern…
    .w-save-as-epub = Als EPUB speichern…
    .w-export-as-pdf = Als PDF exportieren…
    .w-start-from-a-letter = Mit einem Brief beginnen
    .w-start-from-a-report = Mit einem Bericht beginnen
    .w-start-from-a-r-sum = Mit einem Lebenslauf beginnen
    .w-start-from-meeting-notes = Mit Besprechungsnotizen beginnen

d-m6f4uc6u = Prüfbereich
    .w-a-person-or-agent-that-suggests = Eine Person oder ein Agent, die oder der eine Änderung an den Worten vorschlägt, legt sie hier ab, damit Sie sie annehmen.

d-aeni6cqw = Statusleiste
    .w-words = { $words ->
        [one] { $words } Wort
       *[other] { $words } Wörter
    }
    .w-characters = { $len } Zeichen
