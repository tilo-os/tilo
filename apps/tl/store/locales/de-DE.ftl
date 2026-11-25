# App Store auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are
# written from the package's sources.

d-so-tilo-store = App Store

d-so-tilo-store-keywords = store, apps, install, update, uninstall, get, flatpak, registry, repositories, bots, app store, apps installieren, aktualisieren, deinstallieren, quellen

d-wtooim2w = Version
    .doc = Eine signierte Version einer App, wie ihre Quelle sie anbietet.
    .noun = { $count ->
        [one] Version
       *[other] Versionen
    }

d-tv4yjmd7 = Version
    .doc = Die Version, wie ihre Quelle sie schreibt.

d-2jsas2xh = Veröffentlicht
    .doc = Der Tag, an dem ihre Quelle sie veröffentlicht hat.

d-xzuri67c = Wurzel
    .doc = Die signierte Paketwurzel, als Hex.

d-pryqr455 = Oberfläche
    .doc = Die Signatur, die das Paket selbst trägt, als Hex.

d-j7j23dzx = Größe
    .doc = Ihre Bytes, wie ihre Quelle sie angibt.

d-t73ddwug = Laufzeit
    .doc = Die Laufzeit, die sie braucht, wenn sie eine nennt.

d-a6avsz6m = Download
    .doc = Woher ihre Paketdatei geladen wird, wenn ihre Quelle einen Ort nennt.

d-wtooim2w-filter-size = Größe

d-3iyqd2ry = Berechtigung
    .doc = Eine Sache, die eine App auf diesem Computer tun kann, wie ihre Quelle sie angibt.
    .noun = { $count ->
        [one] Berechtigung
       *[other] Berechtigungen
    }

d-yrcnogu3 = Was
    .doc = Was sie anfragt, in den Worten der Quelle: `network`, `files:all`.

d-znsz4dgp = Umfassend
    .doc = Ob die Anfrage alles erreicht statt nur das, was eine Person auswählt.

d-3iyqd2ry-filter-broad = Umfassend

d-cqetz3au = Veröffentlichung
    .doc = Was sich in einer Version einer App geändert hat, wie ihre Quelle es angibt.
    .noun = { $count ->
        [one] Veröffentlichung
       *[other] Veröffentlichungen
    }

d-dbblppxc = Version

d-sil2634m = Hinweise

d-thnissxq = App
    .doc = Eine App, die der Katalog kennt: was sie ist, woher sie kommt, was sie anbietet und wie weit sie auf den Geräten dieses Arbeitsbereichs ist.
    .noun = { $count ->
        [one] App
       *[other] Apps
    }

d-bsz5kepr = Identität
    .doc = Die Identität der App: ihre Paket-ID.

d-z7s7wy7i = Name

d-7mugzbgk = Zusammenfassung
    .doc = Die eine Zeile, die eine Zeile zeigt: der Werbesatz oder die erste Zeile der Beschreibung.

d-xmhc7rzm = Beschreibung

d-6igcsadc = Entwickler

d-y6tadyrs = Lizenz

d-s2h3qmpk = Kategorien

d-z2ymswmt = Symbol
    .doc = Ihr Symbol als Webadresse, die das Gerät lädt.

d-fbg5gcf6 = Bildschirmfotos

d-iuswhjxs = Webseite

d-dmpicgut = Quelltext

d-dyqo2u55 = Herkunft

d-gkbbivsc = Registry
    .doc = Die Registry oder das Repository, über das sie kam, wie Menschen sie lesen.

d-febktfoe = Zweig
    .doc = Der Zweig, in dem ein Flatpak-Repository sie baut.

d-qay7cvdi = Von Benutzern verwaltet
    .doc = Ob dieser Arbeitsbereich eine Repository-App ändern darf: ein eigenes Flatpak-Repository eines Computers gehört seiner Administration.

d-hptnn5eg = Vertrauen

d-kqqhh6as = Versionen

d-sx2epvkk = Verfügbar
    .doc = Die Versionen, die ihre Quelle anbietet, neueste zuerst, als Text.

d-rssj2htx = Installiert
    .doc = Die Version, die dieser Arbeitsbereich ausführt; leer, wenn er keine ausführt.

d-qqimyoka = Festgehalten
    .doc = Ob dieser Arbeitsbereich die Version festhält, die er ausführt.

d-ev27rmap = Geladen
    .doc = Die Version, die dieser Computer für den Arbeitsbereich geladen hat.

d-lmny564e = Ausführbar
    .doc = Ob ihre Quelle sie für diesen Computer baut.

d-54f6mqrf = Prozessoren
    .doc = Die Prozessoren, für die sie gebaut ist, in den Worten der Quelle, wenn dieser Computer keiner davon ist.

d-qbk2t2ii = Wartet auf Entwickler
    .doc = Ihre Builds sprechen alle eine ältere Schnittstelle, als dieses System anbietet.

d-fmc644bz = Empfohlen
    .doc = Ihr Platz in der Empfehlungsliste ihrer Quelle, kleinere zuerst; 0, wenn sie nicht empfohlen wird.

d-5vq3mjxe = Aktualisiert
    .doc = Der Tag, an dem ihre Quelle sie zuletzt geändert hat.

d-aeiz77yf = Herausgeber

d-h6fnaygl = Prüfsumme
    .doc = Die Prüfsumme des Pakets, das dieser Computer geladen hat, als Hex.

d-7tdammi6 = Bot
    .doc = Ein Verzeichnis-Bot: was er tut und braucht, und keine Version zum Laden auf diesem Computer.

d-rnazc2e5 = Bot-Auslöser

d-su65i54b = Bot-Apps

d-4qzdclse = Bot-Konnektoren
    .doc = Seine Konnektoren, jeweils als „Name, Version oder neuer“.

d-ckhofqel = Bot-Erscheinung

d-ehbiqgtb = Berechtigungen
    .doc = Was sie anfragt, sobald ihre Seite die Angaben ihrer Quelle gelesen hat.

d-foea7x37 = Veröffentlichungen

d-fngfv6n4 = Größe
    .doc = Die Bytes, die ihre Quelle lädt, wenn sie es angibt.

d-cifjx7ck = Installierte Größe
    .doc = Die Bytes, die sie installiert belegt, wenn ihre Quelle es angibt.

d-w626nsxj = Sprachen
    .doc = Die Sprachen, in denen sie geschrieben ist, wie ihre Quelle es angibt.

d-25c7mbdv = Referenz
    .doc = Die `.flatpakref`, aus der sie geöffnet wurde, wenn aus einer.

