# Notes in German. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-notes = Notizen

d-so-tilo-notes-keywords = note, memo, memos, notebook, journal, markdown, notepad, textedit, apple notes, google keep, onenote, obsidian, notiz, notizbuch, aufzeichnungen, zettel, merkzettel, tagebuch, notizen machen

d-mavvhka7 = Notiz
    .doc = Eine Notiz: ein Titel, ein Text, die Notiz, unter der sie steht, und ihre Schlagwörter.
    .noun = { $count ->
        [one] Notiz
       *[other] Notizen
    }
    .untitled = Ohne Titel

d-dpj7scfe = Titel

d-olsyp3aw = Text

d-pmjerv63 = Bilder
    .doc = Die Bilder, die die Notiz zeigt, nach der Adresse ihres Inhalts: der Name, unter dem jedes aufgenommen wurde.

d-ssu2tbnf = Übergeordnet

d-erijxfd3 = Quelle
    .doc = Die Datei, aus der die Notiz importiert wurde, nach der Adresse ihres Inhalts: dieselbe Datei noch einmal zu importieren öffnet diese Notiz, statt eine zweite anzulegen.

d-uquoltho = Herkunft
    .doc = Der Name, den diese Datei hatte, samt Pfad in ihrem Archiv: ein Import, der die Verschachtelung eines Pakets behält, findet darüber die Notiz, unter der eine Seite steht.

d-gfk6rqxp = Angeheftet

d-xcpasjg7 = Schlagwörter

d-ddxl7oum = Reaktionen
    .doc = Wer auf die Notiz reagiert hat, nach Emoji: die Person und der Agent, für die jeder Chip in ihrer Kopfzeile steht, und wessen eigene Reaktion ein Druck auf ihn zurücknimmt.

d-t6igvr56 = Reagiert
    .doc = Die Reaktionen, die sie in der nativen Notizen-App hatte, jedes Emoji mit der Zahl derer, die damit reagiert haben: wer dort reagiert hat, wird nicht übernommen, da diese App Personen nur als ihre eigenen Mitglieder nennt.

d-mavvhka7-filter-pinned = Angeheftet

d-mavvhka7-filter-tags = Schlagwörter

d-qymo5lg2 = Neue Notiz
    .doc = Beginnt eine Notiz oben im Baum, mit dem Titel `title` und dem Text `body` (Markdown), wenn der Aufrufer sie schon kennt.

d-37dxigeq = Untergeordnete Notiz
    .doc = Beginnt eine Notiz in der von `n`, nach den Notizen, die sie schon hält.

d-37dxigeq-words = Wörter

d-7jhaq3a5 = Verschieben nach
    .doc = Verschiebt eine Notiz an eine Stelle im Baum: unter das Ende von `parent` (oder ganz oben ohne), oder dorthin, wo `before` steht.
    .w-a-note-cannot-go-inside-itself = Eine Notiz kann nicht in sich selbst liegen

d-ai4sxgvt = Danach verschieben
    .doc = Verschiebt eine Notiz direkt hinter `n` und alle Notizen unter ihr: die Stelle, die ein Ablegen hinter einer Zeile verlangt, und die die Zeile nennt, hinter der sie landet, nicht die danach.
    .w-a-note-cannot-go-inside-itself = Eine Notiz kann nicht in sich selbst liegen

d-c34rzynx = Duplizieren
    .doc = Kopiert eine Notiz mit allen Notizen unter ihr, direkt nach der kopierten.

d-jo2tztsx = Nach Titel sortieren
    .doc = Bringt die Notizen einer Notiz einmal in die Reihenfolge ihrer Titel.
    .w-it-holds-no-notes = Sie enthält keine Notizen

d-qhmjwgei = Anheften
    .doc = Heftet eine Notiz an oder löst sie.
    .w-pin = Anheften
    .w-unpin = Lösen

d-nydyehwn = Löschen
    .doc = Löscht eine Notiz und alle Notizen unter ihr, als eine Änderung, die auf einmal zurückgenommen wird.

d-zvgl2tzt = Umbenennen
    .doc = Benennt eine Notiz um.

