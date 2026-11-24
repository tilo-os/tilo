# Books auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-ebook = Bücher

d-defhqkyd = Buch
    .doc = Ein Buch im Regal: was sein Paket über es sagt, wo die Person darin ist, seine Kapitel in Lesereihenfolge und was sie darin markiert hat.
    .noun = { $count ->
        [one] Buch
       *[other] Bücher
    }
    .untitled = Unbenanntes Buch

d-zzi6jbdk = Titel

d-hzsemqyf = Autor

d-ywdncocc = Thema

d-nczhbvr6 = Sprache
    .doc = Die Sprache, in der das Buch geschrieben ist, für die Silbentrennung.

d-uufjkgr3 = Quelle
    .doc = Die Adresse des Inhalts der Datei: dieselbe Datei noch einmal zu öffnen öffnet dieses Buch.

d-t7fdn6od = Umschlag
    .doc = Die Adresse des Bildes seines Umschlags, wenn das Paket eines mitbringt.

d-sthxrdo4 = Hinzugefügt

d-s4t4ogtx = Kapitel

d-kckryjj5 = Markierungen

d-y3qlwotc = Lesefortschritt
    .doc = Wo jedes Mitglied im Buch ist: ein Eintrag je Leser, verändert beim Lesen und mit seiner Geschichte behalten, sodass das Gelesene der Person über ihre Geräte folgt und niemandes anderes Lesen in ihrem erscheint.

d-defhqkyd-filter-added = Hinzugefügt

d-ra6ct5pp = Kapitel
    .doc = Ein Kapitel eines Buchs: wie der Inhalt es nennt, und seine Wörter.
    .noun = { $count ->
        [one] Kapitel
       *[other] Kapitel
    }
    .untitled = Kapitel

d-njsapppc = Titel

d-hc6oa77y = Inhalt

d-awonaulm = Buch
    .doc = Das Buch, dessen Kapitel es ist: eine Suche, die eine Stelle findet, öffnet es mit dem Buch einen Schritt entfernt.

d-ra6ct5pp-filter-book = Buch

d-ujgcntpf = Markierung
    .doc = Eine Stelle, die eine Person markiert hat, als Anker für die Seite: der Block, in dem sie steht, ihr erstes und letztes Zeichen in den Wörtern des Blocks, und die Wörter selbst.
    .noun = { $count ->
        [one] Markierung
       *[other] Markierungen
    }

d-nsx4szpu = Art
    .doc = `bookmark`, `highlight`, `note` oder `comment`.

d-eu36rgok = Kapitel

d-kkwkq4sx = Block

d-c7gbrwjv = Anfang

d-gwydxwhz = Ende

d-pjwr2noa = Auszug

d-jlprlqyv = Farbe

d-q6o2tid6 = Text
    .doc = Was eine Notiz oder ein Kommentar sagt.

d-qzbrg6g6 = Erstellt

d-jt22d5yp = Zu
    .doc = Die Markierung, auf die sich ein Kommentar bezieht, nach ihrer id.

d-gjbrlgkj = Buch
    .doc = Das Buch, in dem die Markierung steht: eine Seite davon steht einen Schritt von der Stelle entfernt.

d-vpgm2esg = Abschnitte
    .doc = Die Bereiche, die die Stelle abdeckt, in Lesereihenfolge: die Wörter eines Blocks, oder mehrere, wenn sie einen Block überschreitet.

d-ujgcntpf-filter-highlights = Markierungen

d-ujgcntpf-filter-notes = Notizen

d-ujgcntpf-filter-bookmarks = Lesezeichen

d-kfdvrrjc = Lesefortschritt
    .doc = Wo ein Mitglied in einem Buch ist.
    .noun = { $count ->
        [one] Lesefortschritt
       *[other] Lesefortschritte
    }

d-fw6cx373 = Wer
    .doc = Der Leser, der dieser Eintrag ist: jedes Mitglied hat seinen eigenen.

d-hrlsiabz = Kapitel
    .doc = Das offene Kapitel von 0 gezählt, der Block, bei dem seine Seite beginnt, die Seite darin, und wie weit unten die Spalte im fortlaufenden Modus steht (`0` oben bis `1` unten).