d-tdkfnlcz = Laufzeit
    .doc = Die Laufzeit, die sie braucht, und das Repository, das sie anbietet.

d-qnnzjwd4 = Laufzeit-Registry

d-babs5eju = Auftrag
    .doc = Die Änderung, die für sie läuft, nach der Auftrags-ID der Engine.

d-66hvm5fy = Angaben gelesen
    .doc = Ob ihre Seite gelesen hat, was ihre Quelle über sie sagt.

d-7l5m7ka6 = Aktualisierbar
    .doc = Ob eine Quelle eine neuere Version anbietet, als dieser Computer ausführt.

d-awr7kqfn = Problem
    .doc = Warum dieser Computer sie nicht ändern kann, in den Worten der Quelle; leer, wenn er kann.

d-thnissxq-filter-discover = Entdecken

d-thnissxq-filter-updates = Aktualisierungen

d-thnissxq-filter-on_this_device = Auf diesem Gerät

d-thnissxq-filter-bots = Bots

d-thnissxq-filter-categories = Kategorien

d-b67eswak = Repository
    .doc = Ein Flatpak-Repository, wie dieser Computer und dieser Arbeitsbereich es anbieten.
    .noun = { $count ->
        [one] Repository
       *[other] Repositories
    }

d-q6znklmy = Name

d-e74rqhdi = Titel

d-yimcob4p = Adresse

d-7o2l4sfb = Geltungsbereich
    .doc = „device“ oder „account“: wem das Repository gehört.

d-gbitboxx = Aktiviert

d-ixnjbq5u = Problem
    .doc = Warum sein Katalog nicht gelesen werden konnte, wenn er es nicht konnte.

d-mdalqw5g = Anzahl
    .doc = Wie viele Apps sein Katalog enthält, sobald er gelesen wurde.

d-pag3getc = Geladen
    .doc = Ob seine Apps geladen sind.

d-b67eswak-filter-enabled = Aktiviert

d-b67eswak-filter-count = Anzahl

d-b67eswak-filter-loaded = Geladen

d-ptgjb7qs = Sperre
    .doc = Jemandes Sperre einer App.
    .noun = { $count ->
        [one] Sperre
       *[other] Sperren
    }

d-qexvvrxm = Identität
    .doc = Die Identität der App.

d-cyq6y3du = Meldung
    .doc = Eine Meldung über eine App: ein Entwurf, den dieser Arbeitsbereich teilt, bis jemand sie absendet, dann der Fall der Registry und sein Status.
    .noun = { $count ->
        [one] Meldung
       *[other] Meldungen
    }

d-i2w5sqnt = Identität
    .doc = Die Identität der App.

d-tmhftkva = Notiz
    .doc = Was die Person gesehen hat, als Beleg, den eine andere Prüferin nachvollziehen kann.

d-riwxceey = Status
    .doc = Der Status der Registry: „Nicht abgesendet“, bis sie antwortet.

d-edrxm3vo = Fall
    .doc = Die Fall-ID der Registry, sobald es eine gibt.

d-sv5euoyy = Vorgang
    .doc = Der signierte Vorgang, den die Registry angenommen hat, damit eine Aktualisierung oder ein Einspruch genau ihn nennt.

d-sez3dgh3 = Registry
    .doc = Die Registry, die den Fall führt.

d-r4polcvt = Store
    .doc = Der App Store des Arbeitsbereichs: der Katalog, den seine Quellen anbieten, die eingerichteten Repositories, die Meldungen der Menschen und die Sperren jeder Person.
    .noun = { $count ->
        [one] Store
       *[other] Stores
    }

d-t6lkc6fh = Apps

d-gvveyw3n = Repositories

d-5g4uijlp = Meldungen

d-a6ibqgbw = Sperren

d-72gq3jue = Status
    .doc = Was das letzte Lesen der Quellen ergeben hat.

d-sotfrvum = Problem
    .doc = Warum das letzte Lesen der Quellen fehlgeschlagen ist, wenn es das ist.

d-eubzl4qx = Lesen
    .doc = Das Lesen, das jetzt läuft, nach der Auftrags-ID der Engine.

d-e22zgwb6 = Flatpak
    .doc = Ob dieser Computer Flatpak hat.

d-uxwwy2fl = Flatpak-Problem
    .doc = Warum das Flatpak dieses Computers nicht gelesen werden konnte.

d-yut35juc = Ohne Antwort
    .doc = Die aktivierten Registries, die beim letzten Lesen nicht geantwortet haben.

d-y32wfrks = Abgelaufen
    .doc = Die aktivierten Registries, deren Katalog abgelaufen ist.

d-452zgqdl = Geprüfte Wurzel
    .doc = Das Paket, das jemand der App gegeben hat, geprüft, aber noch nicht installiert: seine Wurzel und die Datei, aus der es gelesen wurde.

d-5ynamzdl = Geprüfte App

d-vihbdrl7 = Geprüfte Datei

d-r4polcvt-filter-flatpak = Flatpak

d-r4polcvt-filter-unanswered = Ohne Antwort

d-r4polcvt-filter-expired = Abgelaufen

d-h45lelxn = Eintrag
    .doc = Eine App aus einer Quelle, wie die Engine sie gelesen hat.
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }

d-epxtvzuf = Identität

d-6a7tsprp = Name

d-ea2bjyc3 = Zusammenfassung

d-gpwdasim = Beschreibung

d-tns2iluv = Entwickler

d-2zltc7rw = Lizenz

d-ukzeb4vu = Kategorien

d-uqkifo7y = Symbol

d-5kd5ubmn = Bildschirmfotos

d-elvkwauw = Webseite

d-okmosbps = Quelltext

d-mke4chcm = Herkunft

d-xbvh2wz3 = Registry

d-cpannwt7 = Zweig

d-wikfckou = Von Benutzern verwaltet

d-nwfophjp = Vertrauen

d-i4hnlx7u = Versionen
    .doc = Die Versionen, die ihre Quelle anbietet, jede als eigener JSON-Text.

d-ehs57hul = Verfügbar

d-x3qagtfz = Installiert

d-36fsx2jx = Festgehalten

d-cnrxvmkp = Geladen

d-7jxbeqay = Ausführbar

d-nyj6afge = Prozessoren

d-jsqjxd7b = Wartet auf Entwickler

d-taengdq6 = Empfohlen

d-cvzbzfol = Aktualisiert

d-px6eamz7 = Herausgeber

d-rga5kuoh = Prüfsumme

d-na6yqlcj = Bot

d-lb5drfvf = Bot-Auslöser

d-chtwyqcq = Bot-Apps