d-6fwsh5ui = Text hinzufügen
    .doc = Schreibt `text` (Markdown) ans Ende einer Notiz, unter das, was sie schon sagt: eine Zeile, eine Liste oder ein Absatz dazu.
    .did = hat einer Notiz etwas hinzugefügt

d-gc34cbhl = Notiz ablegen
    .doc = Legt in einem Schritt eine Notiz mit dem Titel `title` und dem Schlagwort `tag` an: was eine andere App von dem aufschreibt, was sie getan hat (der Morgen in Heute), ohne auf die gemachte Notiz zu warten, um sie zu benennen.

d-j33acdci = Neues Schlagwort
    .doc = Legt ein Schlagwort des Notizbuchs mit dem Namen `name` an oder gibt das schon so benannte: der Name ist das, woran Menschen, Agenten und Skripte ein Schlagwort erkennen.
    .w-a-tag-needs-a-name = Ein Schlagwort braucht einen Namen

d-7ow26bju = Verschlagworten
    .doc = Gibt einer Notiz ein Schlagwort.
    .w-a-tag-needs-a-name = Ein Schlagwort braucht einen Namen

d-3u3bevec = Schlagwort entfernen
    .doc = Nimmt ein Schlagwort von einer Notiz.

d-ja7xx6nf = Umbenennen
    .doc = Benennt ein Schlagwort des Notizbuchs um: jede Notiz, die den alten Namen trägt, trägt den neuen.
    .w-a-tag-needs-a-name = Ein Schlagwort braucht einen Namen
    .w-the-notebook-has-that-tag = Das Notizbuch hat dieses Schlagwort

d-6hqlshe7 = Löschen
    .doc = Löscht ein Schlagwort des Notizbuchs: keine Notiz trägt den Namen mehr, und das Notizbuch führt es nicht mehr auf.

d-eec4pye2 = Schlagwörter aus Notizen übernehmen
    .doc = Führt die Schlagwörter auf, die die Notizen des Notizbuchs tragen, jedes einmal: was ein Notizbuch braucht, das geschrieben wurde, bevor das Notizbuch seine Schlagwörter aufführte, und was ein Start hinterließ, der Schlagwörter direkt an Notizen schrieb.

d-3ngyieqa = In der Notiz suchen
    .doc = Findet die Wörter `text` in einer Notiz, unabhängig von der Groß- und Kleinschreibung, und antwortet mit jedem Absatz, der sie enthält, samt seiner Stelle in der Notiz und seinen Wörtern: was die Suchleiste des Fensters zeigt, erreichbar für einen Agenten oder die Palette, die die Notiz kennt, aber nicht, wo in ihr zu suchen ist.
    .w-type-something-to-find = Zum Suchen etwas eingeben

d-jkqp27g7 = Reagieren
    .doc = Fügt deine Reaktion mit einem Emoji zu einer Notiz hinzu oder nimmt sie zurück, wenn du damit reagiert hattest: ein Druck auf einen ihrer Chips oder die Wahl aus dem, den ihr Hinzufügen-Knopf geöffnet hat.

d-jkqp27g7-emoji = Emoji

d-4s5iqnfo = Bild einfügen
    .doc = Setzt ein Bild in eine Notiz, dort wo ihre Worte enden, beschrieben durch `alt` für einen Leser, der es nicht sehen kann: die Person wählt die Datei, und die Notiz behält die Adresse ihres Inhalts statt des Bildes, und den Namen, unter dem es aufgenommen wurde.
    .did = fügte ein Bild hinzu

d-xptsjaii = Markdown exportieren
    .doc = Eine Notiz als Markdown: ihr Titel, dann ihr Text, jedes Bild, das sie zeigt, daneben als `<note>/images/<name>`, ein Pfad, dem ein anderer Leser der Datei folgt und den diese App wieder als Adresse der Bilddaten liest.

d-2e5dloct = Notiz exportieren
    .doc = Eine Notiz als Datei: ihr Markdown, und die Bilder, die sie zeigt, daneben.

d-cdjz7d5y = Notizbuch exportieren
    .doc = Das Notizbuch als Zip, eine Markdown-Datei für jede Notiz: die einer Notiz unter dem Ordner der Notiz, in der sie steht, und die Bilder, die sie zeigt, in ihrem eigenen Ordner `images` daneben.

