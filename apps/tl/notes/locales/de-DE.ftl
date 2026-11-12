# Notes in German. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-lang-notes = Notizen

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

d-mavvhka7-filter-pinned = Angeheftet

d-mavvhka7-filter-tags = Schlagwörter

d-qymo5lg2 = Neue Notiz
    .doc = Beginnt eine Notiz oben im Baum.

d-37dxigeq = Untergeordnete Notiz
    .doc = Beginnt eine Notiz in der von `n`, nach den Notizen, die sie schon hält.

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

d-gc34cbhl = Notiz ablegen
    .doc = Legt in einem Schritt eine Notiz mit dem Titel `title` und dem Schlagwort `t` an: was eine andere App von dem aufschreibt, was sie getan hat (der Morgen in Heute), ohne auf die gemachte Notiz zu warten, um sie zu benennen.

d-7ow26bju = Verschlagworten
    .doc = Gibt einer Notiz ein Schlagwort.

d-3u3bevec = Schlagwort entfernen
    .doc = Nimmt ein Schlagwort von einer Notiz.

d-4s5iqnfo = Bild einfügen
    .doc = Setzt ein Bild in eine Notiz, dort wo ihre Worte enden: die Person wählt die Datei, und die Notiz behält die Adresse ihres Inhalts statt des Bildes, und den Namen, unter dem es aufgenommen wurde.
    .did = fügte ein Bild hinzu

d-xptsjaii = Markdown exportieren
    .doc = Eine Notiz als Markdown: ihr Titel, dann ihr Text, jedes Bild, das sie zeigt, daneben als `<note>/images/<name>`, ein Pfad, dem ein anderer Leser der Datei folgt und den diese App wieder als Adresse der Bilddaten liest.

d-2e5dloct = Notiz exportieren
    .doc = Eine Notiz als Datei: ihr Markdown, und die Bilder, die sie zeigt, daneben.

d-cdjz7d5y = Notizbuch exportieren
    .doc = Das Notizbuch als Zip, eine Markdown-Datei für jede Notiz: die einer Notiz unter dem Ordner der Notiz, in der sie steht, und die Bilder, die sie zeigt, in ihrem eigenen Ordner `images` daneben.

d-t4nxxtng = Markdown importieren
    .doc = Importiert eine Markdown-Datei als Notiz oben im Baum oder öffnet die Notiz, die diese Datei schon angelegt hat: eine Notiz behält die Adresse der Datei, aus der sie kam, dieselbe Datei zweimal zu importieren ergibt also eine Notiz.
    .did = importierte eine Notiz

d-h3zczsen = Archiv importieren
    .doc = Importiert ein Zip aus Markdown (ein Notizbuch, das diese App exportiert hat, oder einen Notion-Export) als Notizen unter dem Notizbuch, eine Seite unter der Seite, die sie hielt, in einer Änderung: Rückgängig nimmt den ganzen Import zurück.
    .did = importierte ein Notizbuch

d-m6rqg3kf = Angeheftet

d-cn43mwee = Verschieben nach~present

d-7in2x6ql = Danach verschieben~present

d-ojfhuiuf = Untergeordnete hinzufügen
    .w-new-sub-note = Untergeordnete Notiz

d-5y75taxm = Notizknoten

d-osyh3odz = Notizseite
    .w-write-something = Schreib etwas

d-v7cub7aw = Notizen
    .doc = Die Notizen des Arbeitsbereichs.
    .noun = { $count ->
        [one] Notizbuch
       *[other] Notizbücher
    }

d-tmqqtkeb = Notizen

d-akcqjax3 = Notizbuch

d-x4im3xs5 = Notizbuchkopf

d-qrsfr4a5 = Notizbuchmenü
    .w-file = Datei
    .w-import-markdown = Markdown importieren…
    .w-import-a-zip-of-markdown = Ein Zip aus Markdown importieren…
    .w-export-the-notebook = Das Notizbuch exportieren…
    .w-insert-an-image = Ein Bild einfügen…
    .w-export-this-note = Diese Notiz exportieren…

d-xqxkoqbj = Notizzeile

d-6wqou47w = Notizeditor

d-nki6assy = Notizangaben
    .w-created = Erstellt
    .w-edited = Bearbeitet
    .w-1-word = 1 Wort
    .w-words = { $count } Wörter

d-ttq2d5ah = Dateiname
    .w-note = Notiz