d-jcezfbrh = Bot-Konnektoren

d-cj5tnnei = Bot-Erscheinung

d-ojgbghet = Laufzeit

d-j5umq2va = Aktualisierbar

d-2nmtgjmg = Problem

d-czhobryy = Unterstützte Sprachen
    .doc = Die Sprachen, in denen sie laut ihrer Quelle geschrieben ist.

d-h45lelxn-filter-user_managed = Von Benutzern verwaltet

d-h45lelxn-filter-pinned = Festgehalten

d-h45lelxn-filter-runnable = Ausführbar

d-h45lelxn-filter-awaiting_developer = Wartet auf Entwickler

d-h45lelxn-filter-featured = Empfohlen

d-h45lelxn-filter-bot = Bot

d-h45lelxn-filter-bot_appearance = Bot-Erscheinung

d-h45lelxn-filter-updatable = Aktualisierbar

d-ljjwi6ge = Katalog
    .doc = Was ein Lesen der Quellen gefunden hat.
    .noun = { $count ->
        [one] Katalog
       *[other] Kataloge
    }

d-mr6dwxv6 = Einträge
    .doc = Jede App, die eine Quelle gelesen hat, als eigener JSON-Text.

d-onje62ck = Repositories
    .doc = Jedes Repository als eigener JSON-Text.

d-ncdaffvy = Status

d-kntjub7s = Problem

d-av6ojjws = Flatpak

d-xe2u37td = Flatpak-Problem

d-elx5dznp = Ohne Antwort

d-dhwcxbjv = Abgelaufen

d-ljjwi6ge-filter-flatpak = Flatpak

d-zvyjoyxd = Gelesenes Repository
    .doc = Ein Repository, wie die Engine es gelesen hat.
    .noun = { $count ->
        [one] gelesenes Repository
       *[other] gelesene Repositories
    }

d-rz4t6wig = Name

d-roitw26x = Titel

d-635v2xjh = Adresse

d-xkljozjv = Geltungsbereich

d-e4ylyjav = Aktiviert

d-rk5vdazr = Problem

d-zvyjoyxd-filter-enabled = Aktiviert

d-bnyohoe2 = Angaben
    .doc = Was das Lesen einer Seite über eine App hinzufügt: die Angaben der Quelle.
    .noun = { $count ->
        [one] Angabe
       *[other] Angaben
    }

d-6cb7jl47 = Beschreibung

d-7cwn55x3 = Entwickler

d-mptnhu62 = Lizenz

d-a37qv2oa = Webseite

d-uba5apij = Quelltext

d-nt6mule6 = Größe

d-lrlxwbcb = Installierte Größe

d-oj36amou = Bildschirmfotos

d-g3nhe33n = Berechtigungen

d-lygslquo = Veröffentlichungen

d-whmuitft = Laufzeit

d-uijcroeo = Laufzeit-Registry

d-it2pk5w4 = Prüfsumme

d-gmjvqfez = Unterstützte Sprachen

d-bnyohoe2-filter-size = Größe

d-bnyohoe2-filter-installed_size = Installierte Größe

d-g7icceba = Prüfung
    .doc = Ein geprüftes Paket, bereit zur Installation.
    .noun = { $count ->
        [one] Prüfung
       *[other] Prüfungen
    }

d-mzd6fvgn = Eintrag

d-hxcclid7 = Wurzel

d-oiutpkg7 = Problem

d-tvnwz7z4 = Fall
    .doc = Die Antwort eines Repositories zu einem Fall.
    .noun = { $count ->
        [one] Fall
       *[other] Fälle
    }

d-zyv4ydev = Fall

d-hf6ogyp3 = Status

d-o4zyd7r3 = Lage

d-zznn4lla = Problem

d-7yi3ywbf = Vorbereitet
    .doc = Eine für die Registry vorbereitete Meldung: der Vorgang, den sie signiert, und die Registry, die ihn führt.
    .noun = { $count ->
        [one] Vorbereitetes
       *[other] Vorbereitete
    }

d-wywxs5oj = Vorgang

d-zenjiysp = Registry

d-owkrdhjj = Problem

d-jmxig2ub = Auftragswörter
    .doc = Die eigene Zeile der Engine über einen Auftrag, sobald er sich bewegt hat.
    .noun = { $count ->
        [one] Auftragswort
       *[other] Auftragswörter
    }

d-pwpo5osg = Status

d-vtmqhpab = Gelesener Fall
    .doc = Der Moderationsfall einer Registry, wie diese App ihn für eine Aktualisierung behält.
    .noun = { $count ->
        [one] gelesener Fall
       *[other] gelesene Fälle
    }

d-wi7dxv7v = Fall

d-f67cduxy = Status

d-5cmcfoao = Lage

d-u3qiqphi = Änderung
    .doc = Eine Änderung, die eine App von der Engine verlangt: was sie ist, die genaue Version, die ihre Quelle angeboten hat, und die Referenz, die ein Flatpak installiert.
    .noun = { $count ->
        [one] Änderung
       *[other] Änderungen
    }

d-sr6eixi6 = Identität

d-rkd5rikz = Art

d-wfeji5lc = Version

d-gawnzinv = Wurzel

d-xz347kth = Oberfläche

d-wa7a4ebr = Herausgeber

d-jalnpzfr = Flatpak

d-uuhlsrfk = Referenz

d-2usub7jq = Datei

d-smpzmxa2 = Größe

d-u3qiqphi-filter-size = Größe

d-dhqg2zaa = Anfrage
    .doc = Ein Lesen der Quellen, wie die Engine es entgegennimmt.
    .noun = { $count ->
        [one] Anfrage
       *[other] Anfragen
    }

d-k33jf5d6 = Erzwingen

d-dhqg2zaa-filter-force = Erzwingen

d-4t4usi52 = Flathub-Anfrage
    .doc = Was für eine Seite von Flathubs Katalog angefragt wird.
    .noun = { $count ->
        [one] Flathub-Anfrage
       *[other] Flathub-Anfragen
    }

d-yk5x3lyl = Suchbegriff

d-jyhorffp = Treffer pro Seite

d-n6ivjlst = Seite

d-4t4usi52-filter-hits_per_page = Treffer pro Seite

d-4t4usi52-filter-page = Seite

d-g5vn7lms = Einzelanfrage
    .doc = Wonach eine App gefragt wird: nach ihrer ID und der Registry ihrer Quelle, wenn ein Bot daraus hinzugefügt wird.
    .noun = { $count ->
        [one] Einzelanfrage
       *[other] Einzelanfragen
    }

