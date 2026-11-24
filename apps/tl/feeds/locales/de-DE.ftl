# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-feeds = Feeds

d-so-tilo-feeds-keywords = rss, atom, feeds, feed, nachrichten, nachrichtenleser, newsfeed, abos, akregator, gemini, gemtext, nachrichten, abos

d-rapg7cgp = Ordner
    .doc = Ein Ordner, der verwandte Abos in der Seitenleiste zusammenfasst.
    .noun = { $count ->
        [one] Ordner
       *[other] Ordner
    }

d-ygksbjy5 = Name

d-lb5mig4n = Format

d-56bq23fl = Feed
    .doc = Ein Abo: der Feed, den eine Person liest, und was dieser Arbeitsbereich darüber weiß.
    .noun = { $count ->
        [one] Feed
       *[other] Feeds
    }

d-ruq4ynjw = Name
    .doc = Wie er auf dem Bildschirm heißt: sein eigener Titel, sobald eine Aktualisierung einen brachte, bis dahin die Adresse seiner Website.

d-jdxu5vhb = Adresse
    .doc = Die Adresse des Feeds, die eine Aktualisierung erneut liest.

d-kitws4yh = Website
    .doc = Die Website, von der er kommt.

d-ecrfpxxe = Ordner
    .doc = Der Ordner, in dem er steht; keiner für den Anfang der Seitenleiste.

d-2wtntp4k = Zertifikat
    .doc = Das Zertifikat, das ein `gemini://`-Server zuerst vorgelegt hat, als Hex: das Einzige, was jemanden auf dem Weg davon abhält, den Feed an seiner Stelle auszuliefern.

d-py34q4xk = Störung
    .doc = Warum die letzte Aktualisierung fehlschlug, in Worten; leer, solange er gut steht.

d-56bq23fl-filter-folder = Ordner

d-56bq23fl-filter-format = Format

d-a3monasj = Artikel
    .doc = Ein Artikel, den eine Aktualisierung brachte: was der Feed über ihn sagt, und was seine Leser entschieden haben.
    .noun = { $count ->
        [one] Artikel
       *[other] Artikel
    }
    .untitled = Artikel ohne Titel

d-chbpw5pg = Feed
    .doc = Das Abo, aus dem er kam.

d-swmvsojn = Titel

d-gvntfpn6 = Autor
    .doc = Wer ihn geschrieben hat; leer, wenn der Feed es nicht sagt.

d-p5lxkp7w = Adresse
    .doc = Die Adresse der eigenen Seite des Artikels.

d-s2wxy2go = Veröffentlicht
    .doc = Wann er veröffentlicht wurde, soweit der Feed es sagt.

d-ok5mmsqb = Zusammenfassung
    .doc = Seine erste Zeile, wie eine Zeile sie liest.

d-2yi7idis = Text
    .doc = Was er sagt, so wie der Feed es geschickt hat.

d-y7lqgqyr = Gelesen

d-hqmrnoz5 = Mit Stern

d-4nrqtndr = Schlagwörter
    .doc = Die eigenen Etiketten der Leser: klein geschrieben, ohne `#`.

d-a3monasj-filter-unread = Ungelesen

d-a3monasj-filter-starred = Mit Stern

d-a3monasj-filter-feed = Feed

d-a3monasj-filter-tags = Schlagwörter

d-tirekt2c = Leser
    .doc = Das Lesen einer Person: der Ort, an dem sie liest, und Größe, Schrift und Papier, in denen die Artikelseite gesetzt ist.
    .noun = { $count ->
        [one] Leser
       *[other] Leser
    }

d-tqbu72hj = Ort
    .doc = Der Ort in der Seitenleiste, an dem sie liest: `all`, `unread`, `starred`, `feed:<id>`, `folder:<id>` oder `tag:<name>`.

d-lx6zg57h = Größe
    .doc = Die Textgröße als Vielfaches der Lesegröße, zwischen 0.8 und 2.

d-um6nclsf = Schrift
    .doc = Die Schrift, in der der Artikel gesetzt ist: `serif`, `sans` oder `mono`.

d-to5unuga = Papier
    .doc = Die Seite hinter den Worten: `system` folgt dem Thema, oder eine feste `light`-, `sepia`- oder `dark`-Seite für langes Lesen.