d-dozgglst = Block

d-j7r2dpph = Seite

d-t4pdkbb6 = Stelle

d-vpcehkho = Am
    .doc = Wann sie zuletzt gelesen haben.

d-kfdvrrjc-filter-who = Wer

d-kfdvrrjc-filter-chapter = Kapitel

d-kfdvrrjc-filter-block = Block

d-kfdvrrjc-filter-page = Seite

d-kfdvrrjc-filter-place = Stelle

d-kfdvrrjc-filter-at = Am

d-gtq4jwzo = Farbschema
    .doc = Eine Seitenfarbe, die ein Leser wählen kann: ihr Papier, ihre Schrift und die Farbe, die eine Auswahl annimmt.
    .noun = { $count ->
        [one] Farbschema
       *[other] Farbschemata
    }

d-yjaxgs5w = Name

d-zfe44ecr = Seite

d-giw4wwwc = Schrift

d-p6cg5shx = Akzent

d-2xhwxzlq = Mitgeliefert
    .doc = Ein Farbschema, das der Leser mitbringt: die Einstellungen behalten es und bieten keinen Weg, es zu löschen.

d-gtq4jwzo-filter-builtin = Mitgeliefert

d-5zqlbdnq = Gefunden
    .doc = Ein Buch, das Project Gutenberg auf eine Suche geantwortet hat und das auf das Herunterladen wartet.
    .noun = { $count ->
        [one] Gefundenes
       *[other] Gefundene
    }

d-cuehsj4a = Titel

d-5z23zsgs = Autor

d-5kd3qulh = Adresse
    .doc = Die Adresse, unter der sein EPUB zu lesen ist.

d-rgyfsse6 = Sprache

d-fhfwqqv3 = Suchbegriff
    .doc = Die Wörter, für die es gefunden wurde, sodass eine neue Suche die alten Antworten wegnimmt.

d-in25mwb6 = Überwacht
    .doc = Ein Ordner auf einem Gerät, den die Bibliothek überwacht: jedes EPUB darin, jetzt oder später, wird ein Buch.
    .noun = { $count ->
        [one] überwachter Ordner
       *[other] überwachte Ordner
    }

d-zfzcnyj7 = Pfad

d-l4rfzd5q = Marke
    .doc = Die Überwachung, die dieses Gerät hält, sodass eine Änderung am Ordner die Suche der Bibliothek startet.

d-vs5vpxpc = Gegeben
    .doc = Die Pfade, für die sie schon Bücher gab, sodass eine Suche nur das Neue liest.

d-dafaqxgm = Hinzugefügt

d-in25mwb6-filter-added = Hinzugefügt

d-srrcatfl = Bibliothek
    .doc = Die Bibliothek des Arbeitsbereichs: die Bücher im Regal, die Bücher, die eine Online-Suche gefunden hat, die Seitenfarben und wie eine Seite für die hier Lesenden gesetzt ist.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-rjqqjeka = Bücher

d-gark455v = Gefunden

d-b2rhddat = Farbschemata

d-sndaeoib = Textgröße
    .doc = Die Größe des Textes als Vielfaches der Lesegröße, zwischen 0,8 und 2.

d-3fnv4dcf = Zeilenabstand
    .doc = Der Zeilenabstand als Vielfaches des eigenen der Schrift, zwischen 0,95 und 1,6.

d-5umaklng = Schriftart
    .doc = Die Schriftart, in der die Seite gesetzt ist.

d-4nrbbruw = Blocksatz
    .doc = Ob Absätze wie in einem Buch im Blocksatz stehen statt im Flattersatz.

d-q2xeecyu = Farbschema
    .doc = Die gewählte Seitenfarbe, nach Namen; die Sepiaseite, wo keine genannt ist.

d-7iepiv5z = Entwurf
    .doc = Der Name, unter dem eine eigene Seitenfarbe angelegt wird, und ihre Papier- und Schriftfarben.

d-wc5z47pj = Eigene Seitenfarbe

d-yiozk22e = Eigene Schriftfarbe