d-bzdopfzn = Identität

d-kz4m2q7x = Registry

d-64wyfi3e = Eines
    .noun = { $count ->
        [one] Eines
       *[other] Einzelne
    }

d-ibr65ngi = Eintrag

d-wmgpz4vn = Ein Lesen
    .noun = { $count ->
        [one] Lesen
       *[other] Lesen
    }

d-gwl2nm5a = Eintrag

d-wmgpz4vn-filter-item = Eintrag

d-ibsutfg5 = Gelesene Repositories
    .noun = { $count ->
        [one] gelesenes Repository
       *[other] gelesene Repositories
    }

d-6476ldq6 = Repositories
    .doc = Jedes Repository als eigener JSON-Text.

d-jr4k76wg = Flatpak

d-zt4zikpc = Problem

d-ibsutfg5-filter-flatpak = Flatpak

d-oy3ekw2b = Repository-Anfrage
    .noun = { $count ->
        [one] Repository-Anfrage
       *[other] Repository-Anfragen
    }

d-7ltvsddp = Name

d-p4q2b4s7 = Adresse

d-vmyl5m7m = Aktiviert

d-oy3ekw2b-filter-enabled = Aktiviert

d-qebm5qpq = Sperr-Anfrage
    .noun = { $count ->
        [one] Sperr-Anfrage
       *[other] Sperr-Anfragen
    }

d-dlrecm5k = Identität

d-xjtxb4gh = Gesperrt

d-qebm5qpq-filter-blocked = Gesperrt

d-3su3ovbd = Paket-Anfrage
    .noun = { $count ->
        [one] Paket-Anfrage
       *[other] Paket-Anfragen
    }

d-fxwybrap = Datei

d-qaju5cxd = Name

d-bkjyjpcr = Größe

d-3su3ovbd-filter-size = Größe

d-ta3ymo2c = Laufzeit-Anfrage
    .noun = { $count ->
        [one] Laufzeit-Anfrage
       *[other] Laufzeit-Anfragen
    }

d-vpcuziom = Laufzeit

d-snmv7oek = Registry

d-pjq42giv = Meldungs-Anfrage
    .noun = { $count ->
        [one] Meldungs-Anfrage
       *[other] Meldungs-Anfragen
    }

d-3x7qwbnh = Identität

d-4ibtuh5s = Notiz

d-gea2szrq = Fall

d-6qwyarvg = Registry

d-jjlocy5a = Einspruch

d-pjq42giv-filter-appeal = Einspruch

d-4mcprvp6 = Sende-Anfrage
    .noun = { $count ->
        [one] Sende-Anfrage
       *[other] Sende-Anfragen
    }

d-mn7ndrul = Identität

d-rq53vgig = Vorgang

d-f72wtnsy = Notiz

d-3knw3o2v = Status-Anfrage
    .noun = { $count ->
        [one] Status-Anfrage
       *[other] Status-Anfragen
    }

d-5xifqzkr = Identität

d-pdu4bxlq = Fall

d-dnrxsz6b = Registry

d-o23b3xvi = Änderungen
    .noun = { $count ->
        [one] Änderung
       *[other] Änderungen
    }

d-zxtxkidh = Liste

d-dj5auzr7 = Einträge
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }

d-r4t6n2uh = Einträge

d-k5i5m2oa = Beleg
    .noun = { $count ->
        [one] Beleg
       *[other] Belege
    }

d-j6n5hmtr = Identität

d-bnqubb5u = Notiz

d-cbhojiz7 = Status

d-t7a6wmgq = Fall

d-ra5kasg4 = Registry

d-dvwnjvtj = Vorgang

d-tdmfwxqo = Herkunft
    .doc = Woher eine App im Katalog kommt.

d-c3vyt4cm = Eine Tilo-Registry
    .doc = Eine Tilo-Registry.

d-mlsekpul = Flathub
    .doc = Flathub.

d-ibi4yaz6 = Ein Flatpak-Repository, das dieser Arbeitsbereich oder Computer eingerichtet hat
    .doc = Ein Flatpak-Repository, das dieser Arbeitsbereich oder Computer eingerichtet hat.

d-5ufarnar = Eine Paketdatei, die jemand dieser App gegeben hat
    .doc = Eine Paketdatei, die jemand dieser App gegeben hat.

d-jzntjxm2 = Vertrauen
    .doc = Was die Prüfung einer Quelle über eine App ergibt.

d-wieqigty = Die Signatur ihres Herausgebers und die Prüfung der Registry
    .doc = Die Signatur ihres Herausgebers und die Prüfung der Registry.

d-bulyhjth = Flathub, das Tilo selbst nicht geprüft hat
    .doc = Flathub, das Tilo selbst nicht geprüft hat.

d-qloka2bk = Ein Repository, das dieser Arbeitsbereich eingerichtet hat und das Tilo nicht geprüft hat
    .doc = Ein Repository, das dieser Arbeitsbereich eingerichtet hat und das Tilo nicht geprüft hat.

d-3asz6773 = Ein Paket, das ein Attestor geprüft hat, dem dieser Computer schon vertraut
    .doc = Ein Paket, das ein Attestor geprüft hat, dem dieser Computer schon vertraut.

d-2klx4tra = Aktualisieren
    .doc = Liest die Quellen des Arbeitsbereichs erneut: die Registries, Flathub und die Flatpak-Repositories dieses Computers, und was der Arbeitsbereich ausführt.

d-e3kequ3n = Lesen bewegt
    .doc = Das Lesen ging weiter: die Zeile, die das Fenster zeigt, während es läuft.

d-e3kequ3n-j = J

d-gwtrq5yk = Lesen fertig
    .doc = Das Lesen endete: der Katalog, den es fand, oder warum es keinen fand.

d-gwtrq5yk-j = J

d-mkrs77sc = Änderung fertig
    .doc = Der Auftrag, den die Änderung einer App ist, so weit die Engine gekommen ist: seine Zeile sagt, wo er steht, und was er gemacht hat, wird gelesen, sobald er endet.

d-mkrs77sc-a = A

d-mkrs77sc-j = J

d-banfx2pr = Lesen beginnen
    .doc = Ein Auftrag, der den Katalog selbst ändert statt einer App, verfolgt wie das Lesen: sein Ende liest die Quellen erneut, sodass ein neues Repository oder eine Aktualisierung von allem ankommt.

d-banfx2pr-op = Vorgang

d-banfx2pr-ask = Anfrage

d-emblroed = Beginnen
    .doc = Ein Auftrag, den die Engine für `a` gestartet hat, oder warum sie keinen gestartet hat.