d-gjs6qx4q = Format

d-5nvadw6d = Feed-Format
    .doc = Das Format, in dem ein Abo gelesen wird.

d-aasdudc2 = Ein RSS-Feed
    .doc = Ein RSS-Feed.

d-5hhba3hx = Ein Atom-Feed
    .doc = Ein Atom-Feed.

d-akvlsxee = Ein JSON-Feed
    .doc = Ein JSON-Feed.

d-ccbdoa7f = Eine Gemini-Kapsel
    .doc = Eine Gemini-Kapsel.

d-tirekt2c-filter-scale = Größe

d-hkyhrlx4 = Aktualisierung
    .doc = Wie weit eine Aktualisierung gekommen ist, und wie die letzte endete.
    .noun = { $count ->
        [one] Aktualisierung
       *[other] Aktualisierungen
    }

d-crwgoxd6 = Auftrag
    .doc = Der Auftrag, den das Gerät ausführt, an seiner Kennung; leer, solange keiner läuft.

d-dzn4ssl3 = Zustand
    .doc = `running`, während das Gerät die Abos liest, und `done`, sobald es fertig ist.

d-7d33zhis = Erledigt

d-mq3vs354 = Gesamt

d-pvpr56lq = Neu
    .doc = Wie viele Artikel die letzte Aktualisierung brachte.

d-gbspv2g7 = Fehlgeschlagen
    .doc = Wie viele Abos sie nicht lesen konnte.

d-dbqvtyzb = Hinweis
    .doc = Warum sie nicht starten konnte, in Worten; sonst leer.

d-hkyhrlx4-filter-done = Erledigt

d-hkyhrlx4-filter-total = Gesamt

d-hkyhrlx4-filter-fresh = Neu

d-hkyhrlx4-filter-failed = Fehlgeschlagen

d-u5mhrzrx = Bibliothek
    .doc = Die Feeds des Arbeitsbereichs: seine Ordner, seine Abos, die Artikel, die sie brachten, und wie jedes Mitglied des Arbeitsbereichs sie liest.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-urf3jsnz = Ordner

d-ba25drsv = Feeds

d-mymgo65x = Artikel

d-53g3dh5y = Leser
    .doc = Die Leser des Arbeitsbereichs: je ein Eintrag, allein seinem Leser eigen.

d-ptibzrny = Aktualisierung

d-u5mhrzrx-filter-refresh = Aktualisierung

d-4y3xzmua = Anfrage
    .doc = Die Abos, um die eine Aktualisierung bittet.
    .noun = { $count ->
        [one] Anfrage
       *[other] Anfragen
    }

d-ajc3gacu = Feeds

d-jl5g75cq = Feed-Anfrage
    .noun = { $count ->
        [one] Feed-Anfrage
       *[other] Feed-Anfragen
    }

d-wvgxfoj6 = Adresse

d-rzd7dr5x = Zertifikat

d-wptdjxom = Auftrag
    .doc = Wie weit das Gerät gekommen ist.
    .noun = { $count ->
        [one] Auftrag
       *[other] Aufträge
    }

d-vxpbania = Zustand

d-qen3hx3f = Erledigt

d-fm7ik3wg = Gesamt

d-krtut5p6 = Bereit

d-wptdjxom-filter-done = Erledigt

d-wptdjxom-filter-total = Gesamt

d-4mwlj5jz = Antwort
    .doc = Die Antwort eines Feeds, genommen, sobald sie eintrifft.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }

d-karmfgtt = Ok

d-mkw76ot4 = Titel

d-vxvxtmdp = Zertifikat

d-rcxpbdkb = Grund

d-tkh3vj2o = Status

d-3oef7xt5 = Meldung

d-in2no76u = Artikel
    .doc = Das eigene JSON jedes Artikels, als Text: eine Liste von Einträgen hält Ressourcen, und JSON-Text macht keine, also wird jeder hier in seinen Eintrag gelesen.

d-4mwlj5jz-filter-ok = Ok

d-4mwlj5jz-filter-status = Status

d-tl62g2cq = Gezogener Artikel
    .doc = Ein Artikel, den ein Feed auflistet.
    .noun = { $count ->
        [one] gezogener Artikel
       *[other] gezogene Artikel
    }

d-qablmehl = Titel

d-5sdtzscn = Autor

