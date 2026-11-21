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

d-ngwafrjq = Von
    .doc = Wer die Seite dort geschrieben hat, wo sie herkam, als ein anderer Dienst sie vor diesem Notizbuch hielt: mit der Notiz aufbewahrt, damit eine herübergeholte Seite noch sagt, wessen Worte sie sind.

d-buuvovaf = Dienst
    .doc = Der Dienst, aus dem die Seite kam, wenn sie aus einem kam (`Outline`).

d-jznzkj47 = Seite
    .doc = Wo die Seite dort zu lesen ist, wenn sie aus einem Dienst kam: dieselbe Seite noch einmal zu importieren öffnet diese Notiz, statt eine zweite anzulegen.

d-hchfoqqm = Erstellt
    .doc = Wann die Seite dort geschrieben und zuletzt geändert wurde, wie der Dienst es sagte.

d-5wdceapz = Bearbeitet

d-gfk6rqxp = Angeheftet

d-xcpasjg7 = Schlagwörter

d-mavvhka7-filter-pinned = Angeheftet

d-mavvhka7-filter-tags = Schlagwörter

d-qymo5lg2 = Neue Notiz
    .doc = Beginnt eine Notiz oben im Baum, mit dem Titel `title` und dem Text `body` (Markdown), wenn der Aufrufer sie schon kennt.

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

d-6fwsh5ui = Text hinzufügen
    .doc = Schreibt `text` (Markdown) ans Ende einer Notiz, unter das, was sie schon sagt: eine Zeile, eine Liste oder ein Absatz dazu.
    .did = hat einer Notiz etwas hinzugefügt

d-4yaqj7z5 = Zu einer Notiz hinzufügen
    .doc = Schreibt `text` (Markdown) ans Ende der Notiz mit dem Titel `title`, gleich ob groß oder klein geschrieben: so schreibt ein Agent oder ein Skript, das eine Notiz bei ihrem Namen kennt („schreib das zu Besprechungsnotizen“), hinein, ohne erst nachzusehen, welche Notiz das ist.
    .did = hat einer Notiz etwas hinzugefügt
    .w-no-note-is-titled = Keine Notiz heißt { $title }

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
    .doc = Importiert eine Markdown-Datei als Notiz oder öffnet die Notiz, die diese Datei schon angelegt hat: eine Notiz behält die Adresse der Datei, aus der sie kam, dieselbe Datei zweimal zu importieren ergibt also eine Notiz. Die Notiz steht unter der Notiz des Ordners, den der Pfad ihrer Datei nennt (`Q3 plan/Ideas.md` unter `Q3 plan.md`).
    .did = importierte eine Notiz

d-t4nxxtng-file = Datei

d-n75ltziu = Eine Seite aus einem Dienst importieren
    .doc = Importiert eine Seite, die ein anderer Dienst hielt, als Notiz und behält, wessen Worte sie sind und wo sie dort stand: was `tilo migrate` für jede herübergeholte Seite aufruft, und was der eigene Import einer Datei ohne die Worte eines Dienstes tut.
    .did = importierte eine Notiz

d-n75ltziu-file = Datei

d-n75ltziu-by = Von

d-n75ltziu-service = Dienst

d-n75ltziu-page = Seite

d-n75ltziu-created = Erstellt

d-n75ltziu-updated = Bearbeitet

d-h3zczsen = Archiv importieren
    .doc = Importiert ein Zip aus Markdown (ein Notizbuch, das diese App exportiert hat, oder einen Notion-Export) als Notizen unter dem Notizbuch, eine Seite unter der Seite, die sie hielt, in einer Änderung: Rückgängig nimmt den ganzen Import zurück.
    .did = importierte ein Notizbuch

d-m6rqg3kf = Angeheftet

d-cn43mwee = Verschieben nach~present

d-7in2x6ql = Danach verschieben~present

d-dimkzymo = Zu einer Notiz hinzufügen~present

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
    .w-import-a-notion-export-or-a = Ein Notion-Export oder ein Zip aus Markdown importieren…
    .w-export-the-notebook = Das Notizbuch exportieren…
    .w-insert-an-image = Ein Bild einfügen…
    .w-export-this-note = Diese Notiz exportieren…

d-kdl43aw7 = Notizen
    .w-no-notes-yet = Noch keine Notizen

d-xqxkoqbj = Notizzeile

d-6wqou47w = Notizeditor

d-nki6assy = Notizangaben
    .w-from = Von { $service }
    .w-from-by = Von { $service }, von { $by }
    .w-created = Erstellt
    .w-edited = Bearbeitet
    .w-1-word = 1 Wort
    .w-words = { $count } Wörter

d-ttq2d5ah = Dateiname
    .w-note = Notiz

# What each argument is called where a person gives it.

d-37dxigeq-n = N

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

d-6fwsh5ui-text = Text

d-4yaqj7z5-title = Titel

d-4yaqj7z5-text = Text

d-gc34cbhl-title = Titel

d-gc34cbhl-tag = Schlagwort

d-7ow26bju-t = T

d-3u3bevec-t = T

d-4s5iqnfo-image = Bild

d-h3zczsen-file = Datei

d-zfjavdpr = Bild behalten
    .doc = Behält ein Bild, das der Editor dort in eine Notiz geschrieben hat, wo die Person es eingefügt oder abgelegt hat: die Notiz hält die Adresse seiner Bytes und den Namen, mit dem es kam, den ein Export daneben schreibt.
    .did = hat ein Bild hinzugefügt

d-zfjavdpr-picture = Bild