d-emblroed-a = A

d-emblroed-op = Vorgang

d-emblroed-ask = Anfrage

d-s3xf57z2 = App lesen
    .doc = Liest eine App, wie sie jetzt steht: was der Arbeitsbereich ausführt, was der Computer geladen hat und das neueste Angebot ihrer Quelle.

d-s3xf57z2-a = A

d-6v23i3ur = Angaben anzeigen
    .doc = Liest die Seite einer App: was ihre Quelle über sie sagt.

d-6v23i3ur-a = A

d-kcpgrqgg = Katalog übernehmen
    .doc = Der Katalog, den ein Lesen gefunden hat, in den Arbeitsbereich gelegt: Apps in der Reihenfolge, in der die Quellen sie gaben, jede an ihrem Platz ersetzt, und die, die ihre Quellen nicht mehr listen, entfernt.

d-kcpgrqgg-status = Status

d-f3masrhu = Flathub lesen
    .doc = Liest Flathubs Katalog über seine eigene Such-API, eine Seite nach der anderen, und behält jede App neben denen der Registries.
    .w-flathub-s-catalogue-could-not-be = Flathubs Katalog konnte nicht gelesen werden: { $why }

d-ij4f6k4m = Flathub-Angaben lesen
    .doc = Was die Seite einer Flathub-App liest: ihr eigenes AppStream-Dokument und was ihr Build verlangt.
    .w-flathub-did-not-answer-for = Flathub antwortete nicht für { $name }
    .w-flathub-s-answer-for-could-not = Flathubs Antwort für { $name } konnte nicht gelesen werden

d-ij4f6k4m-a = A

d-jb2fqpim = Text aufnehmen
    .doc = Eine App, wie eine Quelle sie schrieb, gelesen und an ihrem Platz behalten: eine App, die dieser Arbeitsbereich schon kannte, behält, was die Person an ihr angesehen hat.

d-jb2fqpim-text = Text

d-fdx4aslc = Repository-Text aufnehmen
    .doc = Ein Repository, wie eine Quelle es schrieb, an seinem Platz behalten.

d-fdx4aslc-text = Text

d-bcruk52u = Repositories aktualisieren
    .doc = Liest die Flatpak-Repositories dieses Computers erneut.

d-ayzvtjki = Repository hinzufügen
    .doc = Fügt diesem Arbeitsbereich ein Flatpak-Repository hinzu.
    .w-a-repository-has-a-name = Ein Repository hat einen Namen
    .w-a-repository-name-holds-at-most = Ein Repository-Name hat höchstens 128 Zeichen
    .w-repositories-must-use-https = Repositories müssen HTTPS verwenden
    .w-this-repository-is-already-here = Dieses Repository ist schon hier

d-ayzvtjki-name = Name

d-ayzvtjki-url = Adresse

d-zebn7nsh = Quelle aktivieren
    .doc = Aktiviert eines der Repositories dieses Arbeitsbereichs.
    .w-this-repository-is-the-computer-s = Dieses Repository gehört zum Computer. Hier lassen sich nur Repositories deines Arbeitsbereichs ändern.

d-zebn7nsh-r = R

d-aipro3dw = Quelle deaktivieren
    .doc = Deaktiviert eines der Repositories dieses Arbeitsbereichs.
    .w-this-repository-is-the-computer-s = Dieses Repository gehört zum Computer. Hier lassen sich nur Repositories deines Arbeitsbereichs ändern.

d-aipro3dw-r = R

d-erliinv3 = Repository-Änderung

d-erliinv3-r = R

d-erliinv3-enabled = Aktiviert

d-3bhdbn3p = Repository entfernen
    .doc = Entfernt eines der Repositories dieses Arbeitsbereichs und seine Apps aus dem Katalog.
    .w-this-repository-is-the-computer-s = Dieses Repository gehört zum Computer. Hier lassen sich nur Repositories deines Arbeitsbereichs ändern.

d-3bhdbn3p-r = R

d-5gcbykzz = Durchsuchen
    .doc = Durchsucht die Apps eines Repositories über die Flatpak-Metadaten dieses Computers.

d-5gcbykzz-r = R

d-6zlctlbs = Holen
    .doc = Holt eine App, die der Arbeitsbereich noch nicht ausführt: die neueste Version, die ihre Quelle anbietet.

d-hpw5iub6 = Aktualisieren
    .doc = Aktualisiert eine App, die der Arbeitsbereich ausführt, auf die neueste Version, die ihre Quelle anbietet.

d-fwuiofga = Die signierte Laufzeit installieren
    .doc = Installiert die Laufzeit, die eine App braucht, sobald ihre Seite das genaue signierte Angebot geprüft hat.
    .w-this-app-names-no-runtime = Diese App nennt keine Laufzeit

d-qitieq3j = Deinstallieren
    .doc = Entfernt eine App aus diesem Arbeitsbereich.
    .w-this-app-belongs-to-the-computer = Diese App gehört zum Computer. Bitte seine Administration, sie zu entfernen.

d-ykz7qcg5 = Diese Version behalten
    .doc = Behält die Version einer App in diesem Arbeitsbereich, sodass sie nie als ungenutzt aufgeräumt wird.
    .w-this-app-has-no-version-to = Diese App hat keine Version zum Behalten

d-pad5jjdg = Aktualisierungen erlauben
    .doc = Lässt zu, dass die ungenutzte Version einer App in diesem Arbeitsbereich wieder aufgeräumt wird.

d-4z4tkll4 = Stoppen
    .doc = Stoppt die Änderung, die für eine App läuft, und behält, was sie bis dahin getan hat.

d-s2mhdfg7 = Öffnen
    .doc = Öffnet eine App, die dieser Computer hat, und lädt sie zuerst, wenn der Arbeitsbereich sie ausführt und dieser Computer sie noch nicht hat.

d-lm6i5ieu = Alle Apps aktualisieren
    .doc = Aktualisiert jede App, die der Arbeitsbereich ausführt und deren Quelle eine neuere Version anbietet.
    .w-every-app-is-up-to-date = Jede App ist aktuell

d-vmb6w54f = Zu diesem Arbeitsbereich hinzufügen…
    .doc = Fügt einen Verzeichnis-Bot als eigene Verknüpfung der Person zu diesem Arbeitsbereich hinzu: sein Programm wird gegen die signierte Liste geprüft und als die Verknüpfung gespeichert, die der Host des Arbeitsbereichs für sie ausführt, unter den Auslösern, die das Programm angibt.
    .w-only-a-directory-bot-is-added = Nur ein Verzeichnis-Bot wird zu einem Arbeitsbereich hinzugefügt