d-dww4lfs6 = Ordnerentwurf
    .doc = Der Ordner, den das Feld der Einstellungen nennt: was "Überwachen" überwacht.

d-trpwrvrr = Ordner
    .doc = Die Ordner, die dieses Gerät nach Büchern überwacht.

d-srrcatfl-filter-text_size = Textgröße

d-srrcatfl-filter-line_spacing = Zeilenabstand

d-srrcatfl-filter-justified = Blocksatz

d-4xgt6ef3 = Füllen
    .doc = Womit eine neue Bibliothek beginnt: die Seitenfarben, die jeder Leser kennt, und die gewählte Sepiaseite.

d-euyhlcqi = Buch öffnen
    .doc = Fügt die EPUB-Datei `file` dem Regal hinzu und öffnet sie.
    .did = fügte ein Buch hinzu

d-euyhlcqi-file = Datei

d-umxlbp4p = Bücher ablegen
    .doc = Fügt die EPUB-Dateien hinzu, die eine Person auf dem Regal ablegt, jede wie `open_book` sie hinzufügt.

d-umxlbp4p-files = Dateien

d-vmft5yuq = Ordner überwachen
    .doc = Überwacht den Ordner `path` nach Büchern: jedes EPUB darin, und jedes, das ankommt, wird ein Buch, und die Stelle der Person bleibt, wie bei einem selbst geöffneten.
    .did = überwachte einen Ordner für Bücher
    .w-a-folder-is-watched-by-its = Ein Ordner wird über seinen Namen überwacht

d-vmft5yuq-path = Pfad

d-ap57yp5t = Ordner lesen
    .doc = Liest die EPUBs des Ordners `path` und gibt jedem Datei, die die Bibliothek noch nicht hält, ein Buch.

d-ap57yp5t-path = Pfad

d-3gvqo77t = Ordner geprüft
    .doc = Liest jeden überwachten Ordner erneut: was die Plattform ausführt, wenn sich einer ändert.

d-mjbtpsjr = Buch entfernen
    .doc = Nimmt ein Buch samt seinen Kapiteln und Markierungen aus dem Regal.
    .did = entfernte ein Buch

d-xiofub57 = Stelle merken
    .doc = Wo die lesende Person in `b` ist: ihr eigener Eintrag, an Ort und Stelle verändert, sodass seine Geschichte sagt, wo und wann sie gelesen hat.

d-xiofub57-who = Wer

d-xiofub57-chapter = Kapitel

d-xiofub57-block = Block

d-xiofub57-page = Seite

d-xiofub57-place = Stelle

d-liwol635 = Kapitel lesen
    .doc = Öffnet `c` in `b`: der Inhalt und wo die Person ist bleiben zusammen.

d-liwol635-who = Wer

d-liwol635-c = C

d-5vlcfzhe = Seite zeigen
    .doc = Zeigt Seite `page` des offenen Kapitels des Buchs, von 0 gezählt (im Doppelseitenmodus eine Doppelseite).

d-5vlcfzhe-who = Wer

d-5vlcfzhe-chapter = Kapitel

d-5vlcfzhe-block = Block

d-5vlcfzhe-page = Seite

d-hx37iaus = Stelle zeigen
    .doc = Zeigt die erste Seite des Kapitels `chapter` in `b`: ein Verweis auf eine Stelle landet dort.
    .w-a-passage-stands-at-a-block = Eine Stelle steht bei einem Block

d-hx37iaus-who = Wer

d-hx37iaus-chapter = Kapitel

d-hx37iaus-block = Block

d-algei7uu = Stelle merken
    .doc = Wie weit unten die Person in der Spalte gescrollt hat.

d-algei7uu-who = Wer

d-algei7uu-chapter = Kapitel

d-algei7uu-block = Block

d-algei7uu-page = Seite

d-algei7uu-place = Stelle

d-o6ubqh53 = Sichtbare Blöcke
    .doc = Merkt den Block, bei dem die Seite beginnt, während die Person in der Spalte scrollt, damit ein Lesezeichen dort landet, wo sie ist.

d-o6ubqh53-who = Wer

d-o6ubqh53-blocks = Blöcke

