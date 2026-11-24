# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-hex-editor = Hex-Editor

d-so-tilo-hex-editor-keywords = hex-editor, hex, bytes, binär, binär-editor, editor, datenträger, byte, hexadezimal, binär

d-cjs7wfwl = Änderung
    .doc = Eine Änderung der Bytes: bei `at` stehen `remove` Bytes als die hexadezimalen von `insert`.
    .noun = { $count ->
        [one] Änderung
       *[other] Änderungen
    }

d-b4ynsjfc = Bei

d-lkghkw33 = Entfernen

d-n55erru5 = Einfügen

d-cjs7wfwl-filter-at = Bei

d-cjs7wfwl-filter-remove = Entfernen

d-z7zzcrb6 = Zeile
    .doc = Eine Zeile des Rasters, wie das Gerät sie gelesen hat: ihre Bytes als Hexpaare und für jedes eine `0` oder `1`, ob es sich von dem unterscheidet, was die Datei enthält.
    .noun = { $count ->
        [one] Zeile
       *[other] Zeilen
    }

d-rqqjo5b2 = Hex

d-i23efoqg = Geändert

d-gd5t2ug6 = Deutung
    .doc = Eine Art, die Bytes am Cursor zu lesen: `u16` ist dort 4660.
    .noun = { $count ->
        [one] Deutung
       *[other] Deutungen
    }

d-j3rx5nr6 = Bezeichnung

d-bqtm2sxd = Wert

d-whqswnrc = Breite

d-gd5t2ug6-filter-width = Breite

d-fsjz7klv = Bereich
    .doc = Ein Bereich von Bytes, halboffen.
    .noun = { $count ->
        [one] Bereich
       *[other] Bereiche
    }

d-vjv7m35i = Anfang

d-6nu52vd6 = Ende

d-fsjz7klv-filter-start = Anfang

d-fsjz7klv-filter-end = Ende

d-s5feqeox = Platz
    .doc = Der Platz einer Person in einem Dokument: ihr Cursor, wie sie die Bytes liest und was das Gerät zuletzt für ihr Fenster gelesen hat.
    .noun = { $count ->
        [one] Platz
       *[other] Plätze
    }

d-4pp4s3sf = Cursor

d-ta3nqlbq = Anker

d-d6nzpsjf = Spalte
    .doc = `hex` oder `ascii`: in welche Spalte die Tasten tippen.

d-mum5d7nq = Modus
    .doc = `overwrite` ersetzt das Byte unter dem Cursor; `insert` fügt ein neues ein.

d-xmgji4li = Oberes Nibble
    .doc = Das obere Nibble, das im Überschreibmodus getippt wurde und das die nächste Ziffer vollendet; keines zwischen Bytes.

d-b23s2qvq = Bytereihenfolge
    .doc = `little` oder `big`: in welcher Reihenfolge der Inspektor Zahlen liest.

d-ntd56cpd = Anfang
    .doc = Die erste Zeile, die das Fenster zeigt.

d-d4rmfnak = Spalten
    .doc = Die Bytes, die eine Zeile hält.

d-u54h5izd = Zeilen
    .doc = Die Zeilen, die das Fenster zeigt, wie das Gerät sie gelesen hat: jede ihre Bytes als Hexpaare.

d-iyf5x4du = Geändert
    .doc = Für jede gezeigte Zeile eine `0` oder `1` für jedes ihrer Bytes: ob es sich von dem unterscheidet, was die Datei enthält.

d-nczhspum = Deutungen
    .doc = Was die Bytes am Cursor sind: eine Bezeichnung und ein Wert, ein Tabulator dazwischen.

d-uswoxemn = Treffer
    .doc = Was die Suche des Fensters fand, jedes als `start:end`, und auf welchem der Cursor steht.

d-nv3vcmev = Beim Treffer

d-s777zrl4 = Gekürzt

d-gy4cijiy = Suchbegriff
    .doc = Der zuletzt gesuchte Begriff und wie er gelesen wurde.

d-hfchq6ug = Art