d-hjwb45bx = Notizbuch als Markdown exportieren
    .doc = Das Notizbuch als ein Markdown-Dokument, in dem jede Notiz eine Überschrift eine Ebene tiefer als die Notiz, unter der sie steht, ist (sechs auf der tiefsten), ihr Text darunter steht und danach die Notizen unter ihr folgen, gelesen als ein Text in der Reihenfolge, in der der Baum sie führt.

d-t4nxxtng = Markdown importieren
    .doc = Importiert eine Markdown-Datei als Notiz oder öffnet die Notiz, die diese Datei schon angelegt hat: eine Notiz behält die Adresse der Datei, aus der sie kam, dieselbe Datei zweimal zu importieren ergibt also eine Notiz. Die Notiz steht unter der Notiz des Ordners, den der Pfad ihrer Datei nennt (`Q3 plan/Ideas.md` unter `Q3 plan.md`).
    .did = importierte eine Notiz

d-t4nxxtng-file = Datei

d-h3zczsen = Archiv importieren
    .doc = Importiert ein Zip aus Markdown (ein Notizbuch, das diese App exportiert hat, oder einen Notion-Export) als Notizen unter dem Notizbuch, eine Seite unter der Seite, die sie hielt, in einer Änderung: Rückgängig nimmt den ganzen Import zurück.
    .did = importierte ein Notizbuch

d-m6rqg3kf = Angeheftet

d-cn43mwee = Verschieben nach~present

d-7in2x6ql = Danach verschieben~present

d-ureyllbi = Neues Schlagwort~present

d-mapbyngl = Schlagwort~present

d-2f5tmgbl = Schlagwort umbenennen~present

d-ojfhuiuf = Untergeordnete hinzufügen
    .w-new-sub-note = Untergeordnete Notiz

d-5y75taxm = Notizknoten

d-v7cub7aw = Notizen
    .doc = Die Notizen des Arbeitsbereichs.
    .noun = { $count ->
        [one] Notizbuch
       *[other] Notizbücher
    }

d-tmqqtkeb = Notizen

d-akcqjax3 = Notizbuch
    .w-outline = Gliederung
    .w-tags = Schlagwörter
    .w-no-tags-yet = Noch keine Schlagwörter
    .w-new-tag = Neues Schlagwort
    .w-reacted-in-the-native-notes = In der nativen Notizen-App reagiert: { $reacted }
    .w-write-something = Etwas schreiben
    .w-image = Bild
    .w-page = Seite

d-aikvtwt5 = Notizen mit Schlagwort
    .w-nothing-carries-yet = Noch trägt nichts { $name }

d-x4im3xs5 = Notizbuchkopf

d-qrsfr4a5 = Notizbuchmenü
    .w-file = Datei
    .w-import-markdown = Markdown importieren…
    .w-import-a-notion-export-or-a = Ein Notion-Export oder ein Zip aus Markdown importieren…
    .w-export-the-notebook = Das Notizbuch exportieren…
    .w-export-the-notebook-as-one-markdown = Das Notizbuch als ein Markdown-Dokument exportieren…
    .w-insert-an-image = Ein Bild einfügen…
    .w-export-this-note = Diese Notiz exportieren…
    .w-new-sub-note = Neue Unternotiz

d-kdl43aw7 = Notizen
    .w-no-notes-yet = Noch keine Notizen

d-xqxkoqbj = Notizzeile

d-6wqou47w = Notizeditor
    .w-reacted-in-the-native-notes = In der nativen Notizen-App reagiert: { $reacted }
    .w-write-something = Etwas schreiben
    .w-image = Bild
    .w-page = Seite

d-nki6assy = Notizangaben
    .w-created = Erstellt
    .w-moved-here = Hierher verschoben
    .w-from = Von { $service }
    .w-from-by = Von { $service }, von { $by }
    .w-edited = Bearbeitet
    .w-1-word = 1 Wort
    .w-words = { $count } Wörter

d-ttq2d5ah = Dateiname
    .w-note = Notiz

# What each argument is called where a person gives it.

d-qhmjwgei-pinned = Angeheftet

d-7jhaq3a5-moved = Verschoben

d-7jhaq3a5-parent = Übergeordnet

d-7jhaq3a5-before = Davor

d-ai4sxgvt-moved = Verschoben

