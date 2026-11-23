# Video auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-video = Video

d-j4cx5fiu = Clip
    .doc = Ein Video, das jemand geöffnet hat: wie es heißt und woher seine Bytes abgespielt werden.
    .noun = { $count ->
        [one] Clip
       *[other] Clips
    }
    .untitled = Unbenanntes Video

d-sit6wbwo = Titel
    .doc = Der Name des Videos: die Datei, als die es geöffnet wurde, mit Endung, damit der Player das enthaltene Format liest.

d-vnxskfep = Quelle
    .doc = Die Adresse seiner Bytes: der Inhalt einer Datei (`tilo-blob://…`) oder die Adresse eines Streams.

d-dzmqpss7 = Wiedergabeliste
    .doc = Eine Wiedergabeliste, die alle im Arbeitsbereich sehen; ihre Videos sind die Einträge, die sie nennen.
    .noun = { $count ->
        [one] Wiedergabeliste
       *[other] Wiedergabelisten
    }

d-hktiqra6 = Name

d-vg4p5tbz = Wiedergabelisteneintrag
    .doc = Ein Video einer Wiedergabeliste.
    .noun = { $count ->
        [one] Wiedergabelisteneintrag
       *[other] Wiedergabelisteneinträge
    }

d-pmcgj5bo = Wiedergabeliste

d-gofxf7oh = Clip

d-vg4p5tbz-filter-playlist = Wiedergabeliste

d-vg4p5tbz-filter-clip = Clip

d-xfzvikx4 = Lesezeichen
    .doc = Ein Moment eines Videos, ein Lesezeichen, wenn es nichts sagt.
    .noun = { $count ->
        [one] Lesezeichen
       *[other] Lesezeichen
    }
    .untitled = Lesezeichen

d-h4e7iq7t = Clip

d-cwamirwi = Position
    .doc = Wo im Video es steht, in Sekunden.

d-n323noym = Text

d-xfzvikx4-filter-clip = Clip

d-xfzvikx4-filter-at = Position

d-pu5odurn = Bibliothek
    .doc = Die in diesem Arbeitsbereich geöffneten Videos.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-cfgqumjg = Clips

d-eajmuh6i = Sitzungen
    .doc = Jede Sitzung, die diese App angelegt hat, als eigene Ressourcen aufgehoben; `playing` nennt die eine.

d-7sp4sk7x = Wiedergabelisten

d-ve6kofwh = Einträge

d-srzszayp = Später ansehen
    .doc = Videos, die zum späteren Ansehen aufgehoben sind.

d-265jfqy5 = Lesezeichen

d-jcuq2e4s = Wiedergabe
    .doc = Was läuft und wo es steht: die gemeinsame Sitzung, die Video und Musik teilen.

d-pu5odurn-filter-playing = Wiedergabe

d-qr5yt6i6 = Sitzung
    .doc = Die Sitzung, über die diese App abspielt: die vorhandene oder eine jetzt angelegte.

d-hksyhpqm = Video öffnen
    .doc = Öffnet eine Videodatei: die Datei, die jemand ausgewählt hat, oder eine, die Files übergeben hat, und setzt sie dort fort, wo sie aufgehört hat.
    .did = öffnete ein Video

d-hksyhpqm-file = Datei

d-7vndi4ty = Wiedergabeliste erstellen
    .doc = Startet eine leere Wiedergabeliste.

d-7vndi4ty-name = Name

d-qx6xvn26 = Wiedergabeliste löschen
    .doc = Löscht eine Wiedergabeliste und die Einträge, die sie nennen.

d-qx6xvn26-pl = Wiedergabeliste

d-gy4thduh = Zu Wiedergabeliste hinzufügen
    .doc = Fügt `c` zu `pl` hinzu, einmal.

d-gy4thduh-pl = Wiedergabeliste

d-gy4thduh-c = Clip

d-ocgz6qq7 = Später ansehen
    .doc = Hebt `c` zum späteren Ansehen auf, einmal.

d-ocgz6qq7-c = Clip

d-fkgh2zyf = Lesezeichen hinzufügen
    .doc = Setzt ein Lesezeichen auf den Moment `at` von `c`, mit `text`, wenn jemand etwas geschrieben hat.

d-fkgh2zyf-c = Clip

d-fkgh2zyf-at = Position

d-fkgh2zyf-text = Text

d-5edgge6m = Lesezeichen löschen
    .doc = Löscht ein Lesezeichen oder eine Notiz.

d-5edgge6m-b = Lesezeichen

d-jowwlxkl = Abgelegte Videos öffnen
    .doc = Öffnet die Videos, die jemand auf das Fenster gelegt hat: jede Datei wird ihr Clip, und das erste läuft in der gemeinsamen Sitzung.
    .did = öffnete ein Video
    .w-drop-a-video-file = Eine Videodatei ablegen

d-jowwlxkl-files = Dateien

d-o42ih4gs = Wiedergabe umschalten
    .doc = Gibt die Sitzung wieder oder pausiert sie, wenn sie `c` zeigt: was die P-Taste und die Wiedergabesteuerung des Fensters ausführen, ohne die Bibliothek übergeben zu bekommen.

d-23vsff7g = Video abspielen
    .doc = Spielt `c` in der gemeinsamen Sitzung ab, von der Stelle, an der es aufhörte (beim ersten Mal vom Anfang): was die Play-Taste des Fensters, die Palette und das `play` eines Agenten ausführen.
    .did = spielte ein Video ab

d-23vsff7g-c = Clip

d-4hyldob5 = Von Anfang an abspielen
    .doc = Spielt `c` von seinem Anfang ab, wenn jemand es schon gesehen hat.

d-4hyldob5-c = Clip

d-zzrlp3fd = Öffnete ein Video

d-yydyqfvk = Bibliothek
    .w-drop-a-video = Ein Video ablegen
    .w-any-video-file = Jede Videodatei
    .w-play-a-video = Ein Video abspielen
    .w-open-a-video-file-from-your = Öffne eine Videodatei von deinem Gerät.
    .w-open-video = Video öffnen

d-6o4owyb6 = Video
    .w-open-video = Video öffnen
    .w-library = Bibliothek
    .w-play-from-the-beginning = Von Anfang an abspielen
    .w-bookmark-this-moment = Diesen Moment merken
    .w-watch-later = Später ansehen
    .w-queue = Warteschlange
    .w-shared = Geteilt
    .w-nothing-queued = Nichts in der Warteschlange
    .w-playlists = Wiedergabelisten
    .w-new-playlist = Neue Wiedergabeliste
    .w-nothing-kept-for-later = Nichts für später aufgehoben
    .w-bookmarks = Lesezeichen
    .w-no-bookmarks = Keine Lesezeichen
    .w-play = Abspielen
    .w-can-t-play-this-video = Dieses Video lässt sich nicht abspielen