d-pzwozqbg = Groß- und Kleinschreibung beachten

d-olbisfbn = Ganzes Wort

d-jko5an6z = Leiste
    .doc = Die offene Leiste unter der Kopfzeile: `search`, `replace`, `goto` oder `inspector`; leer für keine.

d-n7ibv25j = Hinweis
    .doc = Was das Fenster zuletzt sagte: ein Ergebnis oder was schiefging.

d-jslwh26v = Art des Hinweises

d-s5feqeox-filter-cursor = Cursor

d-s5feqeox-filter-anchor = Anker

d-s5feqeox-filter-high = Oberes Nibble

d-s5feqeox-filter-first = Anfang

d-s5feqeox-filter-columns = Spalten

d-s5feqeox-filter-at_match = Beim Treffer

d-s5feqeox-filter-truncated = Gekürzt

d-s5feqeox-filter-match_case = Groß- und Kleinschreibung beachten

d-s5feqeox-filter-whole_word = Ganzes Wort

d-jck5gzaa = Dokument
    .doc = Die Bytes einer Datei: wo sie im Inhaltsspeicher des Arbeitsbereichs stehen und welche Änderungen seit dem Lesen gemacht wurden.
    .noun = { $count ->
        [one] Dokument
       *[other] Dokumente
    }
    .untitled = Unbenannt.bin

d-dbkdqzos = Name

d-teahiasd = Geöffnet
    .doc = Wie oft die Bytes neu gelesen wurden.

d-blhbkxjc = Grundlage
    .doc = Die Adresse der Bytes, wie sie gelesen oder zuletzt gespeichert wurden; leer für einen leeren Puffer.

d-wts2l2hg = Pfad
    .doc = Wo die Datei auf diesem Gerät steht; leer für eine übergebene oder nirgends gespeicherte.

d-nqk5afwc = Länge
    .doc = Die Länge der Bytes mit den angewandten Änderungen.

d-7u75gmtu = Bearbeitet
    .doc = Wie viele Bytes sich von dem unterscheiden, was die Datei enthält.

d-3tqozdp3 = Änderungen
    .doc = Die Änderungen seit dem Lesen der Grundlage, in Reihenfolge: `at:remove:insert`, die einzufügenden Bytes als Hexpaare.

d-2fs4ukcj = Plätze
    .doc = Der eigene Platz jeder Person im Dokument.

d-jck5gzaa-filter-opened = Geöffnet

d-jck5gzaa-filter-length = Länge

d-jck5gzaa-filter-edited = Bearbeitet

d-v5bn27jt = Bibliothek
    .doc = Der Hex-Editor des Arbeitsbereichs: die Puffer, die er offen hat.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-fu57pivb = Dokumente

d-tehzgnkf = Frage nach einer Datei
    .noun = { $count ->
        [one] Open ask
       *[other] Open ask
    }

d-rruucmz4 = Pfad

d-ycca2usf = Antwort auf ein Öffnen
    .noun = { $count ->
        [one] Opened answer
       *[other] Opened answer
    }

d-cevle23p = Name

d-idpg6pxk = Adresse

d-cr3tncji = Länge

d-ycca2usf-filter-length = Länge

d-uu2y2bvc = Frage nach Zeilen
    .noun = { $count ->
        [one] Rows ask
       *[other] Rows ask
    }

d-ldq6mtad = Adresse

d-2t4i36rg = Änderungen

d-52p24f3b = Anfang

d-j3ztka6c = Anzahl

d-jmwhucjc = Spalten

d-tjv2yhp3 = Bytereihenfolge

d-oowz47v3 = Cursor

d-uu2y2bvc-filter-first = Anfang

d-uu2y2bvc-filter-count = Anzahl

d-uu2y2bvc-filter-columns = Spalten

d-uu2y2bvc-filter-cursor = Cursor

d-rvfaddkj = Antwort mit Zeilen
    .noun = { $count ->
        [one] Rows answer
       *[other] Rows answer
    }

d-yjjtzhyt = Länge