d-oyxehe6h = Textgröße setzen
    .doc = Setzt die Größe des Textes der Seite, innerhalb dessen, was sich lesen lässt.
    .did = änderte die Textgröße

d-oyxehe6h-size = Größe

d-x6jkfidp = Zeilenabstand setzen
    .doc = Setzt den Zeilenabstand der Seite.
    .did = änderte den Zeilenabstand

d-x6jkfidp-spacing = Abstand

d-x5djz5gg = Schriftart setzen
    .doc = Setzt die Schriftart, in der die Seite gesetzt ist.
    .did = wählte die Schriftart

d-x5djz5gg-face = Schriftart

d-wfzyizr4 = Blocksatz setzen
    .doc = Ob die Absätze der Seite im Blocksatz stehen.
    .did = änderte den Satz

d-wfzyizr4-on = An

d-iex2qhcv = Farbschema wechseln
    .doc = Geht zum nächsten Farbschema, das der Leser mitbringt: der Schnellwechsel seiner Leiste.
    .did = wechselte das Farbschema

d-uuny6qzl = Farbschema wählen
    .doc = Die Seitenfarbe, in der die Seite gesetzt ist, nach Namen.
    .did = wählte das Farbschema

d-uuny6qzl-name = Name

d-msgz7llr = Farbschema anlegen
    .doc = Eine eigene Seitenfarbe, aus einer Papier- und einer Schriftfarbe.
    .did = legte ein Farbschema an
    .w-a-palette-is-made-under-a = Ein Farbschema braucht einen Namen

d-msgz7llr-name = Name

d-msgz7llr-page = Seite

d-msgz7llr-ink = Schrift

d-g5klupmx = Farbschema löschen
    .doc = Löscht eine eigene Seitenfarbe.
    .did = löschte ein Farbschema
    .w-the-reader-s-own-palettes-stay = Die mitgelieferten Farbschemata bleiben

d-g5klupmx-scheme = Farbschema

d-km4hezzj = Markieren
    .doc = Markiert die Stelle im offenen Kapitel von `b`: ihre Bereiche, jeder `block, start, end`.
    .did = markierte eine Stelle

d-km4hezzj-chapter = Kapitel

d-km4hezzj-ranges = Bereiche

d-km4hezzj-color = Farbe

d-l2my2cwb = Notiz hinzufügen
    .doc = Schreibt eine Notiz zu der Stelle im offenen Kapitel von `b`.
    .did = schrieb eine Notiz

d-l2my2cwb-chapter = Kapitel

d-l2my2cwb-ranges = Bereiche

d-l2my2cwb-color = Farbe

d-l2my2cwb-text = Text

d-nvonvbnv = Lesezeichen setzen
    .doc = Setzt ein Lesezeichen dort, wo die Seite `block` im offenen Kapitel von `b` beginnt, oder nimmt es wieder weg.
    .did = setzte ein Lesezeichen oder nahm es weg

d-nvonvbnv-chapter = Kapitel

d-nvonvbnv-block = Block

d-y7xiu6cu = Kommentieren
    .doc = Kommentiert die Markierung `m` im Buch `b`.
    .did = kommentierte eine Markierung

d-y7xiu6cu-m = M

d-y7xiu6cu-text = Text

d-kyszv557 = Farbe ändern
    .doc = Färbt eine Markierung um, und die Notizen dazu.
    .did = änderte die Farbe einer Markierung

d-kyszv557-m = M

d-kyszv557-color = Farbe

d-3c4owmt6 = Markierung löschen
    .doc = Löscht eine Markierung, und die Kommentare dazu.
    .did = löschte eine Markierung

d-3c4owmt6-m = M

d-enyvtx2q = Bücher suchen
    .doc = Sucht bei Project Gutenberg nach Büchern zu `query`, und nimmt die Antworten einer früheren Suche weg.
    .did = suchte nach Büchern

d-enyvtx2q-query = Suchbegriff

d-sq35zdfy = Gefundenes herunterladen
    .doc = Lädt das Buch herunter, das eine Online-Suche gefunden hat, und öffnet es.
    .did = lud ein Buch herunter

d-sq35zdfy-f = F

