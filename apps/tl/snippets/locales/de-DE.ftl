# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-snippets = Textbausteine

d-so-tilo-snippets-keywords = textbausteine, textbaustein, schnipsel, vorlagen, zwischenablage, code, snippets, templates

d-jzur3ob5 = Textbaustein
    .doc = Ein wiederverwendbarer Text oder Code.
    .noun = { $count ->
        [one] Textbaustein
       *[other] snippets
    }
    .untitled = Unbenannter Textbaustein

d-f7zljrnp = Titel

d-5k3zya5m = Inhalt
    .doc = Was kopiert wird.

d-wziqkkiz = Beschreibung
    .doc = Wofür der Textbaustein gedacht ist, über seinem Inhalt gezeigt.

d-rorcz6y2 = Favorit
    .doc = Markiert: markierte Textbausteine stehen in jeder Liste ganz oben.

d-b5ooyg4x = Ordner
    .doc = Der Ordner, in den er abgelegt ist; leer für keinen, den jede Liste außer seiner eigenen zeigt.

d-26cc5hue = Sprache

d-te2jwa3o = Schlagwörter
    .doc = Worum es geht, in Kleinbuchstaben und ohne `#`.

d-kkfkhxfb = Nutzungen
    .doc = Wie oft er kopiert wurde: was „Meistgenutzt“ liest.

d-jzur3ob5-filter-favorites = Favoriten

d-jzur3ob5-filter-tags = Schlagwörter

d-jzur3ob5-filter-language = Sprache

d-4eiv5c3p = Füllwert
    .doc = Eine Variable, die eine Person ausfüllt, und was sie dafür getippt hat.
    .noun = { $count ->
        [one] Füllwert
       *[other] fill values
    }

d-u5ym5ttn = Name

d-h5vss6x5 = Wert

d-fylf2syc = Füllung
    .doc = Das Ausfüllen der Variablen eines Textbausteins durch eine Person.
    .noun = { $count ->
        [one] Füllung
       *[other] fills
    }

d-qxvejddr = Textbaustein
    .doc = Der Textbaustein, der ausgefüllt wird; keiner, solange nichts ausgefüllt wird.

d-e6uc6jtt = Werte

d-fylf2syc-filter-snippet = Textbaustein

d-3x4ryb7i = Bibliothek
    .doc = Die Bibliothek des Arbeitsbereichs: jeder Textbaustein, den sie hält.
    .noun = { $count ->
        [one] Bibliothek
       *[other] libraries
    }

d-g5k73yqh = Textbausteine

d-avqtjv3w = Füllungen
    .doc = Die Füllung jeder Person hier.

d-sxr42pgj = Sprache
    .doc = Die Art von Text, in der ein Textbaustein geschrieben ist: sein Zeichen in der Liste und die Farben seines Inhalts.

d-clowkhig = Etwas, das nur Worte ist
    .doc = Etwas, das nur Worte ist.

d-4tycb6j2 = Shell

d-w4pekszm = Rust

d-dpv33ulb = SQL

d-x6c6cuyb = Git

d-zmb7xzvo = Python

d-56xzlfdj = JavaScript

d-ch7z3egw = JSON

d-zfvuyfaq = YAML

d-aiadtekw = Markdown

d-a4yonxcw = HTML

d-xxhzlytk = CSS

d-pmoafk6y = TOML

d-sewa2633 = Dockerfile

d-7dpulqjh = Neuer Textbaustein
    .doc = Macht einen Textbaustein, abgelegt in `folder`, wenn einer genannt ist.
    .did = machte einen Textbaustein
    .w-a-snippet-needs-a-title = Ein Textbaustein braucht einen Titel

d-7dpulqjh-title = Titel

d-7dpulqjh-folder = Ordner

d-of3vdpk2 = Duplizieren
    .doc = Macht eine zweite Kopie eines Textbausteins, direkt danach; seine Nutzungszahl beginnt wieder bei null.
    .did = duplizierte einen Textbaustein

d-of3vdpk2-snippet = Textbaustein

d-of3vdpk2-title = Titel

d-wvqaaaem = Löschen
    .doc = Löscht einen Textbaustein aus der Bibliothek.
    .did = löschte einen Textbaustein

d-wvqaaaem-snippet = Textbaustein

d-jznobsdy = Markierung entfernen
    .doc = Markiert einen Textbaustein oder nimmt die Markierung zurück.
    .w-remove-star = Markierung entfernen
    .w-star-it = Markieren

d-jznobsdy-starred = Markiert

d-vjnt6qmu = Schlagwörter bearbeiten…
    .doc = Gibt einem Textbaustein die Schlagwörter, die eine Person getippt hat, durch Kommas getrennt: jedes getrimmt, klein, ohne `#`, höchstens 24.
    .did = versah einen Textbaustein mit Schlagwörtern

d-vjnt6qmu-tags = Schlagwörter

d-dzojuuh3 = Schlagwort entfernen
    .doc = Nimmt einem Textbaustein ein Schlagwort ab.

d-dzojuuh3-tag = Schlagwort