d-vmb6w54f-a = A

d-f32yh5vc = App sperren
    .doc = Sperrt eine App: sie erscheint dann nirgends im Store oder in der Suche und lässt sich von dort weder öffnen noch installieren.

d-f32yh5vc-a = A

d-kpgqwe2u = Entsperren
    .doc = Entsperrt eine App, die diese Person gesperrt hat.
    .w-this-app-is-not-blocked = Diese App ist nicht gesperrt

d-kpgqwe2u-a = A

d-dxyl32te = Paket prüfen
    .doc = Prüft eine Paketdatei, die jemand dem Store gibt.

d-dxyl32te-file = Datei

d-7cabzl2m = Geprüftes Paket installieren
    .doc = Installiert das gerade geprüfte Paket, nachdem die Person seine Angaben gelesen hat.
    .w-review-this-exact-package-before-installing = Prüfe genau dieses Paket, bevor du es installierst

d-viajklb6 = Referenz öffnen
    .doc = Öffnet eine `.flatpakref`: sie kommt als Karte in den Katalog, und die Installation bleibt ein eigener Schritt der Person.

d-viajklb6-file = Datei

d-2nqrkbcq = Link öffnen
    .doc = Öffnet einen `tilo-store:`-Link: die App (`tilo-store:app/<identity>`) oder Laufzeit (`tilo-store:runtime/<identity>`), die er nennt, oder den Katalog.

d-2nqrkbcq-link = Link

d-3fvgbh36 = Die Laufzeit einer App prüfen
    .doc = Liest das genaue signierte Angebot einer Laufzeit, sodass ihre Seite sagt, was ihre Installation ausführen würde.

d-3fvgbh36-runtime = Laufzeit

d-3fvgbh36-registry = Registry

d-nbj74m2v = Meldungsentwurf erstellen

d-nbj74m2v-a = A

d-sbradm4n = Moderationsfall festhalten
    .doc = Hält den Fall der Registry für eine App fest, wie eine Aktualisierung ihn gelesen hat.
    .w-a-case-has-an-id-and = Ein Fall hat eine ID und einen Status

d-sbradm4n-identity = Identität

d-sbradm4n-case = Fall

d-sbradm4n-status = Status

d-4zlwbfzy = Meldung vorbereiten
    .doc = Bereitet den genauen signierten Vorgang vor, den eine Meldung oder ein Einspruch sendet, ohne ihn zu senden.

d-4zlwbfzy-r = R

d-4zlwbfzy-appeal = Einspruch

d-poycyw6m = Meldung absenden
    .doc = Signiert eine Meldung mit der Identität des Arbeitsbereichs und sendet sie an das Repository der App.

d-c3wpc2me = Entscheidung anfechten
    .doc = Ficht eine Entscheidung über eine App an.
    .w-only-a-decision-can-be-appealed = Nur eine Entscheidung lässt sich anfechten

d-pprcmnam = Status aktualisieren
    .doc = Liest den geprüften Status der Registry für den Fall der Meldung erneut.
    .w-this-report-has-no-case-yet = Diese Meldung hat noch keinen Fall

d-pprcmnam-r = R

d-mzdd436j = Quellen lesen
    .doc = Der Katalog: die Apps, die die Quellen anbieten, nach dem gewählten Ort, mit der Seite der App neben der Liste.

d-deg65ibs = Angaben lesen
    .doc = Das Öffnen der Seite einer App liest einmal, was ihre Quelle über sie sagt.

d-wrb37n75 = Aktion ausführen
    .doc = Die Aktion, die die eine Schaltfläche einer Seite ausführt, nach dem Namen, den der Katalog festgelegt hat.

d-wrb37n75-verb = Verb

d-omucqgg7 = Meldungsdatei
    .doc = Der Beleg einer Meldung als Datei, die eine Person speichert und mitsendet.

d-qapap5zc = Aktionsname
    .w-open = Öffnen
    .w-get = Holen
    .w-install = Installieren
    .w-update = Aktualisieren
    .w-install-runtime = Laufzeit installieren
    .w-add-to-this-workspace = Zu diesem Arbeitsbereich hinzufügen…
    .w-install-flatpak-first = Zuerst Flatpak installieren

d-q4sxd6gk = Ablehnung
    .w-waiting-for-an-update-from-the = Wartet auf ein Update vom Entwickler
    .w-not-available-for-this-computer-is = Für diesen Computer nicht verfügbar: { $name } gibt es nur für { $processors }.
    .w-flatpak-unavailable = Flatpak nicht verfügbar
    .w-this-repository-belongs-to-the-computer = Dieses Repository gehört zum Computer. Softwareänderungen brauchen seine Administration.
    .w-no-version-from-this-repository-can = Keine Version aus diesem Repository läuft auf diesem Arbeitsbereich.

d-iavnqqng = Herkunftsworte
    .w-tilo-from = Tilo, aus { $registry }
    .w-flathub = Flathub
    .w-local-package = Lokales Paket

d-n4wf76ry = Vertrauensworte
    .w-publisher-identity-and-package-integrity-are = Identität des Herausgebers und Unversehrtheit des Pakets sind geprüft. Das ist keine unabhängige Qualitätsprüfung.
    .w-tilo-has-not-independently-reviewed-this = Tilo hat diese App nicht unabhängig geprüft.
    .w-this-metadata-comes-from-a-repository = Diese Angaben stammen aus einem Repository, das dein Arbeitsbereich eingerichtet hat. Tilo hat die App nicht unabhängig geprüft.
    .w-verified-by-an-attestor-this-computer = Von einem Attestor geprüft, dem dieser Computer schon vertraut.

d-chuviu52 = Vertrauen kurz
    .w-publisher-verified = Herausgeber geprüft
    .w-flathub = Flathub
    .w-configured-repository = Eingerichtetes Repository
    .w-verified-locally = Lokal geprüft

d-yijdkrzm = Geräteworte
    .w-runs-in-the-workspace-it-is = Läuft in dem Arbeitsbereich, dem sie hinzugefügt wird
    .w-downloaded-not-running = Geladen, läuft nicht
    .w-not-running-in-this-workspace = Läuft nicht in diesem Arbeitsbereich
    .w-kept-version = Behalten: Version { $installed }
    .w-version-may-be-cleaned-up-when = Version { $installed }, wird bei Nichtnutzung eventuell aufgeräumt

d-5ncgpk2g = Größenworte
    .w-gb = { $rounded } GB
    .w-mb = { $rounded } MB
    .w-kb = { $rounded } KB