d-ao74hr7t = Bearbeitet

d-lgghdfh2 = Lesen

d-df32ywi4 = Fehler

d-jhbkwvr2 = Zeilen

d-3ugkwm42 = Geändert

d-xhxdxsgb = Deutungen

d-rvfaddkj-filter-length = Länge

d-rvfaddkj-filter-edited = Bearbeitet

d-rvfaddkj-filter-read = Lesen

d-jfroysuv = Frage nach einer Suche
    .noun = { $count ->
        [one] Find ask
       *[other] Find ask
    }

d-6grc7o75 = Adresse

d-5yumynmf = Änderungen

d-ljornfmm = Suchbegriff

d-wyvxlbd4 = Art

d-b4bt6mtj = Groß- und Kleinschreibung

d-bxix7im4 = Wort

d-qyar3ymw = Grenze

d-jfroysuv-filter-case = Groß- und Kleinschreibung

d-jfroysuv-filter-word = Wort

d-jfroysuv-filter-limit = Grenze

d-26cey6xc = Antwort auf eine Suche
    .noun = { $count ->
        [one] Found answer
       *[other] Found answer
    }

d-t6d5sfoz = Treffer

d-4fdr6kla = Gekürzt

d-26cey6xc-filter-truncated = Gekürzt

d-qpiqvvbd = Frage nach einer Änderung
    .noun = { $count ->
        [one] Apply ask
       *[other] Apply ask
    }

d-ba424tqt = Adresse

d-qtf35usu = Änderungen

d-r5xf3t6m = Vorgang

d-z2ahkv3u = Bei

d-3kwri4q5 = Länge

d-pk7jqula = Bytes

d-qpiqvvbd-filter-at = Bei

d-qpiqvvbd-filter-len = Länge

d-l4ctb355 = Antwort auf eine Änderung
    .noun = { $count ->
        [one] Applied answer
       *[other] Applied answer
    }

d-2w7szbth = Änderung

d-ll4ibsb5 = Länge

d-l4ctb355-filter-length = Länge

d-wdtj6n4g = Frage nach einem Ersetzen
    .noun = { $count ->
        [one] Replace ask
       *[other] Replace ask
    }

d-3riih5ln = Adresse

d-o4kbz7cm = Änderungen

d-ptnk64f6 = Bereiche

d-us73s4xt = Bytes

d-mj26u2qk = Modus

d-452gevoq = Antwort auf ein Ersetzen
    .noun = { $count ->
        [one] Replaced answer
       *[other] Replaced answer
    }

d-j3lt4zli = Änderungen

d-3kaouk56 = Anzahl

d-4fce7y46 = Länge

d-452gevoq-filter-count = Anzahl

d-452gevoq-filter-length = Länge

d-uodblmtb = Frage nach einem Bereich
    .noun = { $count ->
        [one] Range ask
       *[other] Range ask
    }

d-3surtarm = Adresse

d-l7dzgaqe = Änderungen

d-3v66hb5v = Anfang

d-fsjcytnx = Ende

d-fyp52l2r = Art

d-uodblmtb-filter-start = Anfang

d-uodblmtb-filter-end = Ende

d-xwmajagh = Antwort mit einem Bereich
    .noun = { $count ->
        [one] Range answer
       *[other] Range answer
    }

d-d7nxzpqz = Text

d-jzqk7v3k = Frage nach Bytes
    .noun = { $count ->
        [one] Bytes ask
       *[other] Bytes ask
    }

d-66cpqgom = Adresse

d-4tyyvxqh = Änderungen

d-c4k3zrrj = Antwort mit Bytes
    .noun = { $count ->
        [one] Bytes answer
       *[other] Bytes answer
    }

d-smxqtshm = Bytes

d-yvz46vke = Frage nach einem Speichern
    .noun = { $count ->
        [one] Save ask
       *[other] Save ask
    }

d-62acy6x2 = Adresse

d-2d44lw44 = Änderungen

d-sujs7lc2 = Pfad