d-ai4sxgvt-n = N

d-c34rzynx-n = N

d-jo2tztsx-n = N

d-nydyehwn-n = N

d-zvgl2tzt-title = Titel

d-qymo5lg2-title = Titel

d-qymo5lg2-body = Text

d-qymo5lg2-parent = Übergeordnet

d-qymo5lg2-after = Danach

d-6fwsh5ui-text = Text

d-gc34cbhl-title = Titel

d-gc34cbhl-tag = Schlagwort

d-j33acdci-name = Name

d-7ow26bju-tag = Schlagwort

d-3u3bevec-tag = Schlagwort

d-ja7xx6nf-tag = Schlagwort

d-ja7xx6nf-wanted = Gewünscht

d-6hqlshe7-tag = Schlagwort

d-3ngyieqa-text = Text

d-4s5iqnfo-image = Bild

d-4s5iqnfo-alt = Alternativtext

d-h3zczsen-file = Datei

d-eo4enwxh = Von
    .doc = Wer die Seite dort geschrieben hat, als ein anderer Dienst sie vor diesem Notizbuch hielt: mit der Notiz bewahrt, damit eine übernommene Seite weiter sagt, wessen Worte sie sind.

d-yzessjhr = Dienst
    .doc = Der Dienst, von dem die Seite stammt, wenn sie von einem stammt (`Outline`).

d-zex2vawx = Seite
    .doc = Wo die Seite dort gelesen wird, wenn sie von dort stammt: dieselbe Seite noch einmal zu importieren öffnet diese Notiz, statt eine zweite anzulegen.

d-rnhwkv6z = Erstellt
    .doc = Wann die Seite dort geschrieben und zuletzt dort geändert wurde, wie jener Dienst es sagte.

d-2od7mwir = Geändert

d-q4w7m2xk = Gewählt
    .doc = Die Adresse des Bildes, das die Person zuletzt zum Einsetzen gewählt hat, und wie oft sie eines gewählt hat: die Seite übergibt beides dem Editor, der das Bild dort einsetzt, wo sie schreibt, statt am Ende der Notiz.

d-v8n3t5jb = Gewählt-Zähler

d-qgq6mcy7 = Zur Seite gelegt
    .doc = Notizen, die aus der früheren Notizen-App übernommen wurden und die sie nicht mehr zeigte: eine Seite, deren übergeordnete Seite gelöscht wurde, hier außerhalb des Baums bewahrt statt verworfen.

d-dq2mnb22 = Schlagwörter
    .doc = Die Schlagwörter, die die Notizen des Notizbuchs tragen, jedes ein Name, an dem Notizen hängen: eine Notiz behält ihre Schlagwörter als Wörter, und das Notizbuch führt die Liste, in der eine Person sie benennt, umbenennt und löscht.

d-qj3f52qw = Schlagwort
    .doc = Ein Schlagwort des Notizbuchs: ein Name, den Notizen tragen, einmal aufgeführt, damit eine Person es an einer Stelle liest, umbenennt und löscht, und jede Notiz, die es trägt, folgt.
    .noun = { $count ->
        [one] Schlagwort
       *[other] Schlagwörter
    }

d-itadoh74 = Name

d-cvwcei36 = Gefunden
    .doc = Eine Stelle in einer Notiz, an der die gesuchten Wörter stehen: der Absatz, in dem sie stehen, von der ersten der Notiz an gezählt, und seine Wörter.
    .noun = { $count ->
        [one] Treffer
       *[other] Treffer
    }

d-dxxdlopq = Bei
    .doc = Der Absatz, von der ersten der Notiz an gezählt (1).

d-2vlaqp45 = Wörter
    .doc = Die Wörter des Absatzes.

d-cvwcei36-filter-at = Bei

d-qvjedq2v = Früheres Notizbuch übernehmen
    .doc = Das Notizbuch, das die frühere Notizen-App vor der Übernahme dieser Kennung hielt, einmalig in dieses übernommen: jede Seite als Notiz, unter der Notiz, unter der ihre Seite stand, und in der Reihenfolge, in der ihre Seiten dort standen, mit Titel, Text, Schlagwörtern und der Kennung ihrer Seite.