d-zwhg2wf4 = Berechtigungsworte
    .w-uses-the-internet = Nutzt das Internet
    .w-opens-and-saves-files-you-choose = Öffnet und speichert Dateien, die du auswählst
    .w-reads-and-changes-all-your-files = Liest und ändert alle deine Dateien
    .w-reads-and-changes-your-home-folder = Liest und ändert deinen Persönlichen Ordner
    .w-uses-the-camera = Nutzt die Kamera
    .w-uses-the-microphone = Nutzt das Mikrofon
    .w-captures-your-screen = Nimmt deinen Bildschirm auf
    .w-uses-your-location = Nutzt deinen Standort
    .w-uses-all-devices-such-as-cameras = Nutzt alle Geräte, etwa Kameras und Gamecontroller
    .w-uses-bluetooth = Nutzt Bluetooth
    .w-talks-to-system-services = Spricht mit Systemdiensten
    .w-keeps-secrets-in-your-keyring = Bewahrt Geheimnisse in deinem Schlüsselbund
    .w-uses-your-ssh-or-gpg-keys = Nutzt deine SSH- oder GPG-Schlüssel
    .w-prints = Druckt
    .w-reads-and-changes-the-clipboard = Liest und ändert die Zwischenablage
    .w-shows-notifications = Zeigt Mitteilungen
    .w-runs-programs = Führt Programme aus
    .w-can-run-programs-outside-its-sandbox = Kann Programme außerhalb seiner Sandbox ausführen
    .w-opens-links-in-other-apps = Öffnet Links in anderen Apps

d-yvywzeqz = Bot-Auslöserworte
    .w-when-its-connector-reports-an-event = Wenn sein Konnektor ein Ereignis meldet
    .w-when-a-resource-changes = Wenn sich eine Ressource ändert
    .w-on-a-schedule = Nach Zeitplan
    .w-when-someone-runs-it = Wenn jemand ihn ausführt
    .w-when-a-message-arrives = Wenn eine Nachricht ankommt

d-g4gc5vkw = Herkunft kurz
    .w-bot = Bot
    .w-tilo = Tilo
    .w-flathub = Flathub
    .w-local-package = Lokales Paket

d-r6lfeprr = Kategorieworte
    .w-office = Büro
    .w-development = Entwicklung
    .w-graphics = Grafik
    .w-sound-video = Ton und Video
    .w-network = Netzwerk
    .w-education = Bildung
    .w-science = Wissenschaft
    .w-games = Spiele
    .w-system = System
    .w-utilities = Dienstprogramme

d-h6isdwur = Repository hinzufügen~present

d-3zppaqhs = Repository aktivieren~present

d-ap2xqqbg = Repository deaktivieren~present

d-d4bnh64q = Repository entfernen~present

d-qa3erebl = Holen~present

d-frmgpell = Aktualisieren~present

d-j4iwpnpf = Laufzeit installieren~present

d-l6ibqb6r = Entfernen~present

d-zbdh5lax = Festhalten~present

d-a6h37upx = Alle aktualisieren~present

d-mbmo3meu = Bot hinzufügen~present

d-s6refga2 = App entsperren~present

d-ou24sxcw = Geprüftes installieren~present

d-i4b3rb4w = Moderationsfall festhalten~present

d-cm5whz4i = Einspruch~present

d-bixeamzz = Status aktualisieren~present

d-xo5mha7p = Katalog
    .w-search = Suche
    .w-all-apps = Alle Apps
    .w-app-store = App Store
    .w-refresh-apps = Apps aktualisieren
    .w-update-all-apps = Alle Apps aktualisieren
    .w-review-package = Paket prüfen
    .w-flatpak-repositories = Flatpak-Repositories
    .w-trust-and-moderation = Vertrauen und Moderation
    .w-back-to-apps = Zurück zu den Apps
    .w-package = Paket
    .w-categories = Kategorien
    .w-reading-the-sources = Quellen werden gelesen
    .w-could-not-read-the-sources = Quellen konnten nicht gelesen werden
    .w-no-apps-found = Keine Apps gefunden
    .w-try-a-different-search-or-category = Versuche eine andere Suche oder Kategorie.
    .w-flathub-could-not-be-read = Flathub konnte nicht gelesen werden
    .w-could-not-carry-that-out = Das ließ sich nicht ausführen

d-nncj3afc = Entdecken
    .w-no-apps-available = Keine Apps verfügbar
    .w-add-or-enable-a-repository-you = Füge ein Repository hinzu, dem du vertraust, oder aktiviere eines.
    .w-featured = Empfohlen
    .w-recently-updated = Kürzlich aktualisiert
    .w-all-apps = Alle Apps

d-alb5chrj = App-Karte

d-ck3o6mji = App-Liste
    .w-show-more = Mehr anzeigen

d-pmrqekaa = App-Zeile
    .w-working = Arbeitet…
    .w-could-not-finish = Konnte nicht beendet werden

d-vakqk5y4 = App-Seite
    .w-allow-updates = Aktualisierungen erlauben
    .w-keep-this-version = Diese Version behalten
    .w-uninstall = Deinstallieren
    .w-stop = Stoppen
    .w-unblock = Entsperren
    .w-block-app = App sperren
    .w-technical-details = Technische Angaben
    .w-cannot-change-this-app = Diese App lässt sich nicht ändern
    .w-working = Arbeitet…
    .w-package-verified = Paket geprüft
    .w-review-its-details-before-installing-it = Lies seine Angaben, bevor du es installierst.
    .w-install-reviewed-package = Geprüftes Paket installieren
    .w-latest-version = Neueste: Version { $available }
    .w-waiting-for-an-update-from-the = Wartet auf ein Update vom Entwickler
    .w-only-built-for = Nur gebaut für { $processors }
    .w-about-this-app = Über diese App
    .w-screenshots = Bildschirmfotos
    .w-screenshot-of = Bildschirmfoto von { $name }
    .w-what-s-new = Was ist neu
    .w-version = Version { $version }
    .w-trust = Vertrauen
    .w-could-not-finish = Konnte nicht beendet werden

d-gxuht36a = Berechtigungsbereich
    .w-what-it-can-do = Was sie tun kann
    .w-runs-in-tilo-s-sandbox-and = Läuft in Tilos Sandbox und fragt, bevor sie deine Dateien, Kamera oder dein Mikrofon nutzt.
    .w-its-source-has-not-said-what = Ihre Quelle hat nicht gesagt, was diese App tun kann.