d-ae5rtinu = Adresse

d-gmi4oufp = Veröffentlicht

d-pj5brijd = Zusammenfassung

d-t2byonw5 = Text

d-tl62g2cq-filter-published = Veröffentlicht

d-mvrkvcfa = Abruf-Anfrage
    .noun = { $count ->
        [one] Abruf-Anfrage
       *[other] Abruf-Anfragen
    }

d-5mcvbcwk = Auftrag

d-ixh37lm6 = Adresse

d-cdqjjp54 = Umriss
    .doc = Was eine OPML-Datei auflistet.
    .noun = { $count ->
        [one] Umriss
       *[other] Umrisse
    }

d-esmrzlts = Titel

d-fhfn2dm2 = Adresse

d-akah2ujo = Website

d-otz67kho = Ordner

d-amdvzfos = Umrisse
    .noun = { $count ->
        [one] Umriss
       *[other] Umrisse
    }

d-z646rsds = Umrisse
    .doc = Das eigene JSON jedes Umrisses, als Text.

d-kzsmsaqw = OPML-Anfrage
    .noun = { $count ->
        [one] OPML-Anfrage
       *[other] OPML-Anfragen
    }

d-vg5idgn5 = Text

d-5xejf43x = OPML-Ausgabe
    .noun = { $count ->
        [one] OPML-Ausgabe
       *[other] OPML-Ausgaben
    }

d-pd5lnepq = Feeds

d-cyxxthpb = OPML-Feed
    .noun = { $count ->
        [one] OPML-Feed
       *[other] OPML-Feeds
    }

d-5w7b7tgv = Titel

d-5u77j2s2 = Adresse

d-igyb7lxx = Website

d-367at7x3 = Ordner

d-6dcuxfum = OPML-Text
    .noun = { $count ->
        [one] OPML-Text
       *[other] OPML-Texte
    }

d-q2mosawv = Text

d-ndtl6ntc = Hier lesen starten
    .doc = Gibt der Person hier ein eigenes Lesen, wenn sie noch keines hat: den Ort, an dem sie liest, und wie die Artikelseite gesetzt ist.

d-5hhcf7z5 = Ankommen
    .doc = Öffnet Feeds und gibt der Person ein eigenes Lesen, wenn sie hier noch keines hat.

d-hzsnnlnm = Beim Öffnen lesen
    .doc = Einen Artikel zu öffnen liest ihn, wie jeder Weg zu ihm es tut.

d-wcne5wpz = Ort zeigen
    .doc = Liest von `place` in der Seitenleiste.

d-wcne5wpz-place = Ort

d-ojrglzv3 = Schlagwort-Artikel zeigen
    .doc = Zeigt den Ort, an dem die Artikel eines Schlagworts stehen.

d-ojrglzv3-tag = Schlagwort

d-cdpf55sg = Feed des Artikels zeigen
    .doc = Zeigt den Ort, an dem der Feed eines Artikels ihn auflistet, damit ein Suchtreffer oder das `open` eines Agenten seine Zeile in die Liste daneben bringt.

d-w73iiryz = Abo hinzufügen
    .doc = Abonniert die Adresse eines Feeds, oder zeigt das Abo, das diese Bibliothek schon hat.
    .did = einen Feed abonniert
    .w-a-subscription-needs-a-feed-s = Ein Abo braucht die Adresse eines Feeds

d-w73iiryz-url = Adresse

d-w73iiryz-folder = Ordner

d-jzvp3vyd = Abo beenden
    .doc = Beendet das Abo: es verlässt die Seitenleiste, und was es brachte, geht mit ihm.
    .did = ein Abo beendet

d-jzvp3vyd-feed = Feed

d-apxioyg5 = Neuer Ordner
    .doc = Legt einen Ordner an, der verwandte Abos zusammenfasst; seine Abos bleiben, wenn er geht.
    .did = einen Ordner angelegt
    .w-a-folder-needs-a-name = Ein Ordner braucht einen Namen

d-apxioyg5-name = Name

d-uahbgvjf = Umbenennen
    .doc = Benennt einen Ordner um.
    .w-a-folder-needs-a-name = Ein Ordner braucht einen Namen

d-uahbgvjf-name = Name

d-lray7fou = Ordner löschen
    .doc = Löscht einen Ordner; seine Abos bleiben, am Anfang der Seitenleiste.
    .did = einen Ordner gelöscht

