# Books auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

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
d-kfdvrrjc-filter-finished = Beendet

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
    .doc = Der Ordner, wie der Ordnerdialog des Systems ihn genannt hat: was `File.list` liest. Einen Ordner, den kein Dialog beantwortet hat, liest keine App.

d-idbgmutp = Name
    .doc = Wie der Ordner heißt, wo die Person ihn gewählt hat, damit die Einstellungen ihn zeigen können.

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

d-trpwrvrr = Ordner
    .doc = Die Ordner, die dieses Gerät nach Büchern überwacht.

d-srrcatfl-filter-text_size = Textgröße

d-srrcatfl-filter-line_spacing = Zeilenabstand

d-srrcatfl-filter-justified = Blocksatz
d-srrcatfl-filter-hide_finished = Beendete ausblenden

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
    .w-a-folder-is-watched-through-the = Ein Ordner wird über den Ordnerdialog des Systems überwacht

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

d-tavokm6y = Schriftartentitel
    .w-serif = Serif
    .w-sans = Sans
    .w-monospace = Monospace
    .w-libertinus = Libertinus
    .w-literata = Literata
    .w-eb-garamond = EB Garamond
    .w-source-serif = Source Serif

d-ngbcvx66 = Papier von
    .w-f5ead6 = #f5ead6

d-omlx7xsp = Tinte von
    .w-33291d = #33291d

d-o2c5xprv = Treffer gesagt
    .w-nothing-in-this-chapter = Nichts in diesem Kapitel
    .w-1-passage = 1 Textstelle
    .w-passages = { $count } Textstellen

d-shnvpmub = Fortschritt von
    .w-finished = Fertig
    .w-chapter-of = Kapitel { $clamp } von { $total }

d-oxaurbg7 = Autor von
    .w-unknown-author = Unbekannter Autor

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
    .w-added-last = Zuletzt hinzugefügt
    .w-by-author = Nach Autor
    .w-by-title = Nach Titel
    .w-hide-finished = Beendete ausblenden
    .w-sort = Sortieren

d-cmr5ibgv = Leseeinstellungen
    .w-reading-settings = Leseeinstellungen
    .w-text = Text
    .w-text-size = Textgröße
    .w-line-spacing = Zeilenabstand
    .w-justified-paragraphs = Absätze im Blocksatz
    .w-reading-faces = Schriftarten
    .w-set-at = { $family } · gesetzt auf { $size }×
    .w-hide = Ausblenden
    .w-show = Einblenden
    .w-how-large-reads = Wie groß { $font_title } liest
    .w-rename = Umbenennen
    .w-remove = Entfernen
    .w-the-name-it-shows-as = Der Name, unter dem sie erscheint
    .w-save-name = Namen speichern
    .w-cancel = Abbrechen
    .w-watched-folders = Überwachte Ordner
    .w-watch-a-folder = Ordner auswählen…
    .w-add-a-face = Schriftart hinzufügen
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
    .w-previous-passage = Vorherige Passage
    .w-next-passage = Nächste Passage
    .w-book-details = Buchdetails
    .w-face = Schriftart
    .w-title = Titel
    .w-author = Autor
    .w-subject = Thema
    .w-language-as-en-or-de = Sprache, als `en` oder `de`
    .w-choose-a-cover = Cover wählen
    .w-take-the-cover-off = Cover abnehmen
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
    .w-cancel = Abbrechen
    .w-copy = Kopieren
    .w-edit = Bearbeiten
    .w-edit-the-note = Die Notiz bearbeiten
    .w-finished = Beendet
    .w-save = Speichern

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

d-m5vjvjxn = Beendet
    .doc = Ob die Person das Buch beendet hat: was das Regal auslässt, wenn es beendete Bücher verbirgt.

d-rmk5y2dl = Sortieren nach
    .doc = Wie das Regal seine Bücher ordnet: `title`, `author` oder `added`, zuletzt hinzugefügte zuerst.

d-iw7jv3y3 = Beendete ausblenden
    .doc = Ob das Regal die Bücher auslässt, die eine Person beendet hat.

d-jya2xpym = Markieren
    .doc = Eine Markierungsfarbe, nach dem Namen, mit dem sie gewählt wird, und der Farbe, die sie malt.
    .noun = { $count ->
        [one] Markierung
       *[other] Markierungen
    }

d-2mud3bws = Name

d-ztqdbzwg = Wert

d-qazz5dbn = Regalreihenfolge setzen
    .doc = Ordnet das Regal nach Titel, nach Autor oder nach den zuletzt hinzugefügten Büchern.
    .did = sortierte das Regal
    .w-a-shelf-is-sorted-by-title = Ein Regal wird nach Titel, Autor oder Hinzufügen sortiert

d-qazz5dbn-order = Reihenfolge

d-7c3mgmnh = Beendete ausblenden setzen
    .doc = Lässt die Bücher, die eine Person beendet hat, aus dem Regal, oder zeigt sie wieder.
    .did = änderte, ob beendete Bücher erscheinen

d-7c3mgmnh-hidden = Verborgen

d-4wmsw3oj = Beendet setzen
    .doc = Markiert `b` als beendet für die lesende Person oder beginnt es neu.
    .did = änderte, ob ein Buch beendet ist

d-4wmsw3oj-who = Wer

d-4wmsw3oj-finished = Beendet