d-wizqobn5 = Faktenbereich
    .w-facts = Fakten
    .w-source = Quelle
    .w-version = Version
    .w-download-size = Downloadgröße
    .w-installed-size = Installierte Größe
    .w-developer = Entwickler
    .w-licence = Lizenz
    .w-updated = Aktualisiert
    .w-categories = Kategorien
    .w-supported-languages = Unterstützte Sprachen
    .w-app-id = App-ID
    .w-publisher-key = Herausgeberschlüssel
    .w-package-digest = Paketprüfsumme
    .w-required-runtime = Benötigte Laufzeit
    .w-open-website = Webseite öffnen
    .w-open-source-code = Quelltext öffnen

d-6hlkpuof = Bot-Bereich
    .w-what-it-does = Was er tut
    .w-what-it-can-reach = Was er erreichen kann
    .w-nothing-beyond-the-workspace-it-runs = Nichts außerhalb des Arbeitsbereichs, in dem er läuft.
    .w-uses-s-actions-and-watches-its = Nutzt die Aktionen von { $other } und beobachtet seine Änderungen
    .w-connector-version-or-newer = Konnektor, Version { $connector } oder neuer
    .w-may-post-under-other-names-and = Kann unter anderen Namen und Bildern schreiben. Immer als Bot gekennzeichnet.

d-amq6xvh5 = Meldungsbereich
    .w-report-a-problem = Ein Problem melden
    .w-reports-are-signed-with-your-workspace = Meldungen werden mit der Identität deines Arbeitsbereichs signiert und bleiben Entwürfe, die dein Arbeitsbereich teilt, bis du sie absendest.
    .w-create-report-draft = Meldungsentwurf erstellen

d-kcllfezw = Meldungsformular
    .w-case = Fall { $case }
    .w-describe-what-happened = Beschreibe, was passiert ist
    .w-submit-report = Meldung absenden
    .w-refresh-status = Status aktualisieren
    .w-appeal-decision = Entscheidung anfechten
    .w-export-evidence = Beleg exportieren

d-3trohjzy = Meldungsseite
    .w-report-for = Meldung für { $identity }
    .w-show-the-app = Die App zeigen
    .w-could-not-carry-that-out = Das ließ sich nicht ausführen

d-jyxy3v7p = Repository-Seite
    .w-browse-software-from-your-configured-repositories = Durchstöbere Software aus deinen eingerichteten Repositories. Repositories dieses Computers bleiben verfügbar; Repositories, die du hinzufügst, gehören zu deinem Arbeitsbereich.
    .w-refresh-repositories = Repositories aktualisieren
    .w-flatpak-unavailable = Flatpak nicht verfügbar
    .w-configured-repositories = Eingerichtete Repositories
    .w-no-repositories-configured = Keine Repositories eingerichtet
    .w-add-a-repository = Ein Repository hinzufügen
    .w-name = Name
    .w-repository-name = Repository-Name
    .w-repository-url = Repository-Adresse
    .w-repositories-must-use-https = Repositories müssen HTTPS verwenden.
    .w-https = https://…
    .w-add-repository = Repository hinzufügen
    .w-could-not-carry-that-out = Das ließ sich nicht ausführen

d-5pnjajg7 = Repository-Zeile
    .w-this-computer = Dieser Computer
    .w-this-workspace = Dieser Arbeitsbereich
    .w-disable-source = Quelle deaktivieren
    .w-enable-source = Quelle aktivieren
    .w-remove-repository = Repository entfernen
    .w-browse-apps = Apps durchsuchen

d-6u3w4u44 = Repository-Ansicht
    .w-could-not-browse-repository = Repository konnte nicht durchsucht werden
    .w-no-apps-loaded-from-this-repository = Keine Apps aus diesem Repository geladen
    .w-browse-apps-to-read-what-it = Durchsuche die Apps, um zu lesen, was es anbietet.
    .w-from = { $parts_words } aus { $title }
    .w-disable-source = Quelle deaktivieren
    .w-enable-source = Quelle aktivieren
    .w-browse-apps = Apps durchsuchen

d-bzwxksey = Vertrauensseite
    .w-how-reports-work = Wie Meldungen funktionieren
    .w-reports-are-signed-with-your-workspace = Meldungen werden mit der Identität deines Arbeitsbereichs signiert und bleiben Entwürfe, die dein Arbeitsbereich teilt, bis du sie absendest. Ein neuer Status erscheint erst, wenn er geprüft ist; eine Anfrage ohne Verbindung oder eine abgelehnte bleibt ein Entwurf, den du erneut senden kannst.
    .w-what-a-signature-means = Was eine Signatur bedeutet
    .w-a-verified-package-comes-from-its = Ein geprüftes Paket kommt von seinem Herausgeber und wurde nicht verändert. Das macht eine App nicht nützlich, privat oder ehrlich, und der Store erfindet nie ein Prüfsiegel.
    .w-publishing-guidelines = Veröffentlichungsrichtlinien
    .w-listings-must-describe-what-the-app = Einträge müssen beschreiben, was die App tut, bezahlte oder Online-Dienste nennen, von denen sie abhängt, nur die Berechtigungen verlangen, die sie braucht, die Daten der Menschen über Aktualisierungen hinweg behalten, mit assistiver Technik arbeiten und niemals andere nachahmen oder Menschen ohne Hinweis verfolgen.
    .w-reports = Meldungen
    .w-no-report-drafts = Keine Meldungsentwürfe
    .w-open-an-app-and-choose-create = Öffne eine App und wähle Meldungsentwurf erstellen.

d-wtyvo4dc = Meldungszeile
    .w-report-for = Meldung für { $identity }
    .w-no-concern-described-yet = Noch kein Anliegen beschrieben

d-qbd5uogv = Fehlgeschlagen
    .doc = Warum die letzte Änderung an ihr auf diesem Computer fehlgeschlagen ist; leer, bis eine es tut, und sobald eine Änderung endet.

d-y63chpxh = Flathub-Problem
    .doc = Warum Flathubs Katalog nicht gelesen werden konnte, wenn er es nicht konnte.

d-6udr6hps = Befehlsproblem
    .doc = Warum ein Befehl, der keine Änderung einer App war, nicht ausgeführt werden konnte: ein Repository hinzufügen, ein Paket prüfen, eine Meldung aktualisieren.

d-enms3not = Fehlende Quellen
    .w-did-not-answer-apps-it-lists = { $name } hat nicht geantwortet. Die Apps, die es listet, fehlen, bis es das tut.
    .w-the-app-catalogue-from-has-expired = Der App-Katalog von { $name } ist abgelaufen. Apps, die du hast, funktionieren weiter.