d-ob6iyicm = Gefundenes leeren
    .doc = Nimmt die Antworten einer Online-Suche weg.
    .did = leerte die online gefundenen Bücher

d-oxd7kaud = Ordner überwachen~present

d-w4cgekcm = Stelle zeigen~present

d-vwfvgnvc = Farbschema anlegen~present

d-f2pjguwz = Farbschema löschen~present

d-yseykhty = Bibliothek
    .w-books = Bücher
    .w-add-a-book = Buch hinzufügen
    .w-what-the-library-shows = Was die Bibliothek zeigt
    .w-shelf = Regal
    .w-settings = Einstellungen
    .w-found-online = Online gefunden
    .w-no-books-yet = Noch keine Bücher
    .w-open-an-epub-file-and-it = Öffne eine EPUB-Datei; sie steht dann hier und merkt sich deine Stelle beim Lesen.

d-cmr5ibgv = Leseeinstellungen
    .w-reading-settings = Leseeinstellungen
    .w-text = Text
    .w-text-size = Textgröße
    .w-line-spacing = Zeilenabstand
    .w-face = Schriftart
    .w-the-reader-s-serif = Die Serifenschrift des Lesers
    .w-justified-paragraphs = Absätze im Blocksatz
    .w-watched-folders = Überwachte Ordner
    .w-a-folder-on-this-device = Ein Ordner auf diesem Gerät
    .w-watch-it = Überwachen
    .w-page-palettes = Seitenfarben
    .w-palette-name = Name der Farben
    .w-paper = Papier
    .w-ink = Schrift
    .w-use-these-colours = Diese Farben verwenden
    .w-new-epubs-here-become-books = Neue EPUBs hier werden Bücher
    .w-paper-ink = Papier { $page }, Tinte { $ink }
    .w-delete = { $name } löschen

d-jnthvtdd = Gefundene Bücher
    .w-find-a-book-online = Ein Buch online finden
    .w-title-author-or-subject = Titel, Autor oder Thema
    .w-search = Suchen
    .w-clear = Leeren
    .w-nothing-found-yet = Noch nichts gefunden
    .w-search-project-gutenberg-for-a-free = Suche bei Project Gutenberg nach einem freien Buch; es steht hier, bis du es herunterlädst.

d-tntic6tx = Buchseite
    .w-contents = Inhalt
    .w-justified = Blocksatz
    .w-reading-settings = Leseeinstellungen
    .w-bookmarks = Lesezeichen
    .w-notes = Notizen
    .w-highlight = Markieren
    .w-note = Notiz
    .w-bookmark = Lesezeichen
    .w-dismiss = Verwerfen
    .w-write-a-note-on-the-passage = Schreib eine Notiz zu dieser Stelle
    .w-save-note = Notiz speichern
    .w-previous = Zurück
    .w-page-of = Seite { $value } von { $pages }
    .w-next = Weiter
    .w-smaller-text = Kleinerer Text
    .w-larger-text = Größerer Text
    .w-change-the-page-palette = Seitenfarben wechseln
    .w-pages = Seiten
    .w-one-page = Eine Seite
    .w-two-pages = Zwei Seiten
    .w-scrolled = Fortlaufend
    .w-bookmark-this-page = Lesezeichen für diese Seite
    .w-how-far-through-this-chapter = Wie weit durch dieses Kapitel
    .w-this-chapter-s-marks = Markierungen dieses Kapitels
    .w-chapter = Kapitel { $value }
    .w-comment = Kommentieren
    .w-delete = Löschen
    .w-comment-on-the-highlight = Die Markierung kommentieren
    .w-send-comment = Kommentar senden

d-uxr7ymrf = Kapitelseite
    .w-read-in = In { $title } lesen
    .w-continuous = fortlaufend

d-qzus6seo = Markierungsseite
    .w-a-bookmark = Ein Lesezeichen
    .w-chapter = Kapitel { $value }
    .w-show-it-in = In { $title } zeigen

d-hjpohooo = Buchzeile

d-4dzykgkv = Markierungszeile

d-so-tilo-ebook-keywords = buch, bücher, ebook, ebooks, epub, lesen, leser, roman, bibliothek, markierungen, anmerkungen