d-gc4q7njn = Ordner umbenennen
    .doc = Benennt einen Ordner überall um; ein leerer neuer Name legt seine Textbausteine unter nichts ab.
    .did = benannte einen Ordner um
    .w-there-is-no-such-folder = Diesen Ordner gibt es nicht

d-gc4q7njn-folder = Ordner

d-gc4q7njn-to = Zu

d-627pjftv = Schlagwort umbenennen
    .doc = Benennt ein Schlagwort überall um.
    .did = benannte ein Schlagwort um
    .w-there-is-no-such-tag = Dieses Schlagwort gibt es nicht
    .w-a-tag-needs-a-name = Ein Schlagwort braucht einen Namen

d-627pjftv-tag = Schlagwort

d-627pjftv-to = Zu

d-rirlohhq = Schlagwort entfernen
    .doc = Nimmt jedem Textbaustein ein Schlagwort ab.
    .did = entfernte ein Schlagwort
    .w-there-is-no-such-tag = Dieses Schlagwort gibt es nicht

d-rirlohhq-tag = Schlagwort

d-2fawdcmk = Kopieren
    .doc = Kopiert den Inhalt eines Textbausteins in die Zwischenablage und zählt die Nutzung.
    .did = kopierte einen Textbaustein
    .w-copied = Kopiert
    .w-copied-it-leaves-the-clipboard-in = Kopiert; nach einer Minute ist die Zwischenablage wieder leer
    .w-could-not-copy-to-the-clipboard = Konnte nicht in die Zwischenablage kopieren

d-niz7er5a = Ausfüllen und kopieren
    .doc = Beginnt das Ausfüllen der Variablen eines Textbausteins: ein Feld für jede Variable seines Inhalts, geleert.

d-niz7er5a-snippet = Textbaustein

d-iaud5pvs = Kopieren
    .doc = Kopiert den ausgefüllten Inhalt, zählt die Nutzung und legt die Füllung weg.
    .did = kopierte einen Textbaustein
    .w-copied = Kopiert
    .w-copied-it-leaves-the-clipboard-in = Kopiert; nach einer Minute ist die Zwischenablage wieder leer
    .w-could-not-copy-to-the-clipboard = Konnte nicht in die Zwischenablage kopieren

d-iaud5pvs-fill = Füllung

d-5w7vbjxg = Abbrechen
    .doc = Legt die Füllung ohne Kopieren weg.

d-hnseelrx = Füllung leeren
    .doc = Leert eine Füllung, sodass das Blatt schließt.

d-uek4qev2 = Neuer Textbaustein

d-6bdzaoah = Markieren

d-m6awo5yk = Ordner umbenennen

d-ziy3kioi = Schlagwort umbenennen

d-fdsmfdne = Schlagwort entfernen

d-2jh6hotv = Textbausteinzeile

d-xqxtrmmb = Textbausteinseite
    .w-remove-star = Markierung entfernen
    .w-star-it = Markieren
    .w-duplicate = Duplizieren
    .w-delete = Löschen
    .w-untitled-snippet = Unbenannter Textbaustein
    .w-language = Sprache
    .w-no-folder = Kein Ordner
    .w-edit-tags = Schlagwörter bearbeiten…
    .w-what-it-is-for = Wofür er gedacht ist
    .w-fill-in-and-copy = Ausfüllen und kopieren
    .w-copy = Kopieren
    .w-it-looks-like-a-credential-so = Es sieht wie ein Zugangsdatum aus, daher verlässt die Kopie die Zwischenablage in unter einer Minute

d-lqt75wck = Füllblatt
    .w-fill-in-and-copy = Ausfüllen und kopieren
    .w-what-each-variable-stands-for = Wofür jede Variable steht
    .w-copy = Kopieren
    .w-cancel = Abbrechen

d-aaruymww = Textbausteinfenster
    .w-snippets = Textbausteine
    .w-all-snippets = Alle Textbausteine
    .w-favorites = Favoriten
    .w-most-used = Meistgenutzt
    .w-folders = Ordner
    .w-tags = Schlagwörter
    .w-keep-the-text-and-code-you = Behalte den Text und Code, den du oft einfügst, mit eigenen Ordnern und Schlagwörtern.
    .w-nothing-here-yet = Hier ist noch nichts.
    .w-new-snippet = Neuer Textbaustein
    .w-no-snippets-yet = Noch keine Textbausteine
    .w-choose-a-snippet = Wähle einen Textbaustein

d-3afzc6hv = Text kopieren
    .doc = Legt `text` in die Zwischenablage: als Geheimnis, das keine Verläufe behalten und das die Zwischenablage nach einer Weile verlässt, wenn es wie ein Zugangsdatum aussieht, damit ein API-Schlüssel nicht für das nächste Einfügen dort bleibt.

d-3afzc6hv-text = Text

d-73sjk77z = Eine Nutzung erfassen
    .doc = Zählt eine Nutzung eines Schnipsels, ohne ihn zu kopieren: was ein Skript oder ein Agent erfasst, der den Inhalt auf seinem eigenen Weg genommen hat, damit "Am häufigsten" dieselbe Zahl zeigt, wie der Schnipsel auch genutzt wurde.