d-rhst3sds = Eine Notiz bearbeiten
    .doc = Schreibt neu, was eine Notiz oder ein Kommentar sagt.
    .w-only-its-author-can-edit-a = Nur die verfassende Person kann eine Notiz bearbeiten
    .w-write-a-note-first = Schreib zuerst eine Notiz

d-rhst3sds-m = M

d-rhst3sds-text = Text

d-t53zqwaz = Die Passage kopieren
    .doc = Kopiert eine Passage, sodass sie überall eingefügt werden kann: was das Kopieren einer Auswahl tut.

d-t53zqwaz-text = Text

d-om3e7iv4 = Regalreihenfolge setzen~present

d-7n672gfq = Notiztext bearbeiten~present

# Schriftarten und Buchdetails
d-6xfhtrzr = Schriftart
    .doc = Eine Schriftart, in der die Seite gesetzt werden kann: eine Familie, die dieses Gerät hat, der Name, den eine Person ihr gegeben hat, wie groß sie neben den anderen liest, und ob sie in der Auswahl des Lesers erscheint.
    .noun = { $count ->
        [one] Schriftart
       *[other] Schriftarten
    }

d-jlawoyck = Familie
    .doc = Die Familie, in der die Schriftart gesetzt ist: `serif`, `mono`, eine Familie, die das Toolkit mitbringt, oder eine, die das Gerät hat.

d-w2n5kzdl = Name
    .doc = Der Name, den eine Person ihr gegeben hat; leer, solange sie heißt wie ihre Familie.

d-6pky4ug3 = Größe
    .doc = Die Textgröße, in der diese Schriftart liest, als Vielfaches der Seite, zwischen 0,7 und 1,4.

d-qxdff5v3 = Sichtbar
    .doc = Ob sie in der Schriftartenauswahl des Lesers erscheint.

d-7bsrqnib = Mitgeliefert
    .doc = Eine Schriftart, die der Leser mitbringt: die Einstellungen behalten sie und bieten keinen Weg, sie zu entfernen.

d-obt3nk5o = Schriftarten
    .doc = Die Schriftarten, in denen die Seite gesetzt werden kann: die, die der Leser mitbringt, und die Familien, die eine Person von diesem Gerät hinzugefügt hat.

d-23vpvbzy = Titel setzen
    .doc = Gibt `b` den Titel, unter dem eine Person es liest.
    .did = benannte ein Buch um
    .w-a-book-has-a-title = Ein Buch hat einen Titel

d-23vpvbzy-title = Titel

d-3cfv74w3 = Autor setzen
    .doc = Gibt `b` seinen Autor, oder lässt es ohne einen.
    .did = änderte den Autor eines Buchs

d-3cfv74w3-author = Autor

d-kg76tk4c = Thema setzen
    .doc = Gibt `b` die Themenzeile, die seine Karte und seine Regalfilter sagen.
    .did = änderte das Thema eines Buchs

d-kg76tk4c-subject = Thema

d-syhb3w3r = Sprache setzen
    .doc = Setzt die Sprache, in der `b` gesetzt ist, für seine Silbentrennung.
    .did = änderte die Sprache eines Buchs
    .w-a-book-is-set-in-a = Ein Buch ist in einer Sprache gesetzt

d-syhb3w3r-language = Sprache

d-np45fqzx = Cover setzen
    .doc = Legt `picture` als Cover auf `b`.
    .did = änderte das Cover eines Buchs

d-np45fqzx-picture = Bild

d-6lnp7mta = Cover entfernen
    .doc = Nimmt ein Cover von `b`, damit seine Karte die eigene Kunst des Buchs zeichnet.
    .did = nahm das Cover eines Buchs ab

d-v5w4tpa7 = Schriftart hinzufügen
    .doc = Fügt die Familie `family`, die dieses Gerät hat, den Schriftarten des Lesers hinzu.
    .did = fügte eine Schriftart hinzu
    .w-a-face-has-a-family = Eine Schriftart hat eine Familie

d-v5w4tpa7-family = Familie

d-xkw5f5wc = Schriftart umbenennen
    .doc = Gibt der Schriftart `f` den Namen, unter dem eine Person sie liest; leer heißt sie wie ihre Familie.
    .did = benannte eine Schriftart um

d-xkw5f5wc-f = Schriftart
d-xkw5f5wc-name = Name

d-6l7hpiw4 = Schriftgröße setzen
    .doc = Setzt, wie groß die Schriftart `f` neben den anderen liest.
    .did = änderte die Größe einer Schriftart

d-6l7hpiw4-f = Schriftart
d-6l7hpiw4-size = Größe

d-5e53kwxp = Sichtbarkeit einer Schriftart setzen
    .doc = Sagt, ob die Schriftart `f` in der Auswahl des Lesers erscheint.
    .did = änderte, ob eine Schriftart erscheint

d-5e53kwxp-f = Schriftart
d-5e53kwxp-shown = Sichtbar

d-rvd4667o = Schriftart entfernen
    .doc = Nimmt eine eigene Schriftart der Person aus der Auswahl des Lesers.
    .did = entfernte eine Schriftart
    .w-the-reader-s-own-faces-stay = Die eigenen Schriftarten des Lesers bleiben

d-rvd4667o-f = Schriftart

d-vyxg67tn = Titel setzen
d-64m3byry = Sprache setzen
d-ujf5rwrf = Schriftart hinzufügen
d-o5nk6k7i = Schriftart entfernen