d-evzo42ed = Antwort auf ein Speichern
    .noun = { $count ->
        [one] Saved answer
       *[other] Saved answer
    }

d-fnprm7qy = Adresse

d-bnwrfq64 = Länge

d-evzo42ed-filter-length = Länge

d-lku5ldrh = Ankommen
    .doc = Ein Dokument wird von dem geöffnet, der es öffnet: er bekommt seinen eigenen Platz darin, und das Fenster liest die Bytes, die es zeigt.

d-ib5i76y6 = Cursor bewegen
    .doc = Der Cursor zog zu `at`, innerhalb der Bytes gehalten; `extend` behält den Anfang der Auswahl, und die erste Zeile des Fensters folgt ihm, wenn er die gezeigten Zeilen verlässt.

d-ib5i76y6-desk = Platz

d-ib5i76y6-at = Bei

d-ib5i76y6-extend = Ausdehnen

d-ykxrxick = Sagen
    .doc = Sagt etwas im Fenster: ein Ergebnis oder was schiefging.

d-ykxrxick-words = Wörter

d-ykxrxick-kind = Art

d-ryi5rumk = Zeigen
    .doc = Liest die Zeilen des Fensters und die Bytes an seinem Cursor erneut vom Gerät.

d-vojndsoo = Bearbeiten
    .doc = Wendet eine Änderung der Bytes an: das Gerät prüft sie und antwortet mit der Änderung, die bleibt.

d-vojndsoo-op = Vorgang

d-vojndsoo-at = Bei

d-vojndsoo-len = Länge

d-vojndsoo-bytes = Bytes

d-topfzl2k = Pfad öffnen
    .doc = Öffnet die Datei bei `path` auf diesem Gerät als eigenen Puffer.

d-topfzl2k-path = Pfad

d-lsmhjlew = Abgelehnter Puffer

d-lsmhjlew-path = Pfad

d-lsmhjlew-why = Grund

d-ki2kthjn = Datei öffnen
    .doc = Öffnet eine Datei, die eine Person gewählt hat oder die eine andere App übergab, als eigenen Puffer.
    .did = öffnete eine Datei

d-ki2kthjn-file = Datei

d-gdczru47 = Geöffneter Puffer
    .doc = Der Puffer der Bytes bei `address`: der bereits darauf geöffnete, neu gelesen, oder ein neuer.

d-gdczru47-name = Name

d-gdczru47-address = Adresse

d-gdczru47-path = Pfad

d-gdczru47-length = Länge

d-wydoopxa = Neue leere Datei
    .doc = Ein leerer Puffer, in den Bytes getippt werden, gespeichert, wo die Person es sagt.
    .did = startete einen leeren Puffer

d-eziyendc = Leerer Puffer

d-eziyendc-name = Name

d-5ptrab3k = Datei öffnen
    .doc = Die Aktion, die die Dateiauswahl der Person füllt.

d-5ptrab3k-file = Datei

d-wucjou7l = Speichern
    .doc = Speichert den Puffer, woher er kam, oder fragt wo, wenn er noch nirgends steht.
    .did = speicherte die Datei
    .w-this-file-stands-nowhere-on-this = Diese Datei steht noch nirgends auf diesem Gerät: nutze Speichern unter

d-o7q63qsf = Bytes speichern
    .doc = Die Bytes als Datei, die die Person mit der Speicherauswahl des Systems ablegt, die das Fenster zeigt.

d-ounak5zh = Neu gegründet
    .doc = Was gespeichert wurde, ist die Grundlage: keine Änderung steht mehr dagegen.

d-ounak5zh-address = Adresse

d-ounak5zh-length = Länge

d-e3uvry6i = Datei schließen
    .doc = Vergisst einen Puffer, seine Änderungen mit ihm.

d-e3uvry6i-d = D

d-sjgncfl3 = Taste drücken
    .doc = Die auf dem Raster gedrückten Tasten, wie die Tastatur der Person sie nennt.

d-sjgncfl3-key = Taste

d-tky2mwod = Tastenkürzel
    .doc = Die Strg-Kürzel, die getippt werden, während die Bytes die Tastatur haben.

