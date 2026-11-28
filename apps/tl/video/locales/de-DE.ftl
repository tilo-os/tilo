# Video auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-video = Video

d-so-tilo-video-keywords = video, videos, videoplayer, film, filme, abspielen, anschauen, wiedergabe, medienspieler, clip, clips, medien, mp4, mkv, webm, mov, avi, player, movie, watch

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

d-s6n7w3gj = Tonspuren
    .doc = Die Tonspuren, die der Player darin gefunden hat, in der Reihenfolge, in der er sie zählt.

d-3hvcfvte = Tonspur
    .doc = Die Tonspur, die gewählt ist, nach ihrem Index; keine lässt die Wahl des Players.

d-vxy4abmr = Untertitelspuren
    .doc = Die Untertitelspuren, die der Player darin gefunden hat.

d-cmb3ivib = Untertitelspur
    .doc = Die Untertitelspur, die gewählt ist, nach ihrem Index; `-1` zeigt keine.

d-iryxnejv = Kapitel
    .doc = Die Kapitel, die der Player darin gefunden hat, in der Reihenfolge, in der die Zeitleiste sie markiert.

d-i7pv5r4r = Kapitelanfänge
    .doc = Wo jedes Kapitel beginnt, in Sekunden, in der Reihenfolge, in der `chapters` sie nennt.

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

d-7sp4sk7x = Wiedergabelisten

d-ve6kofwh = Einträge

d-srzszayp = Später ansehen
    .doc = Videos, die zum späteren Ansehen aufgehoben sind.

d-265jfqy5 = Lesezeichen

d-jcuq2e4s = Wiedergabe
    .doc = Was läuft und wo es steht: die gemeinsame Sitzung, die Video und Musik teilen.

d-eajmuh6i = Sitzungen
    .doc = Jede Sitzung, die diese App angelegt hat, als eigene Ressourcen aufgehoben; `playing` nennt die eine.

d-pu5odurn-filter-playing = Wiedergabe

d-kwojadb3 = Zuletzt gesehen
    .doc = Ein Video, das dieses Gerät abgespielt hat, und wo dieses Gerät aufgehört hat zu schauen.
    .noun = { $count ->
        [one] zuletzt gesehen
       *[other] zuletzt gesehen
    }

d-rdg5hfm3 = Clip

d-w433xqrc = Position
    .doc = Wo das Schauen aufhörte, in Sekunden.

d-lvtvvgk2 = Gesehen
    .doc = Wann zuletzt geschaut wurde, damit die Liste das Neueste zuerst zeigt.

d-kwojadb3-filter-clip = Clip

d-kwojadb3-filter-at = Position

d-kwojadb3-filter-watched = Gesehen

d-psx7dcx5 = Zuletzt gesehen
    .doc = Die Videos, die dieses Gerät abgespielt hat, das neueste zuerst.
    .noun = { $count ->
        [one] zuletzt gesehen
       *[other] zuletzt gesehen
    }

d-f7lphj6j = Videos

d-lerqlq4u = Wiedergabe-Einstellungen
    .doc = Wie dieses Gerät abspielt: was ein neu geöffnetes Video tut, mit welcher Geschwindigkeit es läuft und ob es am Ende von vorn beginnt.
    .noun = { $count ->
        [one] Wiedergabe-Einstellung
       *[other] Wiedergabe-Einstellungen
    }

d-w3zseuh3 = Automatisch abspielen
    .doc = Ein neu geöffnetes Video beginnt zu spielen.

d-pq35xopz = Geschwindigkeit
    .doc = Die Geschwindigkeit, mit der es läuft, von `0.25` bis `4`; `1` ist normal.

d-u6o56m6v = Wiederholen
    .doc = Ein Video beginnt am Ende von vorn.

d-lerqlq4u-filter-autoplay = Automatisch abspielen

d-lerqlq4u-filter-rate = Geschwindigkeit

d-lerqlq4u-filter-repeat_video = Wiederholen

d-hksyhpqm = Video öffnen
    .doc = Öffnet eine Videodatei: die Datei, die jemand ausgewählt hat, oder eine, die Files übergeben hat, und setzt sie dort fort, wo sie aufgehört hat.
    .did = öffnete ein Video

d-hksyhpqm-file = Datei

d-zejfv3u3 = Öffnen
    .doc = Nimmt ein geöffnetes Video auf: Es ist das neueste dieses Geräts und wartet pausiert an seiner Stelle, wenn gewählt ist, dass Videos nicht von selbst starten.

d-zejfv3u3-c = Clip

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

d-sqnfpjwr = Zu Lesezeichen springen
    .doc = Springt zu einem Lesezeichen: Das Video spielt ab dem Moment, den das Lesezeichen markiert.

d-sqnfpjwr-b = Lesezeichen

d-xmkzcyb7 = Umbenennen
    .doc = Benennt eine Wiedergabeliste um.
    .did = benannte eine Wiedergabeliste um

d-xmkzcyb7-name = Name

d-tcwb4khe = Wartendes Video abspielen
    .doc = Spielt einen Eintrag der Warteschlange ab und lässt den Rest der Warteschlange, wie er steht.