d-pjxiode5 = Einrücken
    .doc = Setzt eine Notiz unter die Notiz, über der sie steht, mit den Notizen unter ihr, wie `Tab` es im Baum tut; ohne benannte unter die Notiz darüber.
    .w-nothing-over-it-to-move-under = Nichts darüber, worunter sie passt

d-pjxiode5-above = Darüber

d-bosv7337 = Ausrücken
    .doc = Setzt eine Notiz neben die Notiz, in der sie steht, direkt hinter sie und die Notizen unter ihr, wie `Shift-Tab` es im Baum tut.
    .w-it-is-at-the-top-level = Sie steht bereits auf der obersten Ebene

d-gsiqqwwd = Nach oben verschieben
    .doc = Setzt eine Notiz direkt über die Notiz darüber, mit den Notizen unter ihr, wie `Alt-Up` es im Baum tut; ohne benannte über die Notiz direkt darüber.
    .w-nothing-over-it-to-move-it = Nichts darüber, wohin sie käme

d-gsiqqwwd-above = Darüber

d-qi2ysxan = Nach unten verschieben
    .doc = Setzt eine Notiz direkt unter die Notiz darunter und die Notizen unter dieser, mit ihren eigenen Notizen danach, wie `Alt-Down` es im Baum tut.

d-s4ewx4fo = Einer Notiz hinzufügen
    .doc = Schreibt `text` (Markdown) an das Ende der Notiz mit dem Titel `title`, unabhängig von der Groß- und Kleinschreibung: wie ein Agent oder Skript, das eine Notiz bei ihrem Namen kennt ('das zu Besprechungsnotizen hinzufügen'), in sie schreibt, ohne erst nachzusehen, welche Notiz das ist.
    .did = hat einer Notiz hinzugefügt
    .w-no-note-is-titled = Keine Notiz heißt { $title }

d-s4ewx4fo-title = Titel

d-s4ewx4fo-text = Text

d-h6r2c9yd = Bild einsetzen
    .doc = Setzt ein Bild dort ein, wo die Person schreibt: das gewählte Bild wird dem Editor übergeben, der es an der Schreibmarke einsetzt, wie sein eigener Befehl `/image`.
    .did = fügte ein Bild hinzu

d-h6r2c9yd-image = Bild

d-tmlnqj7q = Bild behalten
    .doc = Behält ein Bild, das der Editor in eine Notiz geschrieben hat, dort, wo die Person es eingefügt oder abgelegt hat: die Notiz hält die Adresse seiner Bytes und den Namen, unter dem es kam, den ein Export daneben schreibt.
    .did = hat ein Bild hinzugefügt

d-tmlnqj7q-picture = Bild

d-l23y3cjf = Eine Seite aus einem Dienst importieren
    .doc = Importiert eine Seite, die ein anderer Dienst als Notiz hielt, und bewahrt, wessen Worte sie sind und wo sie dort stand: was `tilo migrate` für jede übernommene Seite aufruft, und was der eigene Import einer Datei ohne die Worte eines Dienstes tut.
    .did = hat eine Notiz importiert

d-l23y3cjf-file = Datei

d-l23y3cjf-by = Von

d-l23y3cjf-service = Dienst

d-l23y3cjf-page = Seite

d-l23y3cjf-created = Erstellt

d-l23y3cjf-updated = Geändert

d-fxhtpbwf = Eine Notiz als Text importieren
    .doc = Importiert eine Notiz als Wörter statt als Datei: ihren Titel, ihren Text als Markdown, die Bilder, die sie zeigt, und die Notizen, zu denen ihre Links führen.
    .did = hat eine Notiz importiert

d-fxhtpbwf-title = Titel

d-fxhtpbwf-markdown = Markdown

d-fxhtpbwf-pictures = Bilder

d-fxhtpbwf-links = Links

d-hy2uefpy = Dateien importieren
    .doc = Auf das Notizbuch abgelegte Dateien kommen nacheinander herein. Eine Markdown-Datei wird zu einer Notiz. Ein Archiv wird zu den Seiten, die es enthält, ob das eigene ausgegebene Notizbuch oder der Export einer anderen App.
    .did = hat eine Datei importiert

d-hy2uefpy-files = Dateien

d-i653dg7r = An eine Notiz anhängen~present