d-tky2mwod-desk = Platz

d-tky2mwod-key = Taste

d-53hjuxzd = Bewegt
    .doc = Der Cursor zog zu `at`: die Zeilen und der Inspektor lesen dort.

d-53hjuxzd-desk = Platz

d-53hjuxzd-at = Bei

d-53hjuxzd-extend = Ausdehnen

d-oxhf3mvc = Taste tippen

d-oxhf3mvc-desk = Platz

d-oxhf3mvc-one = Eins

d-cddfr4of = Hexziffer tippen
    .doc = Tippt eine Hexziffer: im Überschreibmodus setzt die erste das obere Nibble und die zweite vollendet das Byte; im Einfügemodus wird jedes vollendete Byte eingefügt.

d-cddfr4of-desk = Platz

d-cddfr4of-value = Wert

d-i4voe4c3 = Text tippen
    .doc = Tippt ein Zeichen in die Textspalte und rückt dann vor.

d-i4voe4c3-desk = Platz

d-i4voe4c3-one = Eins

d-d4uh3stf = Rücktaste
    .doc = Im Einfügemodus geht das Byte vor dem Cursor; im Überschreibmodus tritt der Cursor nur zurück.

d-d4uh3stf-desk = Platz

d-5jv54u5l = Am Cursor löschen
    .doc = Die Auswahl geht, sonst das Byte unter dem Cursor.

d-5jv54u5l-desk = Platz

d-wenh6lnl = Alles auswählen

d-wenh6lnl-desk = Platz

d-mr6fuewa = Kopieren
    .doc = Kopiert die ausgewählten Bytes in der Form der Spalte: `50 4e 47 44` oder `PNGD`.

d-y5wcfls4 = Ausschneiden
    .doc = Kopiert die Auswahl und löscht sie.

d-t63az7ij = Einfügen
    .doc = Fügt die Zwischenablage bei der Auswahl oder beim Cursor ein, im Fokusmodus.

d-bvziotqd = Ersetzt
    .doc = Ersetzt jeden Bereich durch dieselben Bytes, in einem Schritt.

d-bvziotqd-desk = Platz

d-bvziotqd-spans = Bereiche

d-bvziotqd-bytes = Bytes

d-lcexraol = Bytes suchen
    .doc = Öffnet die Suchleiste.

d-tcw7lqtk = Suchen und ersetzen
    .doc = Öffnet die Ersetzen-Leiste.

d-s5cyut7x = Zu Offset springen
    .doc = Öffnet die Leiste zum Springen zu einem Offset.

d-7kxwiqbq = Inspektor zeigen oder verbergen
    .doc = Zeigt oder verbirgt den Inspektor.

d-fsxslum7 = Leiste schließen
    .doc = Schließt die Leiste unter der Kopfzeile.

d-qr5huw5z = Die Bytes durchsuchen
    .doc = Findet `query` in den Bytes: ein Hexmuster oder Text nach seinen Regeln.

d-qr5huw5z-query = Suchbegriff

d-an5w227j = Zeigen
    .doc = Setzt den Cursor auf den Treffer bei `index`.

d-an5w227j-index = Index

d-elqaxiiv = Alle ersetzen
    .doc = Ersetzt jeden Treffer der Suche durch `with`.

d-elqaxiiv-with = Damit

d-pq6chsex = Bereiche ersetzen

d-pq6chsex-desk = Platz

d-pq6chsex-spans = Bereiche

d-pq6chsex-with = Damit

d-5ri4qufq = Art der Suche
    .doc = Liest die Wahl der Suchleiste: Text oder Hexadezimal, Groß- und Kleinschreibung und ganze Wörter.

d-5ri4qufq-i = I

d-uzubdaxi = Groß- und Kleinschreibung der Suche

d-uzubdaxi-on = An

d-qea6cfig = Ganzes Wort der Suche

d-qea6cfig-on = An

d-fvwif6qs = Springen zu
    .doc = Setzt den Cursor auf den Offset, den `query` nennt.