d-tcwb4khe-item = Eintrag

d-huzntei4 = Aus der Warteschlange entfernen
    .doc = Nimmt einen Eintrag aus der Warteschlange der Sitzung.

d-huzntei4-item = Eintrag

d-zcglai7s = Fortschritt merken
    .doc = Hält fest, wie weit der Player in `c` gekommen ist: die Stelle der Sitzung und die Zeile der zuletzt gesehenen Videos dieses Geräts.

d-zcglai7s-c = Clip

d-zcglai7s-share = Anteil

d-6pcwlru5 = Gesehen
    .doc = Hält fest, dass dieses Gerät `c` bis `at` gesehen hat, das neueste zuerst und die letzten 32 behalten.

d-6pcwlru5-c = Clip

d-6pcwlru5-at = Position

d-yzvwjk3f = Zuletzt gesehene Videos löschen
    .doc = Löscht die zuletzt gesehenen Videos dieses Geräts; die Clips bleiben in der Bibliothek.
    .did = löschte die zuletzt gesehenen Videos

d-mmxw7gge = Spuren des Videos gefunden
    .doc = Merkt sich die Tonspuren, Untertitel und Kapitel, die der Player in `c` gefunden hat, damit jemand daraus wählen kann und die Zeitleiste zeigt, wo ein Kapitel beginnt.

d-mmxw7gge-audio = Tonspur

d-mmxw7gge-subtitles = Untertitel

d-mmxw7gge-chapters = Kapitel

d-mmxw7gge-times = Positionen

d-rryapr2j = Zu Kapitel springen
    .doc = Springt zum Kapitel namens `name`: Das Video spielt ab dem Anfang des Kapitels.

d-rryapr2j-name = Name

d-juxlv3zp = Diese Tonspur verwenden
    .doc = Verwendet die Tonspur namens `name` dieses Videos.

d-juxlv3zp-name = Name

d-2jhna34f = Untertitel anzeigen
    .doc = Zeigt die Untertitelspur namens `name`, oder keine, wenn sie "Off" heißt.

d-2jhna34f-name = Name

d-hwfw32w4 = Weitere Video- und Audioformate abspielen
    .doc = Öffnet die Einstellungen, wo weitere Video- und Audioformate installiert werden.

d-cm6w76ww = Video beim Öffnen abspielen
    .doc = Spielt Videos, die ab jetzt geöffnet werden, ab oder lässt sie an ihrer Stelle, wenn sie geöffnet werden.

d-cm6w76ww-on = An

d-ulr5rpw5 = Wiedergabegeschwindigkeit
    .doc = Die Geschwindigkeit, mit der Videos auf diesem Gerät laufen, von `0.25` bis `4`.

d-ulr5rpw5-rate = Geschwindigkeit

d-uesjgphy = Video wiederholen
    .doc = Ob ein Video am Ende von vorn beginnt.

d-uesjgphy-on = An

d-jowwlxkl = Abgelegte Videos öffnen
    .doc = Öffnet die Videos, die jemand auf das Fenster gelegt hat: jede Datei wird ihr Clip, und das erste läuft in der gemeinsamen Sitzung.
    .did = öffnete ein Video
    .w-drop-a-video-file = Eine Videodatei ablegen

d-jowwlxkl-files = Dateien

d-2exzc5l7 = Wiedergabe
    .doc = Die Sitzung, über die diese App abspielt: die vorhandene oder eine jetzt angelegte.

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
    .w-local = Lokal

d-6o4owyb6 = Video
    .w-open-video = Video öffnen
    .w-library = Bibliothek
    .w-play-from-the-beginning = Von Anfang an abspielen
    .w-bookmark-this-moment = Diesen Moment merken
    .w-watch-later = Später ansehen
    .w-queue = Warteschlange
    .w-recent = Zuletzt gesehen
    .w-shared = Geteilt
    .w-nothing-queued = Nichts in der Warteschlange
    .w-remove-from-queue = Aus der Warteschlange entfernen
    .w-no-recent-videos = Keine zuletzt gesehenen Videos
    .w-stopped-at = Angehalten bei { $clock_text }
    .w-clear-recent-videos = Zuletzt gesehene Videos löschen
    .w-playlists = Wiedergabelisten
    .w-delete-shared-playlist = Geteilte Wiedergabeliste löschen
    .w-new-playlist = Neue Wiedergabeliste
    .w-nothing-kept-for-later = Nichts für später aufgehoben
    .w-bookmarks = Lesezeichen
    .w-no-bookmarks = Keine Lesezeichen
    .w-delete-timestamp-annotation = Zeitmarke löschen
    .w-play = Abspielen
    .w-can-t-play-this-video-yet = Dieses Video lässt sich noch nicht abspielen
    .w-chapters = Kapitel
    .w-play-more-video-and-audio-formats = Weitere Video- und Audioformate abspielen
    .w-play-a-video-when-it-opens = Video beim Öffnen abspielen
    .w-repeat-the-video = Video wiederholen
    .w-audio-track = Tonspur
    .w-subtitles = Untertitel