d-lray7fou-folder = Ordner

d-dduccdwb = Umbenennen
    .doc = Benennt ein Abo um, wie sein Feed sich auch nennt.
    .w-a-subscription-needs-a-name = Ein Abo braucht einen Namen

d-dduccdwb-name = Name

d-cmlsymqr = In Ordner verschieben
    .doc = Verschiebt ein Abo in einen Ordner, oder aus jedem Ordner heraus.

d-cmlsymqr-folder = Ordner

d-bmzxopbs = Alle als gelesen markieren
    .doc = Markiert alles, was der Ort `place` auflistet, als gelesen.
    .did = Artikel als gelesen markiert

d-bmzxopbs-place = Ort

d-ducv63vk = Als ungelesen markieren
    .doc = Markiert einen Artikel als gelesen oder ungelesen.
    .w-mark-as-unread = Als ungelesen markieren
    .w-mark-as-read = Als gelesen markieren

d-ducv63vk-read = Gelesen

d-iozr4xux = Stern entfernen
    .doc = Markiert einen Artikel mit einem Stern, oder nimmt ihn zurück.
    .w-remove-star = Stern entfernen
    .w-star-article = Artikel mit Stern markieren

d-iozr4xux-starred = Mit Stern

d-45kyblnz = Schlagwörter bearbeiten…
    .doc = Gibt einem Artikel die Schlagwörter, die eine Person getippt hat, durch Kommas getrennt: jedes beschnitten, klein geschrieben, ohne `#`, höchstens 24 davon.
    .did = einen Artikel verschlagwortet

d-45kyblnz-tags = Schlagwörter

d-wjiyczto = Alle Feeds aktualisieren
    .doc = Liest jedes Abo erneut.
    .did = die Feeds aktualisiert
    .w-there-are-no-subscriptions-to-refresh = Es gibt keine Abos zu aktualisieren
    .w-a-refresh-is-already-running = Eine Aktualisierung läuft bereits
    .w-couldn-t-start-fetching = Abruf konnte nicht gestartet werden: { $why }

d-io5ncepq = Aktualisierung stoppen
    .doc = Stoppt eine laufende Aktualisierung.

d-y4wmk3bn = Aktualisierung läuft
    .doc = Nimmt, was das Gerät bisher gelesen hat, während eine Aktualisierung läuft: die Antwort jedes Abos, sobald sie eintrifft, und was der Feed über sich selbst sagt.
    .w-a-subscription-s-answer-did-not = Die Antwort eines Abos ließ sich nicht lesen: { $why }
    .w-the-refresh-s-answer-did-not = Die Antwort der Aktualisierung ließ sich nicht lesen: { $why }

d-y4wmk3bn-j = J

d-lthzsf6i = Aktualisierung beendet
    .doc = Die Aktualisierung endete: sie ist fertig, oder eine Person hat sie gestoppt, oder sie ist gescheitert und das Fenster sagt warum.
    .w-the-refresh-stopped = Die Aktualisierung endete: { $error }

d-lthzsf6i-j = J

d-ompsa4v6 = OPML importieren…
    .doc = Abonniert, was eine OPML-Datei auflistet: jede Adresse, die die Bibliothek noch nicht hat, in den Ordnern, die die Datei nennt, angelegt, wo sie fehlen.
    .did = Abos importiert

d-ompsa4v6-file = Datei

d-2cdnd6w6 = OPML exportieren…
    .doc = Die Abos der Bibliothek als OPML-2.0-Datei.

d-bklq4t5c = Textgröße
    .doc = Legt fest, wie groß der Text des Artikels gesetzt ist: 0.8 bis 2 mal die Lesegröße.

d-bklq4t5c-scale = Größe

d-x3dxonwa = Lesetext vergrößern
    .doc = Einen Schritt größer im Lesen selbst.

d-n66gk5mg = Lesetext verkleinern
    .doc = Einen Schritt kleiner.

d-6if3si6p = Leseschrift
    .doc = Die Schrift, in der der Artikel gesetzt ist: `serif`, `sans` oder `mono`.
    .w-a-reading-face-is-serif-sans = Eine Leseschrift ist serif, sans oder mono

d-6if3si6p-face = Schrift