d-fvwif6qs-query = Suchbegriff

d-2cks257j = Bytereihenfolge setzen

d-2cks257j-i = I

d-hffonsed = Modus setzen

d-hffonsed-i = I

d-fgy2d4ey = Spalte setzen

d-fgy2d4ey-i = I

d-wilggcvm = Verwerfen

d-yyd33pnt = Gedrückt
    .doc = Ein Druck auf das Raster: das Byte bei `at` bekommt den Cursor, die gedrückte Spalte den Fokus, und `"shift"` behält den Anfang der Auswahl.

d-yyd33pnt-at = Bei

d-z3a3o4ap = Gerollt
    .doc = Das Rad oder die Leiste bewegte das Fenster um `rows`.

d-z3a3o4ap-rows = Zeilen

d-ttzgh32m = Bytes einfügen
    .doc = Fügt `bytes` (hexadezimal, etwa `de ad be ef`) bei `offset` ein.

d-ttzgh32m-offset = Offset

d-ttzgh32m-bytes = Bytes

d-4idnkxx2 = Bytes ersetzen
    .doc = Ersetzt die Bytes bei `offset` durch `bytes` (hexadezimal) und lässt die Datei nie wachsen.

d-4idnkxx2-offset = Offset

d-4idnkxx2-bytes = Bytes

d-cgsqzkz4 = Bytes löschen
    .doc = Entfernt `len` Bytes bei `offset`.

d-cgsqzkz4-offset = Offset

d-cgsqzkz4-len = Länge

d-umksjjan = Speichern

d-it2igfh4 = Bibliothek
    .w-hex-editor = Hex-Editor
    .w-open-file = Datei öffnen
    .w-new-empty-file = Neue leere Datei
    .w-no-file-open = Keine Datei geöffnet
    .w-open-a-file-to-read-its = Öffne eine Datei, um ihre Bytes zu lesen, oder starte eine leere und tippe Bytes ein.
    .w-open-files = Geöffnete Dateien
    .w-close = Schließen
    .w-changed = { $size_text } · { $edited } geändert
    .w-not-on-disk = Nicht auf der Platte
    .w-saved = Auf der Platte

d-hecirnqi = Dokument
    .w-dismiss = Verwerfen
    .w-de-ad-be-ef = de ad be ef
    .w-words-in-the-bytes = Wörter in den Bytes
    .w-read-as = Lesen als
    .w-hex = Hex
    .w-text = Text
    .w-match-case = Groß-/Kleinschreibung beachten
    .w-whole-word = Ganzes Wort
    .w-close = Schließen
    .w-what-to-put-there = Was dort stehen soll
    .w-replace-all = Alle ersetzen
    .w-first-matches = Erste { $len } Treffer
    .w-matches = { $len } Treffer
    .w-0x1f-or-31 = 0x1F oder 31
    .w-reading-the-file = Datei wird gelesen…

d-c3gyftot = Werkzeugleiste
    .w-save = Speichern
    .w-save-as = Speichern unter
    .w-edit-mode = Bearbeitungsmodus
    .w-overwrite = Überschreiben
    .w-insert = Einfügemodus
    .w-keys-type = Tasten schreiben
    .w-hex = Hex
    .w-text = Text
    .w-edit = Bearbeiten
    .w-find-bytes = Bytes suchen
    .w-find-and-replace = Suchen und ersetzen
    .w-go-to-offset = Zum Offset springen
    .w-copy = Kopieren
    .w-cut = Ausschneiden
    .w-paste = Einfügen
    .w-select-all = Alles auswählen
    .w-inspector = Inspektor

d-em4nrjtr = Inspektor
    .w-inspector = Inspektor
    .w-byte-order = Byte-Reihenfolge
    .w-little = Little
    .w-big = Big

d-jdsqkjn7 = Status
    .w-offset-0x = Offset 0x{ $cursor_hex }
    .w-selected = { $byte_count } ausgewählt
    .w-edited = { $byte_count } geändert