d-tntfvdsh = Lesepapier
    .doc = Das Papier, auf dem der Artikel gedruckt ist: `system` folgt dem Thema, oder eine feste Seite für langes Lesen.
    .w-a-paper-is-system-light-sepia = Ein Papier ist system, light, sepia oder dark

d-tntfvdsh-paper = Papier

d-5qtsbxfs = Grund
    .w-couldn-t-reach-the-site = Die Website war nicht erreichbar
    .w-the-site-refused-the-request-http = Die Website hat die Anfrage abgelehnt (HTTP { $status })
    .w-the-site-refused-the-request = Die Website hat die Anfrage abgelehnt
    .w-the-answer-wasn-t-a-feed = Die Antwort war kein Feed
    .w-the-site-s-certificate-changed-so = Das Zertifikat der Website hat sich geändert, jemand gibt sich womöglich als sie aus
    .w-couldn-t-start-fetching = Abruf konnte nicht gestartet werden

d-mb7lwq2a = Abo hinzufügen~present

d-j45dc6en = Neuer Ordner~present

d-y2laaqip = Ordner umbenennen~present

d-sehtg6ap = Abo umbenennen~present

d-5nbrso4z = Als gelesen markieren~present

d-wymgthpw = Mit Stern markieren~present

d-fuglynku = Aktualisieren~present

d-llfretkx = Schrift wählen~present

d-wayoq2pk = Papier wählen~present

d-qpdqpwvk = Feeds
    .w-feeds = Feeds
    .w-no-feeds-yet = Noch keine Feeds
    .w-subscribe-to-an-rss-atom-or = Abonniere einen RSS-, Atom- oder Gemini-Feed, um zu lesen.
    .w-add-feed = Feed hinzufügen

d-3dhsfin6 = Bibliotheksmenü
    .w-library = Bibliothek
    .w-new-folder = Neuer Ordner…
    .w-import-opml = OPML importieren…
    .w-export-opml = OPML exportieren…

d-xudd2qzl = Orte
    .w-all-articles = Alle Artikel
    .w-unread = Ungelesen
    .w-starred = Mit Stern
    .w-feeds = Feeds
    .w-folder = Ordner: { $id }
    .w-tags = Schlagwörter

d-5ohvr24p = Feed-Ort

d-k2jb46f2 = Aktualisierungsleiste
    .w-refreshing-of-feeds = Aktualisiere { $done } von { $total } Feeds
    .w-refreshing-feeds = Feeds werden aktualisiert
    .w-1-new-article = 1 neuer Artikel
    .w-new-articles = { $fresh } neue Artikel
    .w-all-feeds-are-up-to-date = Alle Feeds sind aktuell
    .w-stop = Stopp
    .w-1-feed-could-not-be-refreshed = 1 Feed konnte nicht aktualisiert werden
    .w-feeds-could-not-be-refreshed = { $failed } Feeds konnten nicht aktualisiert werden

d-2kgv5ybw = Ort-Leiste
    .w-open-site = Website öffnen
    .w-rename = Umbenennen
    .w-move-to-folder = In Ordner verschieben
    .w-unsubscribe = Abo beenden
    .w-delete-folder = Ordner löschen

d-ydy7dml3 = Artikelliste

d-4odicc56 = Artikelzeile
    .w-remove-star = Stern entfernen
    .w-star-article = Artikel mit Stern markieren

d-37yldcjy = Artikelzeile

d-ruo7nyba = Artikelseite
    .w-remove-star = Stern entfernen
    .w-star-article = Artikel mit Stern markieren
    .w-mark-as-unread = Als ungelesen markieren
    .w-mark-as-read = Als gelesen markieren
    .w-edit-tags = Schlagwörter bearbeiten…
    .w-open-original = Original öffnen
    .w-continuous = fortlaufend

d-zmnwlend = Lesemenü
    .w-reading-settings = Leseeinstellungen
    .w-larger-text = Größerer Text
    .w-smaller-text = Kleinerer Text
    .w-serif = Mit Serifen
    .w-sans = Ohne Serifen
    .w-mono = Dicktengleich
    .w-system-paper = Systempapier
    .w-light-paper = Helles Papier
    .w-sepia-paper = Sepiapapier
    .w-dark-paper = Dunkles Papier

d-wjiyczto-only = Nur
