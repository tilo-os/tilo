d-so-tilo-browser = Browser

d-lrkgwd2c = Lesezeichen
    .doc = Eine Seite, die jemand gespeichert hat: wo sie ist und wie sie heißt.
    .noun = { $count ->
        [one] Lesezeichen
       *[other] Lesezeichen
    }

d-44buv5tt = Adresse

d-b3zhsydm = Titel

d-ehmpzm3r = Ordner
    .doc = Der Ordner, in dem das Lesezeichen steht, als Pfad in der eigenen Bibliothek (`Arbeit/Lesen`): leer für eines ganz oben.

d-em2t5s5a = Besuch
    .doc = Eine Seite, die die Person besucht hat: wo sie ist, wie sie hieß, als sie zuletzt gelesen wurde, und wann das war.
    .noun = { $count ->
        [one] Besuch
       *[other] Besuche
    }

d-k73mxvmp = Adresse

d-qy6vtgjc = Titel

d-2yoxl6hx = Zuletzt
    .doc = Wann die Person sie zuletzt geöffnet hat, nach ihrer Uhr.

d-eruwucur = Anzahl
    .doc = Wie oft ein Fenster sie geöffnet hat.

d-siihm6ib = Übernommener Besuch
    .doc = Eine Seite aus dem Verlauf eines anderen Browsers, neben den Besuchen dieses Browsers geführt und getrennt gezählt, sodass derselbe Verlauf zweimal importiert eine Seite ergibt.
    .noun = { $count ->
        [one] übernommener Besuch
       *[other] übernommene Besuche
    }

d-76xowg2w = Vorschlag
    .doc = Eine Zeile, die die Adressleiste für das Getippte anbietet: eine Seite, die die Person gespeichert oder geöffnet hat, oder die Suche nach den Wörtern.
    .noun = { $count ->
        [one] Vorschlag
       *[other] Vorschläge
    }

d-zl6ois7x = Art

d-76xowg2w-filter-kind = Art

d-iwoez6ar = Titel

d-np2bxhf4 = Adresse
    .doc = Was die Wahl öffnet: die Seite oder die Antwort der Suchmaschine auf die Wörter.

d-dermimxj = Unterzeile
    .doc = Was die Zeile unter ihrem Titel sagt: die Adresse der Seite oder die Suchmaschine, die gefragt wird.

d-4cfgjljd = Vorschlagsart
    .doc = Woher eine Zeile kommt, die die Adressleiste anbietet.

d-o2azokbu = Lesezeichen
    .doc = Eine Seite, die die Person gespeichert hat.

d-7kmprotk = Besucht
    .doc = Eine Seite, die dieser Browser geöffnet hat.

d-o5i6hm3g = Übernommen
    .doc = Eine Seite, die der Verlauf eines anderen Browsers mitgebracht hat.

d-vb6ajdgd = Suche
    .doc = Die Suchmaschine, gefragt nach dem Getippten.

d-my4dhx2s = Adresse

d-43vnrbh6 = Titel

d-avuwnnrf = Zuletzt

d-srpuiw6c = Anzahl

d-hkdk5jeo = Gemerktes Formular
    .doc = Ein gemerktes Formular einer Seite: was es fragt und wie es jede Frage nennt, nie eine Antwort.
    .noun = { $count ->
        [one] gemerktes Formular
       *[other] gemerkte Formulare
    }

d-3wdvc5zt = Ursprung
    .doc = Der Ursprung der Seite (`https://example.org`), wie ihn auch der Trackingschutz kennt.

d-ksw3jnev = Signatur
    .doc = Die Gestalt des Formulars; sie benennt dasselbe Formular bei einem späteren Besuch und benennt es nicht mehr, sobald das Formular etwas anderes fragt.

d-vdjubgjc = Bezeichnung
    .doc = Wie das Formular heißt: seine eigene Legende oder die Seite, auf der es steht.

d-6atmgq6f = Seiten
    .doc = Die Seiten der Website, auf denen das Formular gelesen wurde.

d-znuzitns = Gesehen
    .doc = Wie oft das Formular gelesen wurde.

d-kexnvpi2 = Gelesen am
    .doc = Wann es zuletzt gelesen wurde, nach der Uhr der Person.

d-jalszv2p = Fragen

d-oizi2wqk = Frage
    .doc = Eine Frage eines gemerkten Formulars: wie sie heißt, wonach sie fragt und welche Antworten sie anbietet.
    .noun = { $count ->
        [one] Frage
       *[other] Fragen
    }

d-2wlmb5rp = Name

d-z4pmfk6p = Antwort
    .doc = Welche Art Antwort sie annimmt: `text`, `email`, `tel`, `number`, `url`, `password`, `choice`, `choices`, `file`, `date` oder `checkbox`.

d-mttccz7q = Bezeichnung
    .doc = Was die Frage fragt, wie die Seite sie beschriftet.

d-yk2bhzup = Auswahlmöglichkeiten

d-gmhi7gfy = Erforderlich

d-oizi2wqk-filter-required = Erforderlich

d-b7ln6koa = Adresse
    .doc = Eine Adresse, mit der die Person Formulare ausfüllt, angeboten, wo ein Formular eine verlangt.
    .noun = { $count ->
        [one] Adresse
       *[other] Adressen
    }

d-fnyioh2y = Bezeichnung

d-3ewzn2z4 = Vollständiger Name

d-n2u44pv5 = Vorname

d-lgputt7y = Nachname

d-plkha23f = Straße

d-pwnlxkv7 = Stadt

d-g7xpip4v = Region

d-kj2h5fbf = Postleitzahl

d-iciyfupp = Land

d-l5vuna4b = Seite
    .doc = Eine in einem Fenster offene Seite: ihre Adresse, wie sie heißt und was in ihr getan wurde, das Neueste zuletzt.
    .noun = { $count ->
        [one] Seite
       *[other] Seiten
    }

d-hf6q2oo4 = Adresse

d-tyhunk32 = Titel

d-4nlkcuks = Pausiert
    .doc = Der Verlauf ist im Fenster dieser Seite pausiert: dort geöffnete Seiten werden nicht zum Verlauf der Person hinzugefügt.

d-dedg3fev = Schritte
    .doc = Was in der Seite getan wurde, das Neueste zuletzt.

d-cjwvjr5r = Suchtext
    .doc = Die Wörter, die die Suche des Fensters zuletzt fand: wonach die Weitersuche zum nächsten und zurück zum vorigen Treffer die Seite fragt.

d-j257iaef = Seitenschritt
    .doc = Eine Sache, die in einer Seite getan wurde: wer gefragt hat, was getan wurde, worauf es wirkte und wie es endete.
    .noun = { $count ->
        [one] Seitenschritt
       *[other] Seitenschritte
    }

d-cpjp36fw = Zeitpunkt
    .doc = Wann es getan wurde, in Millisekunden seit dem Öffnen der Seite.

d-4mcq24cu = Akteur
    .doc = Wer gefragt hat: eine Person, ein Agent, ein Skript.

d-g4lc2ulh = Gerät
    .doc = Das Gerät, von dem aus der Akteur gearbeitet hat: leer für dieses.

d-ouuwq4an = Gerätename

d-7qikcw5o = Vorgang
    .doc = Was getan wurde, wie die Aktionen der Seite es nennen.

d-olgrtknj = Ok

d-2mfaldtw = Code

d-grbghvzm = Meldung

d-vlqrueqe = Adresse
    .doc = Die Adresse, auf die es wirkte, und die, zu der es ging, für einen Schritt, der navigiert hat.

d-x3xrjcbr = Zieladresse

d-ta3jd45o = Rolle
    .doc = Das Element, auf das es wirkte, nach seiner Rolle und seinem Namen.

d-kmz5bb2s = Element

d-g67lnb6k = Text
    .doc = Der Text, den es schrieb, oder die Auswahl, die es traf.

d-ws5snl25 = Werte

d-j257iaef-filter-at = Zeitpunkt

d-j257iaef-filter-actor = Akteur

d-j257iaef-filter-ok = Ok

d-hh26i3w2 = Browser
    .doc = Der Browser des Arbeitsbereichs: was die Person eingestellt hat, die Seiten, die sie gespeichert und besucht hat, die Formulare, die ihre Seiten gefragt haben, und die Adressen, mit denen sie Formulare ausfüllt.
    .noun = { $count ->
        [one] Browser
       *[other] Browser
    }

d-v73p6lyt = Werbeblocker
    .doc = Das Blockieren von Trackern und Werbung ist für jede Seite an außer den unten genannten.

d-4mcclqi3 = Beim Beenden löschen
    .doc = Cookies, Website-Speicher und gemerkte Formulare werden gelöscht, wenn das letzte Fenster schließt.

d-zhpmqsvs = Suchmaschine
    .doc = Die Suchmaschine, die die Adressleiste fragt, wenn das Getippte keine Adresse ist: die Adresse, deren `%s` die Wörter ersetzen.

d-ezeliuhm = Startseite
    .doc = Die Seite, bei der ein Fenster zuerst öffnet.

d-54pfa4xp = Problem
    .doc = Warum der letzte Import nicht übernommen werden konnte, in den Worten der Datei: leer, solange es nichts zu sagen gibt.

d-b7avacvg = Seiten ohne Schutz
    .doc = Die Seiten, für die der Trackingschutz aus ist, nach genauem Ursprung.

d-5kcrev3j = Lesezeichen

d-qfwsjkyv = Besuche

d-dlo6rb3l = Übernommene Besuche

d-m7eeditk = Formulare

d-2zm7y6e5 = Adressen

d-zx4a3bhv = Seiten

d-z3z5vks2 = Adressen aktiviert
    .doc = Formulare werden mit einer gespeicherten Adresse ausgefüllt, solange die Person eine hat und dies anlässt.

d-hh26i3w2-filter-adblock = Werbeblocker

d-hh26i3w2-filter-clear_on_exit = Clear on exit

d-hh26i3w2-filter-sites_without_protection = Sites without protection

d-hh26i3w2-filter-addresses_enabled = Addresses enabled

d-l4fklfdf = Neue Seite
    .doc = Öffnet eine Seite in einem eigenen Fenster.

d-s6ryunn5 = Privat
    .doc = Das Fenster der Seite browst privat: sein Motor läuft auf einem eigenen Profil, das nichts enthält, womit die Person angemeldet ist, ihr keine gespeicherten Anmeldungen anbietet und mit dem Fenster verschwindet.

d-vzmpfm6h = Neues privates Fenster
    .doc = Öffnet ein Fenster, das privat browst: nichts, was es liest, womit es sich anmeldet oder was es herunterlädt, kommt zu dem, was dieses Gerät über die Person behält.

d-ybc7uykj = Adresse öffnen
    .doc = Öffnet eine Adresse in einer eigenen Seite oder fragt die Suchmaschine nach dem Getippten, wenn es keine Adresse ist.
    .w-type-an-address-or-what-to = Tippe eine Adresse oder wonach du suchen willst

d-ybc7uykj-typed = Eingabe

d-nwpvyebd = Seite als Lesezeichen
    .doc = Speichert eine Seite für später.
    .w-a-bookmark-is-an-absolute-http = Ein Lesezeichen ist eine absolute http(s)-Adresse
    .w-that-page-is-bookmarked-already = Diese Seite ist bereits als Lesezeichen gespeichert

d-nwpvyebd-url = Adresse

d-nwpvyebd-title = Titel

d-4v7rayfx = Lesezeichen entfernen
    .doc = Nimmt ein Lesezeichen zurück.

d-ogasu5l3 = Trackingschutz für eine Seite
    .doc = Schaltet den Trackingschutz für eine Seite aus oder wieder ein.
    .w-a-site-is-named-by-one = Eine Seite wird durch einen http(s)-Ursprung benannt

d-ogasu5l3-origin = Ursprung

d-ogasu5l3-enabled = Aktiviert

d-mnlzylp6 = Diese Seite schützen
    .doc = Schaltet den Trackingschutz für diese Seite aus oder wieder ein, wie es das Schild der Seite tut.

d-mnlzylp6-origin = Ursprung

d-w6wanhhk = Dieses Formular vergessen
    .doc = Vergisst ein Formular, das eine Seite gefragt hat.

d-2euwjvpy = Formulare dieser Seite vergessen
    .doc = Vergisst jedes Formular, das eine Seite gefragt hat.
    .w-a-site-is-named-by-one = Eine Seite wird durch einen http(s)-Ursprung benannt

d-2euwjvpy-origin = Ursprung

d-wnoovbkq = Alle gemerkten Formulare vergessen
    .doc = Vergisst jedes Formular, das irgendeine Seite gefragt hat.

d-nvkil5qo = Formular merken
    .doc = Merkt, was die Formulare einer Seite fragen, wie das Fenster sie liest: die Gestalt jedes Formulars und seiner Fragen, nie was ein Feld enthielt.
    .w-a-form-is-remembered-of-one = Ein Formular wird für einen http(s)-Ursprung gemerkt
    .w-a-form-is-named-by-its = Ein Formular wird durch seine Gestalt benannt

d-nvkil5qo-origin = Ursprung

d-nvkil5qo-signature = Signatur

d-nvkil5qo-label = Bezeichnung

d-nvkil5qo-page = Seite

d-nvkil5qo-questions = Fragen

d-nvkil5qo-whole = Ganz

d-xhjamynn = Adresse hinzufügen
    .doc = Fügt eine Adresse für die Formulare hinzu, die die Person ausfüllt.

d-wcffcb7a = Adresse löschen
    .doc = Löscht eine Adresse und füllt Formulare nicht mehr mit ihr aus.

d-luyziriq = Diesen Besuch benennen
    .doc = Benennt einen Besuch, wenn der Person ihr eigener Name für eine Seite besser gefällt als ihr Titel.
    .w-a-visit-is-called-by-some = Ein Besuch wird mit irgendeinem Namen benannt

d-luyziriq-title = Titel

d-zzfcoljy = Diese Seite vergessen
    .doc = Vergisst einen Besuch.

d-tfldgzbg = Verlauf löschen
    .doc = Löscht den Browserverlauf.

d-2dmo7lzf = Besuch
    .doc = Fügt eine Seite, die ein Fenster geöffnet hat, zum Verlauf der Person hinzu und zählt sie, wenn sie schon dort ist.

d-2dmo7lzf-url = Adresse

d-2dmo7lzf-title = Titel

d-2dmo7lzf-at = Zeitpunkt

d-q5brlj4t = Angekommen
    .doc = Nimmt eine Seite auf, bei der ein Fenster angekommen ist: sie kommt in den Verlauf der Person.

d-q5brlj4t-url = Adresse

d-32ky3z5c = Verlauf pausieren
    .doc = Pausiert oder setzt den Verlauf der Person im Fenster einer Seite fort.

d-32ky3z5c-paused = Pausiert

d-xuyixcsy = Browserdaten importieren
    .doc = Nimmt die Browserdaten auf, die ein anderer Browser hält, aus dem Paket, das der eigene Import dieser App und `tilo migrate` übergeben: seinen Verlauf, seine Lesezeichen oder beides.

d-xuyixcsy-file = Datei

d-ix3ng2as = Browserverlauf importieren
    .doc = Importiert die Seiten aus dem Verlauf eines anderen Browsers aus dem Paket, das der eigene Import dieser App und `tilo migrate` übergeben: ein JSON-Text von `{"{"}"browser": "...", "visits": [{"{"}"url": "...", "title": "...", "last": 1760000000, "count": 1{"}"}]{"}"}`.

d-ix3ng2as-bundle = Paket

d-y3y3upgg = Lesezeichen importieren
    .doc = Importiert die Lesezeichen eines anderen Browsers aus dem Paket, das der eigene Import dieser App übergibt: das `ImportBundle`-JSON mit Lesezeichen-Teilen (`{"{"}"bookmarks": [{"{"}"url": "...", "title": "...", "folder": "Work"{"}"}]{"}"}`).

d-y3y3upgg-bundle = Paket

d-t3d7dnlx = Öffnen

d-sayctunu = Als Lesezeichen speichern

d-qrtmsrs3 = Trackingschutz ändern

d-4j4jisai = Formulare der Seite vergessen

d-t5atc53e = Formular merken

d-y7tjvycu = Besuch benennen

d-r7exe3by = Seitenfenster
    .w-browser = Browser
    .w-no-page-is-open = Keine Seite offen
    .w-open-a-page-to-browse-its = Öffne eine Seite zum Browsen: ihre Adresse oder wonach du suchst.

d-vsaswu4a = Seitenansicht
    .w-pause-history = Verlauf pausieren
    .w-bookmark = Lesezeichen
    .w-this-page-could-not-be-shown = Diese Seite konnte nicht gezeigt werden
    .w-what-was-done = Was getan wurde
    .w-back = Zurück
    .w-forward = Vorwärts
    .w-reload = Neu laden
    .w-address-and-search = Adresse und Suche
    .w-questions = Fragen
    .w-page = Seite
    .w-history = Verlauf
    .w-bookmarks = Lesezeichen
    .w-print = Drucken
    .w-browser-settings = Browsereinstellungen
    .w-reader-view = Leseansicht
    .w-zoom-in = Vergrößern
    .w-zoom-out = Verkleinern
    .w-turn-tracker-protection-on-for-this = Trackingschutz für diese Seite einschalten
    .w-turn-tracker-protection-off-for-this = Trackingschutz für diese Seite ausschalten
    .w-forget-this-site-s-forms = Formulare dieser Seite vergessen
    .w-suggestions = Vorschläge
    .w-of = { $find_at } von { $find_count }
    .w-previous-match = Vorheriger Treffer
    .w-next-match = Nächster Treffer
    .w-clear-the-find = Suche beenden
    .w-picture-in-picture = Bild im Bild
    .w-return-from-picture-in-picture = Zurück aus Bild im Bild
    .w-this-page-asks = Diese Seite fragt
    .w-ok = In Ordnung
    .w-cancel = Abbrechen
    .w-actual-size = Tatsächliche Größe
    .w-actual-size-2 = Tatsächliche Größe ({ $percent }%)
    .w-downloads = Downloads
    .w-cancel-every-download = Alle Downloads abbrechen
    .w-unmute-the-page = Seite wieder hörbar
    .w-mute-the-page = Seite stummschalten
    .w-earlier-pages = Frühere Seiten
    .w-later-pages = Spätere Seiten
    .w-site-information = Seiteninformationen
    .w-requests-blocked = Blockierte Anfragen
    .w-certificate = Zertifikat
    .w-clear-this-site-s-data = Daten dieser Website löschen
    .w-block-what-this-site-asks-for = Blockieren, wonach diese Website fragt
    .w-let-the-site-ask-again = Website darf erneut fragen
    .w-developer-tools = Entwicklerwerkzeuge
    .w-printing = Drucken
    .w-logins = Anmeldungen
    .w-find-this-page-s-logins = Anmeldungen dieser Seite finden
    .w-saved-logins = Gespeicherte Anmeldungen
    .w-save-the-login = Anmeldung speichern
    .w-logins-on-this-page = Anmeldungen auf dieser Seite
    .w-save-this-login = Diese Anmeldung speichern
    .w-update-the-saved-login = Gespeicherte Anmeldung aktualisieren
    .w-saved-by-browser-in-your-vault = Von Browser in deinem Tresor gespeichert
    .w-in-your-vault-fills-once-you = In deinem Tresor: wird eingesetzt, sobald du es erlaubst
    .w-fill = Einsetzen
    .w-delete = Löschen
    .w-new-page = Neue Seite
    .w-new-private-window = Neues privates Fenster
    .w-extensions = Erweiterungen
    .w-read-the-installed-extensions = Installierte Erweiterungen lesen
    .w-on = An ({ $version })
    .w-off = Aus ({ $version })
    .w-may-use = Darf { $permissions } nutzen
    .w-turn-the-extension-off = Erweiterung ausschalten
    .w-turn-the-extension-on = Erweiterung einschalten
    .w-remove = Entfernen
    .w-install-from-a-file = Aus einer Datei installieren
    .w-link-to-a-chrome-web-store = Link zu einer Chrome-Web-Store-Erweiterung
    .w-add-from-the-store = Aus dem Store hinzufügen
    .w-check-for-updates = Auf Aktualisierungen prüfen
    .w-add-to-browser = Zum Browser hinzufügen
    .w-notification = Mitteilung
    .w-dismiss = Wegnehmen
    .w-private-window = Privates Fenster
    .w-this-window-keeps-nothing-when-it = Dieses Fenster behält nichts, wenn es schließt: kein Verlauf, keine Anmeldungen und keine Cookies außer denen der Seite.

d-pu2zdrbu = Schrittzeile

d-uppzh7ic = Lesezeichenseite
    .w-address = Adresse
    .w-folder = Ordner
    .w-remove-bookmark = Lesezeichen entfernen
    .w-details = Details
    .w-name = Name
    .w-top-of-the-bookmarks = Oberste Ebene der Lesezeichen

d-y2shuesh = Besuchsseite
    .w-address = Adresse
    .w-last-opened = Zuletzt geöffnet
    .w-opened = Geöffnet
    .w-forget-this-page = Diese Seite vergessen

d-jlg4rve3 = Bibliothek
    .w-bookmarks = Lesezeichen
    .w-history = Verlauf

d-3dld2nsk = Lesezeichenliste
    .w-no-bookmarks-yet = Noch keine Lesezeichen
    .w-top-of-the-bookmarks = Oberste Ebene der Lesezeichen

d-2ow3mpb2 = Besuchszeile

d-okndzb5z = Importierte Zeile

d-qgyn4224 = Lesezeichenzeile

d-b7sixoyk = Adresszeile

d-sij6peeq = Formularzeile
    .w-read-times = { $origin } · { $seen } Mal gelesen
    .w-forget-this-form = Dieses Formular vergessen
    .w-required = Erforderlich

d-ok2yuain = Browsereinstellungen
    .w-browser-settings = Browsereinstellungen
    .w-something-could-not-be-taken-in = Etwas konnte nicht übernommen werden
    .w-browsing = Browsen
    .w-block-advertisements-and-trackers = Werbung und Tracker blockieren
    .w-clear-cookies-site-storage-and-remembered = Cookies, Website-Speicher und gemerkte Formulare beim Schließen des letzten Fensters löschen
    .w-search-engine = Suchmaschine
    .w-https-duckduckgo-com-q-s = https://duckduckgo.com/?q=%s
    .w-home-page = Startseite
    .w-about-blank = about:blank
    .w-addresses = Adressen
    .w-fill-forms-with-a-saved-address = Formulare mit einer gespeicherten Adresse ausfüllen
    .w-add-an-address = Adresse hinzufügen
    .w-sites-without-tracker-protection = Seiten ohne Trackingschutz
    .w-every-site-is-protected = Jede Seite ist geschützt.
    .w-remembered-forms = Gemerkte Formulare
    .w-no-site-s-forms-were-read = Noch keine Formulare einer Seite gelesen.
    .w-forget-all-remembered-forms = Alle gemerkten Formulare vergessen
    .w-protect-this-site-again = Diese Seite wieder schützen

d-36vwukxk = Verlaufsliste
    .w-no-pages-visited-yet = Noch keine Seiten besucht
    .w-from-another-browser = Aus einem anderen Browser

d-so-tilo-browser-keywords = web, internet, surfen, website, url, www, webbrowser, chrome, safari, edge, firefox, browser, browsen, webseite, internetseite

d-mk755sf3 = Dialog
    .doc = Worauf die Seite wartet, wie ihr Fenster es zuletzt gelesen hat: eine Frage, die jemand beantwortet, und das Menü, das sie zeigte.

d-tveuvbse = Menü

d-eiqyiw53 = Lädt
    .doc = Wie weit die Seite geladen ist, worauf sie gezoomt ist und wo die Suchleiste steht.

d-xu2jvlwq = Fortschritt

d-5gspmng7 = Zoomstufe

d-j5emshom = Fundstelle

d-n6y73q7c = Fundzahl

d-o3koqzgj = Dialogfrage
    .doc = Eine Frage, die das Skript der Seite selbst stellt: ein Hinweis, eine Bestätigung, eine Eingabe oder ein Seitenwechsel.
    .noun = { $count ->
        [one] Dialogfrage
       *[other] Dialogfragen
    }

d-trchguwk = Anfrage
    .doc = Die Kennung der Frage beim Engine, die die Antwort nennt.

d-plkldhkl = Art
    .doc = `alert`, `confirm`, `prompt` oder `leave`.

d-ruephqmr = Herkunft

d-asepg6kx = Meldung

d-4v2f3ua3 = Vorgabetext

d-o3koqzgj-filter-request = Anfrage

d-3hxkb5pc = Menüanfrage
    .doc = Das Menü, das die Seite zeigte, mit seinen Zeilen.
    .noun = { $count ->
        [one] Menüanfrage
       *[other] Menüanfragen
    }

d-hjqv4o64 = Anfrage

d-f2gbwl6v = Link

d-6b3vpys4 = Einträge

d-3hxkb5pc-filter-request = Anfrage

d-hmusuedw = Menüzeile
    .doc = Eine Zeile des Seitenmenüs: ein Befehl oder eine Trennlinie.
    .noun = { $count ->
        [one] Menüzeile
       *[other] Menüzeilen
    }

d-kybyncey = Befehl

d-cgfpo6b7 = Bezeichnung

d-hn5vxavq = Deaktiviert

d-qmekhn2b = Angehakt

d-67scpsna = Trennlinie

d-hmusuedw-filter-command = Befehl

d-hmusuedw-filter-disabled = Deaktiviert

d-hmusuedw-filter-checked = Angehakt

d-hmusuedw-filter-separator = Trennlinie

d-e43kzg7c = Anfrage an die Seite
    .doc = Was von einer Seite gefragt wird, wie das Fenster, das sie hält, es liest: ein Vorgang und seine Felder.
    .noun = { $count ->
        [one] Anfrage
       *[other] Anfragen
    }

d-fqvhxdyr = Vorgang

d-hfpnr6ua = Element

d-tpz54oe5 = Text

d-5vdpb6hg = Von

d-wf32vfhg = Um

d-q76mm3tr = Ziehen von

d-byeflqne = Ziehschritte

d-hxjdnlwy = Frame

d-4udfayix = Frames

d-txp5lrru = X

d-bqydqtva = Y

d-wbhz4hh6 = Von x

d-iuschp5g = Von y

d-6i5bwl43 = Nach x

d-hrquaspl = Nach y

d-5ka7kek5 = Rolle

d-davmrr4h = Name

d-dmxbh4fm = Nummer

d-ao4elp2a = Taste

d-jmhhkxjz = Adresse

d-lqvkggvx = Stufe

d-zroiv7hs = Werte

d-ky26jv4a = Pfade

d-iabkjt2r = Schritte

d-zh65uqqk = Seit

d-ersdn54t = Ruhe in ms

d-bgflyuoe = Zeitlimit in ms

d-hjfilxdg = Verschwunden

d-btx3fn7s = Seitenübergreifend

d-zf6y6r3k = Geheim

d-arhpr4t7 = Absenden

d-h6fxerpb = Formular

d-cnvdpt2t = Absendeknopf

d-asha7lz3 = Nach

d-jcrv3nzs = Ins Bild

d-qit2hijp = Taste

d-2zpkv6jg = Aktiviert

d-fggdqfhy = Befehl

d-sizwtymn = Größte Breite

d-htce2xyp = Kennung

d-wbwndzhw = Fragende Stelle

d-nqkenrjw = Benutzername

d-ns2mfzwr = Passwort

d-vwi6rjsr = Annehmen

d-szlyr265 = Erlauben

d-acxwp3qh = Vorwärts

d-m6sqrzav = Weiter

d-e43kzg7c-filter-from = Von

d-e43kzg7c-filter-by = Um

d-e43kzg7c-filter-drag_steps = Ziehschritte

d-e43kzg7c-filter-frames = Frames

d-e43kzg7c-filter-x = X

d-e43kzg7c-filter-y = Y

d-e43kzg7c-filter-from_x = Von x

d-e43kzg7c-filter-from_y = Von y

d-e43kzg7c-filter-to_x = Nach x

d-e43kzg7c-filter-to_y = Nach y

d-e43kzg7c-filter-nth = Nummer

d-e43kzg7c-filter-level = Stufe

d-e43kzg7c-filter-since = Seit

d-e43kzg7c-filter-quiet_ms = Ruhe in ms

d-e43kzg7c-filter-timeout_ms = Zeitlimit in ms

d-e43kzg7c-filter-cross_origin = Seitenübergreifend

d-e43kzg7c-filter-secret = Geheim

d-e43kzg7c-filter-submit = Absenden

d-e43kzg7c-filter-enabled = Aktiviert

d-e43kzg7c-filter-command = Befehl

d-e43kzg7c-filter-max_width = Größte Breite

d-e43kzg7c-filter-id = Kennung

d-e43kzg7c-filter-accept = Annehmen

d-e43kzg7c-filter-allow = Erlauben

d-e43kzg7c-filter-forward = Vorwärts

d-e43kzg7c-filter-next = Weiter

d-nalfirw6 = Antwort
    .doc = Was eine Seite geantwortet hat: ob sie tat, was gefragt war, was sie sagte und was sie fand.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }

d-2ubdeo5l = In Ordnung

d-pv4ylrjw = Code
    .doc = Warum nicht: der Code des Engines und was er sagte.

d-d7izzogo = Meldung

d-ta6htvhh = Adresse
    .doc = Wo die Seite ist, wie sie heißt, ob sie lädt und ob sie irgendwohinging.

d-xzex263o = Titel

d-htwzro7c = Navigation

d-vptvuat4 = Lädt

d-vvykamug = Abriss
    .doc = Was die Seite sagt, Zeile für Zeile: der Abriss einer Lesung, die Zeilen einer Tabelle, der Wert eines Feldes.

d-i2okiki6 = Element
    .doc = Das Element, das eine Suche nennt, und die Abrisszeile, die die Worte sagt.

d-mmtsrl7r = Zeile

d-p6uplgxr = Rolle

d-7zlc443q = Name

d-2ibb6fxi = Frame

d-cxsstht7 = Bild
    .doc = Ein Bild der Seite als PNG des Engines, base64.

d-2ihrsavk = Start
    .doc = Der Browser öffnet auf einer Seite: ein Arbeitsbereich ohne eine bekommt seine Startseite als eine.

d-eryy3sog = Seite lesen
    .doc = Was die Seite von `p` sagt und tut, getragen zum Fenster, das sie hält.

d-eryy3sog-from = Von

d-eryy3sog-frame = Frame

d-ahqulvrl = Änderungen lesen
    .doc = Was sich seit der letzten Lesung geändert hat: die hinzugekommenen und entfernten Zeilen, oder der ganze Abriss, wenn das Dokument neu ist.

d-ahqulvrl-frame = Frame

d-pqjr74hw = Element an einem Punkt
    .doc = Was ein Klick auf einen Punkt träfe, durch Überlagerungen und Banner, und das Element, das es hält.

d-pqjr74hw-x = X

d-pqjr74hw-y = Y

d-pqjr74hw-frame = Frame

d-43i6py3k = Auf die Seite warten
    .doc = Warte, bis die Seite ruhig ist, oder bis die Worte, die sie sagen muss, da sind und die, die sie nicht sagen darf, weg sind.

d-43i6py3k-quiet_ms = Ruhe in ms

d-43i6py3k-timeout_ms = Zeitlimit in ms

d-43i6py3k-text = Text

d-43i6py3k-gone = Verschwunden

d-43i6py3k-frame = Frame

d-m4mydrip = Seitenkonsole lesen
    .doc = Die Konsolenmeldungen und unbehandelten Fehler der Seite ab einer laufenden Nummer.

d-m4mydrip-since = Seit

d-j2jhcwgf = Seitennetz lesen
    .doc = Die Netzwerkanfragen der Seite ab einer laufenden Nummer: Methode, Adresse, Art, Status und Fehler, nie Kopfzeilen, Inhalte oder Cookies.

d-j2jhcwgf-since = Seit

d-g723ncyu = Seitenprotokoll lesen
    .doc = Das Protokoll, wer was in der Seite tat, ab der Stelle, die `since` nennt.

d-g723ncyu-since = Seit

d-bv6svg6f = Seitenfelder lesen
    .doc = Jedes Feld des Dokuments mit dem, was es hält, und wer es setzte.

d-bv6svg6f-frame = Frame

d-cep6j5on = Seitenformulare lesen
    .doc = Die Formulare des Dokuments als das, was sie fragen: ihre Fragen, die angebotenen Antworten, was Pflicht ist und die Knöpfe, die sie senden.

d-cep6j5on-frame = Frame

d-igfrldzj = Seitentabelle lesen
    .doc = Die Tabellen der Seite als Zeilen, eine lange in Teilen gelesen.

d-igfrldzj-element = Element

d-igfrldzj-from = Von

d-igfrldzj-frame = Frame

d-lfdelqpf = Text auf der Seite finden
    .doc = Die Abrisszeilen, die `text` enthalten, jede mit ihrer Zeilennummer.

d-lfdelqpf-text = Text

d-lfdelqpf-frame = Frame

d-lfdelqpf-frames = Frames

d-5qgw32u3 = Element auf der Seite finden
    .doc = Das Element, das Rolle und Name nennen, und die Referenz, die dort wirkt.

d-5qgw32u3-role = Rolle

d-5qgw32u3-name = Name

d-5qgw32u3-nth = Nummer

d-5qgw32u3-frame = Frame

d-5qgw32u3-frames = Frames

d-jbqrqf3y = Seite anklicken
    .doc = Drücke das Element, wie eine Hand es tut.

d-jbqrqf3y-element = Element

d-jbqrqf3y-frame = Frame

d-d3vp3i7b = Über die Seite fahren
    .doc = Bewege den Zeiger über ein Element oder zu einem Punkt der Seite.

d-d3vp3i7b-element = Element

d-d3vp3i7b-x = X

d-d3vp3i7b-y = Y

d-u6q76ca7 = Auf der Seite ziehen
    .doc = Ziehe von einem Element oder Punkt zu einem anderen, in Schritten.

d-u6q76ca7-from = Von

d-u6q76ca7-from_x = Von x

d-u6q76ca7-from_y = Von y

d-u6q76ca7-to = Nach

d-u6q76ca7-to_x = Nach x

d-u6q76ca7-to_y = Nach y

d-u6q76ca7-steps = Schritte

d-v33dekps = Seite ausfüllen
    .doc = Schreibe `text` in ein bearbeitbares Element.

d-v33dekps-element = Element

d-v33dekps-text = Text

d-v33dekps-frame = Frame

d-e73dpir4 = Ein Geheimnis eintragen
    .doc = Schreibe ein Geheimnis in ein Passwort- oder Kartenfeld: es wird nie im Protokoll der Seite behalten.

d-e73dpir4-element = Element

d-e73dpir4-text = Text

d-e73dpir4-frame = Frame

d-mniscmpx = Auf der Seite auswählen
    .doc = Wähle Optionen einer Auswahl.

d-mniscmpx-element = Element

d-mniscmpx-values = Werte

d-mniscmpx-frame = Frame

d-rb2fsv4n = Dateien auf der Seite wählen
    .doc = Wähle die Dateien eines Dateifeldes, mit absolutem Pfad.

d-rb2fsv4n-element = Element

d-rb2fsv4n-paths = Pfade

d-rb2fsv4n-frame = Frame

d-ls3i6oxj = Taste auf der Seite drücken
    .doc = Drücke eine Taste in der Seite, etwa `Control+a` oder `Enter`.

d-ls3i6oxj-key = Taste

d-ls3i6oxj-element = Element

d-ls3i6oxj-frame = Frame

d-arvpanvu = Seite absenden
    .doc = Sende das Formular des Elements durch die eigene Prüfung der Seite.

d-arvpanvu-element = Element

d-arvpanvu-cross_origin = Seitenübergreifend

d-arvpanvu-frame = Frame

d-ag3d3seg = Zeiger auf der Seite
    .doc = Eine vertraute Zeigerbewegung: drücken am ersten Punkt, durch die übrigen fahren, am letzten loslassen, für das Zeichnen auf Flächen.

d-ag3d3seg-steps = Schritte

d-ag3d3seg-button = Taste

d-xtqaaha5 = Seite scrollen
    .doc = Drehe das Rad der Seite an den Anfang oder das Ende, ins Bild oder um eine Strecke.

d-xtqaaha5-element = Element

d-xtqaaha5-to = Nach

d-xtqaaha5-by = Um

d-xtqaaha5-into_view = Ins Bild

d-xtqaaha5-frame = Frame

d-g7tvdtci = Seite aufnehmen
    .doc = Ein Bild der Seite oder des Kastens eines Elements als PNG.

d-g7tvdtci-element = Element

d-g7tvdtci-max_width = Größte Breite

d-g7tvdtci-frame = Frame

d-ckclpobv = Zu einer Adresse gehen
    .doc = Öffne eine Adresse im eigenen Fenster der Seite: eine Website ohne Schema öffnet sich als solche (`example.org`), und Wörter, die keine Adresse nennen, öffnen die Antwort der Suchmaschine darauf.

d-ckclpobv-url = Adresse

d-gencz4pa = Zurück
    .doc = Gehe zurück zur Seite davor.

d-nrcwafmk = Vorwärts
    .doc = Gehe vorwärts zu der Seite, der diese folgte.

d-fb4z5f2e = Neu laden
    .doc = Lade die Seite neu.

d-f4lsizjk = Anhalten
    .doc = Halte das Laden der Seite an.

d-daojkpqh = Leseansicht
    .doc = Zeige den Artikel der Seite ohne ihr Beiwerk.

d-daojkpqh-enabled = Aktiviert

d-d3sudkbf = Zoom
    .doc = Zeige die Seite mit dem Zoom `level`, wobei 1 die tatsächliche Größe ist.

d-d3sudkbf-level = Stufe

d-i6rhfim2 = Auf der Seite suchen
    .doc = Finde Wörter in der Seite, wie es die Suchleiste tut.

d-i6rhfim2-text = Text

d-i6rhfim2-forward = Vorwärts

d-i6rhfim2-next = Weiter

d-67gj6gbh = Suche beenden
    .doc = Beende die Suche der Suchleiste.

d-hpktlikg = Dialog der Seite beantworten
    .doc = Beantworte eine eigene Frage der Seite: einen Hinweis, eine Bestätigung oder eine Eingabe.

d-hpktlikg-id = Kennung

d-hpktlikg-accept = Annehmen

d-hpktlikg-text = Text

d-4djvsjzf = Fragen der Seite prüfen
    .doc = Liest, worauf die Seite wartet, in das Protokoll ihres Fensters: die Frage, die sie stellt, das Menü, das sie zeigte, und wie weit sie geladen ist.

d-u7fbemnk = Seite geöffnet
    .doc = Das Fenster der Seite wurde geöffnet: es legt den Motor einer privaten Seite fest, bevor etwas gelesen wird, und liest dann, was die Seite fragt und zeigt, bevor jemand hinsieht.

d-qbox6son = Befehl des Seitenmenüs
    .doc = Nimmt eine Zeile des Seitenmenüs, über den Befehl, den der Engine ihr gab.

d-qbox6son-id = Kennung

d-qbox6son-command = Befehl

d-s7qhc2c6 = Ging zu
    .doc = Die Seite ging irgendwohin: wo sie jetzt ist, steht auf der Seite, und der Besuch kommt in den Verlauf, sofern das Fenster der Seite ihn nicht pausiert hat.

d-s7qhc2c6-url = Adresse

d-og3g2nds = Die Seite fragen
    .doc = Die Seite `p` tut, was `request` (JSON, wie das Fenster der Seite es liest) sagt, und antwortet mit dem Bericht der Seite.

d-og3g2nds-request = Anfrage

d-o2occ235 = Downloads
    .doc = Die Dateien, die die Seite herunterlädt, wie das Fenster sie zuletzt gelesen hat: Zustand des Motors, nie der eigene Eintrag der Seite.

d-cjw2xcfs = Download
    .noun = { $count ->
        [one] Download
       *[other] Downloads
    }
    .doc = Eine Datei, die eine Seite herunterlädt: woher sie kommt, wohin sie ging und wie weit sie ist.

d-anfisgsq = Übertragung
    .doc = Die Nummer des Motors für die Übertragung, die nennt, was die Person mit ihr tut.

d-in525zqp = Zustand
    .doc = `awaiting`, `downloading`, `completed`, `failed` oder `cancelled`.

d-wz6wi2p3 = Name

d-z4gzfvzw = URL

d-feka5n4y = Pfad
    .doc = Wohin sie gespeichert wurde, sobald sie vollständig ist.

d-kubpqqew = Wiederholbar
    .doc = Ob der Motor sie nach einem Fehler erneut starten kann.

d-cokrfftg = Empfangen
    .doc = Wie viele Bytes angekommen sind, von wie vielen der Motor weiß: keine, wenn er es nicht weiß.

d-pbavpdyk = Gesamt

d-wfaksaa6 = Fehler

d-ntpmcicu = Antworten
    .doc = Womit jede Frage eines Formulars beantwortet wird, nach dem Namen der Frage.

d-qvvu7twk = Eingetippt
    .doc = Die Antwort trägt, was die Person beim Beantworten der Frage getippt hat, auf dem Weg zum Fenster aus ihrer Zustimmung eingesetzt: sie ist nie ein Wert des Laufs.

d-e43kzg7c-filter-typed = Eingetippt

d-4unqkzgu = Formular beantworten
    .doc = Beantwortet ein ganzes Formular der Seite in einem Schritt: jede Frage mit ihrer Antwort, durch die Prüfung der Seite.
d-4unqkzgu-form = Formular
d-4unqkzgu-answers = Antworten
d-4unqkzgu-submit = Senden
d-4unqkzgu-cross_origin = Ursprungsübergreifend
d-4unqkzgu-frame = Frame

d-lvbnkhyd = Der Seite erlauben
    .doc = Gibt einer Seite, was sie erbeten hat (Kamera, Mikrofon, Standort, Mitteilungen).
d-lvbnkhyd-id = Id
d-lvbnkhyd-origin = Ursprung
d-lvbnkhyd-wants = Möchte

d-p7w6i37f = Download speichern
    .doc = Speichert die Datei, die eine Seite anbietet.
d-p7w6i37f-id = Id
d-p7w6i37f-name = Name
d-p7w6i37f-url = URL

d-jzj2zsuv = Download nicht speichern
    .doc = Lehnt die Datei ab, die eine Seite anbietet: nichts wird gespeichert, und die Seite erfährt es.
d-jzj2zsuv-id = Id
d-jzj2zsuv-name = Name

d-6a4m4f4s = Adresse herunterladen
    .doc = Speichert, was eine Adresse liefert, in den Downloads des Browsers.
    .w-type-the-address-to-download = Gib die Adresse zum Herunterladen ein
d-6a4m4f4s-url = URL

d-boafhtzg = Download abbrechen
    .doc = Hält einen laufenden Download an.
d-boafhtzg-d = D

d-dmiputmu = Download wiederholen
    .doc = Startet einen fehlgeschlagenen Download von vorn.
d-dmiputmu-d = D

d-boev3sbj = Alle Downloads abbrechen
    .doc = Hält alle Downloads dieser Seite an.

d-trhtau5i = Der Erweiterung erlauben
    .doc = Gibt einer Erweiterung den zusätzlichen Zugriff, um den sie gebeten hat.
d-trhtau5i-id = Id
d-trhtau5i-extension = Erweiterung
d-trhtau5i-could = Könnte

d-4xa6zyzf = Bei der Seite anmelden
    .doc = Meldet bei einer Seite an, die nach Benutzername und Passwort fragt.
    .take-1 = Benutzername
    .take-2 = Passwort
d-4xa6zyzf-id = Id
d-4xa6zyzf-asker = Fragender
d-4xa6zyzf-site = Seite
d-4xa6zyzf-realm = Bereich

d-4i65tg5t = Anmeldung abbrechen
    .doc = Lehnt die Anmeldung einer Website ab: nichts wird gesendet, und der Server zeigt seine eigene Seite.
d-4i65tg5t-id = Id
d-4i65tg5t-asker = Fragender

d-psytxnym = Senden lassen
    .doc = Lässt hinaus, was die Seite sendet: ein Formular, eine Navigation oder eine Anfrage, die wartet, bis die Person es sagt.
d-psytxnym-id = Id
d-psytxnym-origin = Ursprung
d-psytxnym-method = Methode
d-psytxnym-leaving = Geht hinaus

d-7gjwwcba = Dieses Lesezeichen benennen
    .w-a-bookmark-is-called-by-some = Ein Lesezeichen braucht einen Namen
    .doc = Benennt eine gespeicherte Seite um, wenn der Person ihr eigener Name dafür besser gefällt als der Titel.
d-7gjwwcba-title = Titel

d-vyepjajq = Lesezeichen in einen Ordner legen
    .doc = Legt ein Lesezeichen in einen Ordner der eigenen Bibliothek, etwa `Work/Reading`, oder ganz oben, wenn der Ordner leer ist.
d-vyepjajq-folder = Ordner

d-eb3wtiud = Formular mit einer Adresse ausfüllen
    .w-this-form-asks-nothing-an-address = Dieses Formular fragt nichts, was eine Adresse beantwortet
    .doc = Füllt ein Formular der Seite mit einer gespeicherten Adresse, Frage für Frage, ohne es zu senden. Die eigene Prüfung der Seite entscheidet weiterhin, ob es gesendet werden könnte.
d-eb3wtiud-form = Formular
d-eb3wtiud-a = A

d-fxq6nhpy = Adressseite
    .w-the-address = Die Adresse
    .w-name-for-it = Name dafür
    .w-home = Zuhause
    .w-full-name = Vollständiger Name
    .w-given-name = Vorname
    .w-family-name = Nachname
    .w-street = Straße
    .w-city = Stadt
    .w-region = Region
    .w-postal-code = Postleitzahl
    .w-country = Land
    .w-delete-address = Adresse löschen

d-6w4zu5de = Downloadzeile
    .w-downloading = Wird heruntergeladen
    .w-cancel = Abbrechen
    .w-retry = Wiederholen
    .w-open = Öffnen
    .w-show-in-files = In Dateien anzeigen

d-eg4w4fow = Stumm
    .doc = Ob der Ton der Seite stummgeschaltet ist.

d-ctm6ma5g = Zurück
    .doc = Die Seiten vor dieser und die danach, wie der Motor sie zuletzt auf eine Anfrage geantwortet hat.

d-znzxwmxa = Vorwärts

d-frfmgq2w = Verlaufseintrag
    .noun = { $count ->
        [one] Verlaufseintrag
       *[other] Verlaufseinträge
    }
    .doc = Eine Seite im eigenen Verlauf der Seite: wo sie ist, wie sie hieß und die Nummer des Motors für sie, die das Zurück- oder Vorwärtsgehen nennt.

d-eyk6uw4k = Eintrag

d-2chmpqlt = URL

d-uugq5btr = Titel

d-zoqmh2dz = Stumm
    .doc = Ob der Ton der Seite stummgeschaltet ist.

d-l3gmhwrx = Richtung
    .doc = In welche Richtung der eigene Verlauf der Seite gelesen wird: `back` oder `forward`.

d-pucrebly = Eintrag
    .doc = Die Nummer der Seite für einen Eintrag ihres Verlaufs, die das Gehen dorthin nennt.

d-e43kzg7c-filter-muted = Stumm

d-e43kzg7c-filter-entry = Eintrag

d-hd7ra2gz = Seite stummschalten
    .doc = Schaltet den Ton der Seite stumm oder lässt ihn wieder spielen.
d-hd7ra2gz-muted = Stumm

d-ugidwhtg = Frühere Seiten
    .doc = Fragt die Seite, wohin sie zurückgehen kann. Der Motor antwortet mit den Seiten vor dieser, die der Eintrag der Seite hält, sobald ihr Fenster sie wieder liest.

d-kwhpfr2t = Spätere Seiten
    .doc = Fragt die Seite, wohin sie vorwärts gehen kann; die Antwort wird wie bei `read_back_history` behalten.

d-unfwzbsr = Zu einer Seite ihres Verlaufs gehen
    .doc = Geht zu einer Seite im eigenen Verlauf der Seite, nach dem Eintrag, den ihr Lesen genannt hat.
d-unfwzbsr-entry = Eintrag


d-252payib = Seite
    .doc = Verbindung, Berechtigungen und gespeicherte Daten der Website, wie das Fenster sie zuletzt gelesen hat.

d-qeovjxlz = Seiteninfo
    .noun = { $count ->
        [one] Seiteninfo
       *[other] Seiteninfos
    }
    .doc = Verbindung, Berechtigungen und gespeicherte Daten einer Website, wie das Fenster der Seite sie zuletzt gelesen hat.

d-hrkhpkbd = Ursprung

d-scje2sv3 = Generation
    .doc = Die Generation des Dokuments, für die der Motor geantwortet hat: eine Website-Aktion nennt sie, damit ein Befehl nie auf eine spätere Seite trifft.

d-lyopffwi = Blockiert
    .doc = Wie viele Anfragen in dieser Seite der Trackingschutz blockiert oder ersetzt hat.

d-az2v2pr3 = Geschützt
    .doc = Ob der Trackingschutz für diese Website eingeschaltet ist.

d-ageqt2tb = Sicherheit
    .doc = `secure`, `mixed`, `insecure` oder `unverified`.

d-i3vtfymr = Zertifikat
    .doc = Der eigene Zustand des Zertifikats, wie der Motor ihn nummeriert: 0, wenn er keinen gemeldet hat.

d-zarq7l6o = Aussteller

d-5nsqo5b5 = Betreff

d-gsqr2kkr = Berechtigungen
    .doc = Jedes, wonach die Website fragen darf, als `camera: ask`, `microphone: blocked`.

d-eygtkatk = Zurücksetzbar
    .doc = Ob die Website erneut fragen darf, was ihr verweigert wurde.

d-4vqdzqut = Aktion
    .doc = Was eine Website-Aktion tut: `clear`, `revoke`, `reset`, `enable`, `disable` oder `reload`.

d-fbtgqfhx = Ursprünge
    .doc = Die Websites, für die der Trackingschutz aus ist und die der Motor neben seinen eigenen Listen führt.

d-ymm5jh2h = Ursprung
    .doc = Die Website, um die es bei einer Website-Aktion oder einem Status geht.

d-7gcvts2u = Generation
    .doc = Die Dokumentgeneration, bei der die Website gelesen wurde, die eine Website-Aktion nennt.

d-e43kzg7c-filter-generation = Generation

d-qdr46mze = Seiteninformationen
    .w-this-page-is-not-on-a = Diese Seite gehört zu keiner Website
    .doc = Liest Verbindung, Zertifikat, die vom Trackingschutz blockierten Anfragen und wonach die Website fragen darf in den Eintrag der Seite.

d-xsnvn6le = Daten dieser Website löschen
    .w-read-the-site-s-information-first = Lies zuerst die Informationen zur Website
    .doc = Löscht Speicher, Cookies und gemerkte Formulare dieser Website, auch Cookies, die sie mit ihren Subdomains teilt, und lädt sie neu.

d-muzeyo2h = Blockieren, wonach diese Website fragt
    .w-read-the-site-s-information-first = Lies zuerst die Informationen zur Website
    .doc = Blockiert, wonach die Website fragen darf (Kamera, Mikrofon, Standort und Mitteilungen).

d-63b5vewx = Website darf erneut fragen
    .w-read-the-site-s-information-first = Lies zuerst die Informationen zur Website
    .doc = Lässt die Website erneut fragen, was ihr verweigert wurde.

d-kqjsvdfh = Die Websites anwenden, für die der Trackingschutz aus ist
    .doc = Sagt dem Motor, für welche Websites der Trackingschutz aus ist, wie die eigene Liste des Browsers sie nennt: der Motor führt dieselbe Liste neben seinen Filtern.


d-k3vqdvsp = Download öffnen
    .w-save-the-download-first = Speichere den Download zuerst
    .doc = Öffnet einen gespeicherten Download in der App, die seine Art öffnet, über das eine Öffnen des Systems.
d-k3vqdvsp-name = Name
d-k3vqdvsp-path = Pfad

d-rllv4b5a = Entwicklerwerkzeuge
    .doc = Öffnet die Entwicklerwerkzeuge des Motors für diese Seite: ihre Konsole, ihre Elemente und ihr Netzwerk.

d-3xz2iwbw = Download aktualisieren
    .doc = Schreibt, was die Seite zuletzt über einen ihrer Downloads gesagt hat, in den Eintrag der Seite, wie ihr Fenster es weitergibt, während die Übertragung läuft. Das Fenster ruft sie auf, nie eine Person, ein Agent oder ein Skript.
d-3xz2iwbw-id = Id
d-3xz2iwbw-state = Zustand
d-3xz2iwbw-name = Name
d-3xz2iwbw-url = URL
d-3xz2iwbw-path = Pfad
d-3xz2iwbw-retryable = Wiederholbar
d-3xz2iwbw-received = Empfangen
d-3xz2iwbw-total = Gesamt
d-3xz2iwbw-error = Fehler

d-7roz5txv = Verlauf merken
    .doc = Schreibt die Seiten, zu denen die Seite in eine Richtung gehen kann, in ihren Eintrag, wie ihr Fenster sie weitergibt: was das Menü dann neben Zurück und Vorwärts anbietet.
d-7roz5txv-direction = Richtung
d-7roz5txv-entries = Einträge

d-ptwqhcz2 = Von der Seite geöffnet
    .doc = Behält eine Seite, die die Seite in einem eigenen Fenster geöffnet hat, als Seite dieses Browsers, wie ihr Fenster sie weitergibt: ein Popup mit eigener Größe oder ein neuer Tab.
d-ptwqhcz2-url = URL
d-ptwqhcz2-popup = Popup

d-wrsl2noi = Verlauf kürzen
    .w-a-history-keeps-at-least-one = Ein Verlauf behält mindestens eine Seite
    .doc = Behält die eigenen Besuche der Person bis zu den neuesten `most` und vergisst die ältesten: der Verlauf bleibt, was eine Person lesen könnte.
d-wrsl2noi-most = Höchstzahl


d-tqd4wzqk = Download
d-wutb5pms = Download öffnen
d-nja545os = Website lesen
d-3wo5bbtr = Website-Daten löschen
d-h3g4heky = Website-Berechtigungen blockieren
d-j4t6lwtn = Website erneut fragen lassen
d-asdqm6oc = Lesezeichen benennen
d-xbrdtvx6 = Adresse ausfüllen
d-sb77wivf = Verlauf kürzen

d-nfmtkvs7 = Anmeldeformulare
    .doc = Die Anmeldeformulare, die der Motor auf dieser Seite gefunden hat, wie sein Anmeldespeicher sie aufgelistet hat.

d-jbnzv4tj = Anmeldekonten
    .doc = Die Anmeldungen, die der Motor für diese Website aufbewahrt.

d-jz6xwcus = Aufzeichnung
    .doc = Die Aufzeichnung, die der Motor von einer auf dieser Seite gesendeten Anmeldung gemacht hat, bis sie gespeichert oder verworfen wird.

d-wqz44fsx = Hinweis zur Anmeldung
    .doc = Was der Anmeldespeicher des Motors zuletzt geantwortet hat, in seinen eigenen Worten.

d-2hbds7ee = Anmeldeformular
    .doc = Ein Anmeldeformular, das der Motor auf der Seite gefunden hat: seine eigene Nummer dafür, das Konto, das es anbietet, und welche Art es ist (`sign-in` oder `create`).
    .noun = { $count ->
        [one] Anmeldeformular
       *[other] Anmeldeformulare
    }

d-hdxbxfcl = Formular

d-enz3azev = Benutzername

d-kqqay3gr = Art

d-q3arly5t = Anmeldekonto
    .doc = Eine Anmeldung, die der Tresor des Arbeitsbereichs für diese Website aufbewahrt: das Tresorelement, das sie ist (`vault.item:<id>`), wie sie heißt und der Name, mit dem sie sich anmeldet. Ihr Passwort bleibt im Tresor, geht nur an den Motor der Seite, wenn er ein Formular ausfüllt, und erreicht die App nie.
    .noun = { $count ->
        [one] Anmeldekonto
       *[other] Anmeldekonten
    }

d-53raewop = Konto

d-5oqresy2 = Benutzername

d-qpgkf2v5 = Titel

d-ultqehya = Eigene
    .doc = Ob der Browser sie selbst gespeichert hat: Sie auszufüllen fragt niemanden. Jede andere Anmeldung der Website wird ausgefüllt, sobald ihre Person dem Browser genau diese eine Anmeldung erlaubt hat.

d-wdlbrp6r = Anmeldedaten
    .doc = Die Aktion des Anmeldespeichers im Motor: `status`, `cancel`, `discover`, `addresses`, `list`, `capture`, `save`, `update`, `fill`, `delete` oder `fill_address`.

d-x6klrsfn = Aufzeichnung
    .doc = Die Aufzeichnung, die der Motor von einem Anmeldeformular gemacht hat, die Speichern oder Aktualisieren benennt.

d-o345kv2f = Konto
    .doc = Die Nummer des Motors für ein gespeichertes Konto, die Einsetzen oder Löschen benennt.

d-cirolhk7 = Adresse
    .doc = Die Adresse, mit der ein Formular über den Motor ausgefüllt wird.

d-e43kzg7c-filter-address = Adresse

d-o4s54tdd = Anmeldungen dieser Seite finden
    .doc = Liest die Anmeldeformulare, nach denen diese Seite fragt, über den Anmeldespeicher des Motors.

d-z745w65b = Gespeicherte Anmeldungen
    .doc = Listet die Anmeldungen, die der Motor für diese Website aufbewahrt.

d-jdz7mpvk = Diese Anmeldung speichern
    .doc = Nimmt, was die Person in ein Anmeldeformular getippt hat, damit es für diese Website gespeichert werden kann.

d-jdz7mpvk-f = F

d-uz7qxlux = Anmeldung speichern
    .doc = Speichert die Aufzeichnung, die der Motor für diese Website gemacht hat.
    .w-take-the-login-first = Nimm die Anmeldung zuerst auf

d-gzdjcbf4 = Gespeicherte Anmeldung einsetzen
    .doc = Füllt ein Formular dieser Seite mit einer Anmeldung, die der Motor aufbewahrt.

d-gzdjcbf4-f = F

d-gzdjcbf4-a = A

d-obkrjsac = Gespeicherte Anmeldung aktualisieren
    .doc = Ersetzt eine gespeicherte Anmeldung durch die Aufzeichnung, die der Motor gemacht hat.
    .w-take-the-login-first = Nimm die Anmeldung zuerst auf

d-obkrjsac-f = F

d-obkrjsac-a = A

d-3vy7jxmm = Gespeicherte Anmeldung vergessen
    .doc = Vergisst eine Anmeldung, die der Motor für diese Website aufbewahrt.

d-3vy7jxmm-a = A

d-rycvvkrz = Anmeldung nicht speichern
    .doc = Lehnt die Anmeldung ab, die der Motor zum Speichern anbietet.

d-qhntuc74 = Adressen, die Formulare dieser Seite annehmen
    .doc = Liest die Adressen, mit denen der Motor Formulare ausfüllt.

d-y2b7rzoy = Mit einer gespeicherten Adresse ausfüllen
    .doc = Füllt ein Formular dieser Seite mit einer gespeicherten Adresse, über den Motor.

d-y2b7rzoy-f = F

d-y2b7rzoy-a = A

d-fnrvkh2d = Anmeldedaten merken
    .doc = Schreibt, was der Anmeldespeicher des Motors geantwortet hat, in den Eintrag der Seite: die Formulare und Konten, die er aufgelistet hat, die Aufzeichnung, die er gemacht hat, und seine eigenen Worte.

d-fnrvkh2d-did = Tat

d-fnrvkh2d-success = Erfolg

d-fnrvkh2d-message = Meldung

d-fnrvkh2d-forms = Formulare

d-fnrvkh2d-accounts = Konten

d-fnrvkh2d-capture = Aufzeichnung

d-fnrvkh2d-username = Benutzername

d-fnrvkh2d-late = Verspätet

d-agsgbsb5 = Anmeldung speichern~present

d-5wtzjn5r = Gespeicherte Anmeldung aktualisieren~present

d-ryvmj2zx = Erweiterung
    .doc = Eine auf diesem Gerät installierte Erweiterung: was sie ist, was ihr Manifest verlangt und ob die Person sie eingeschaltet hat.
    .noun = { $count ->
        [one] Erweiterung
       *[other] Erweiterungen
    }

d-vhst373v = Name

d-7b7zbela = Version

d-v7jkztuy = Manifestversion

d-iaycfmmk = Beschreibung

d-kkeg7wgf = Berechtigungen
    .doc = Was die Erweiterung in ihrem Manifest verlangt, in ihren eigenen Worten, eines nach dem anderen.

d-372chbvw = Eingeschaltet

d-oerj5bdj = Datei
    .doc = Die Adresse eines Erweiterungspakets, das die Person gewählt hat, im Inhaltsspeicher dieses Geräts.

d-xic2qsit = Eintrag
    .doc = Der Eintrag im Chrome Web Store, den eine Installation nennt: die Adresse seiner Seite oder die Kennung der Erweiterung.

d-5px2ripq = Erweiterung
    .doc = Die Kennung einer installierten Erweiterung, die Einschalten oder Entfernen nennt.

d-nskkfe4o = Erweiterungen
    .doc = Die auf diesem Gerät installierten Erweiterungen, wie das Fenster einer Seite sie zuletzt aufgelistet hat.

d-uq4ctcke = Hinweis zur Erweiterung
    .doc = Was Installieren, Ein- oder Ausschalten einer Erweiterung ergeben hat, in den Worten der Person.

d-fyfcqngs = Installierte Erweiterungen lesen
    .doc = Liest die auf diesem Gerät installierten Erweiterungen in den Eintrag des Browsers.

d-z2yj3nim = Eine Erweiterung aus einer Datei installieren
    .doc = Installiert das Erweiterungspaket, das die Person gewählt hat, eine vom Herausgeber signierte .crx-Datei oder ein einfaches .zip: der Motor prüft ein signiertes Paket, entpackt es unter seiner eigenen Kennung und listet es auf.

d-z2yj3nim-file = Datei

d-ytmpzscq = Aus dem Chrome Web Store hinzufügen
    .doc = Installiert den Eintrag `said` aus dem Chrome Web Store (die Adresse seiner Seite oder die Kennung der Erweiterung): sein Paket kommt vom Aktualisierungsdienst des Stores, wird geprüft und eingeschaltet, wie der Store eine Erweiterung installiert.
    .w-name-the-extension-or-its-store = Nenne die Erweiterung oder ihre Store-Seite

d-ytmpzscq-said = Adresse oder Kennung

d-wwjywknf = Aus dem Chrome Web Store hinzufügen~present

d-goygbsaa = Die Erweiterungen auf Aktualisierungen prüfen
    .doc = Fragt den Chrome Web Store nach neueren Versionen der hier installierten Store-Erweiterungen und installiert sie: sie sind da, wenn sich das nächste Mal eine Seite öffnet, und werden jetzt geladen.

d-pipb6lni = Erweiterungsaktualisierungen vermerken
    .doc = Schreibt, was die Frage nach neueren Versionen im Chrome Web Store ergeben hat, in den Eintrag des Browsers.

d-pipb6lni-checked = Geprüft

d-pipb6lni-updated = Aktualisiert

d-pipb6lni-failed = Fehlgeschlagen

d-ljfxfb7w = Erweiterung ein- oder ausschalten
    .doc = Schaltet eine installierte Erweiterung im laufenden Fenster ein oder aus: Chromium lädt und entlädt sie, wo die Seite steht, sodass nichts auf einen Neustart des Fensters wartet.

d-ljfxfb7w-e = E

d-ljfxfb7w-on = An

d-u33rt6ml = Erweiterung entfernen
    .doc = Vergisst eine auf diesem Gerät installierte Erweiterung: ihren Ordner und alles, was sie darf.

d-u33rt6ml-e = E

d-hh7ptqf4 = Erweiterungen merken
    .doc = Schreibt die Erweiterungen, die das Fenster einer Seite aufgelistet hat, in den Eintrag des Browsers.

d-hh7ptqf4-installed = Installiert

d-hh7ptqf4-message = Meldung

d-ylu3tat4 = Druckhinweis
    .doc = Warum die Seite nicht gedruckt werden konnte, in den Worten der Person; leer, sobald eine auf dem Weg zum Druckzentrum ist oder war.

d-h46exzv2 = Drucken
    .doc = Schreibt die Seite als PDF und übergibt sie dem Druckzentrum, wo man den Drucker und die Art des Drucks wählt.
    .w-open-a-page-to-print = Öffne eine Seite zum Drucken

d-cyex2shm = Druck vermerken
    .doc = Übergibt das vom Motor geschriebene PDF über ihre eigene Aktion dem Druckzentrum, damit man dort den Drucker und die Art des Drucks wählt; die vom Motor geschriebene Kopie wird entfernt.
    .w-the-page-could-not-be-printed = Die Seite konnte nicht gedruckt werden
    .w-the-page-could-not-be-printed-2 = Die Seite konnte nicht gedruckt werden: { $why }
    .w-the-page-is-too-large-to = Die Seite ist zu groß zum Drucken
    .w-pdf = { $name }.pdf

d-cyex2shm-ok = Erfolg

d-cyex2shm-path = Pfad

d-cyex2shm-name = Name

d-cyex2shm-message = Meldung

d-f2c637xq = Seite drucken~present

d-4gc5tpck = Erweiterung vermerken
    .doc = Schreibt, was Installieren, Ein- oder Ausschalten einer Erweiterung ergeben hat, in den Eintrag des Browsers.

d-4gc5tpck-ok = Erfolg

d-4gc5tpck-kind = Art

d-4gc5tpck-message = Meldung

d-4gc5tpck-id = Kennung

d-iymjskx5 = Schlüssel
    .doc = Die Kennung des Motors für die Erweiterung, die Einschalten oder Entfernen nennt.

d-2kfxgvhv = Download in Dateien anzeigen
    .doc = Zeigt, wo ein gespeicherter Download in Dateien steht, mit der Datei markiert: der eine Ort des Systems für das, was ein Fenster gespeichert hat.
    .w-save-the-download-first = Speichere den Download zuerst

d-2kfxgvhv-path = Pfad

d-arui4kb5 = Download anzeigen~present

d-3j7hm4v2 = Seitenknopf
    .doc = Der Knopf einer Erweiterung in der Leiste der Seite: was er zeigt, ob er einen Druck annimmt und ob ein Druck eine Seite der Erweiterung selbst öffnet.
    .noun = { $count ->
        [one] Seitenknopf
       *[other] Seitenknöpfe
    }

d-t25qin6v = Erweiterung
    .doc = Die Kennung des Motors für die Erweiterung, für die der Knopf steht.

d-pigtj7mb = Beschriftung

d-dizhfbps = Abzeichen

d-ugbnd25j = Eingeschaltet

d-2wovoiro = Popup

d-a45urcsf = Knöpfe
    .doc = Die Knöpfe der Erweiterungen dieser Seite in der Leiste, wie der Motor sie zuletzt gemeldet hat.

d-asvx3wpo = Erweiterung drücken
    .doc = Drückt einen der Erweiterungsknöpfe der Seite: der Motor führt die Aktion der Erweiterung aus oder öffnet die Seite, die sie anbietet.
    .w-the-extension-turned-its-button-off = Die Erweiterung hat ihren Knopf auf dieser Seite ausgeschaltet

d-asvx3wpo-b = B

d-5r7zzpz6 = Erweiterung drücken~present

d-3wa3bkzu = Mitteilungen
    .doc = Die Mitteilungen, die die Erweiterungen dieser Seite gezeigt haben und nicht zurückgenommen haben.

d-4rjencjg = Erweiterungsmitteilung
    .doc = Eine Mitteilung, die eine Erweiterung gezeigt hat: die Erweiterung, von der sie kommt, ihre eigene Nummer dafür, was sie sagt und welche Knöpfe sie anbietet.
    .noun = { $count ->
        [one] Erweiterungsmitteilung
       *[other] Erweiterungsmitteilungen
    }

d-bmmxddfi = Erweiterung
    .doc = Die Kennung des Motors für die Erweiterung, die die Mitteilung gezeigt hat.

d-7wjg3mlt = Mitteilung

d-sqkziu2x = Titel

d-h27qgqgy = Meldung

d-c3idm5kp = Knöpfe

d-xtjgu4pr = Mitteilung
    .doc = Die eigene Nummer der Erweiterung für eine ihrer Mitteilungen.

d-mowvq7xq = Knopf der Mitteilung drücken
    .doc = Drückt einen der Knöpfe, die eine Mitteilung einer Erweiterung anbietet.

d-mowvq7xq-n = M

d-mowvq7xq-button = Knopf

d-w6ubskp2 = Mitteilung wegnehmen
    .doc = Nimmt eine Mitteilung, die eine Erweiterung gezeigt hat, weg, ohne etwas zu drücken.

d-w6ubskp2-n = M

d-6kjhqwh5 = Link öffnen
    .doc = Öffnet einen Link eines Schemas, das dieser Browser beansprucht: die Adresse in einer eigenen Seite, oder die Suche, die ein `web+search:`-Link verlangt. Eine Shell reicht einen Link hierher, ob ein Fenster der App offen ist oder nicht, und was dies zurückgibt, öffnet sich im Fenster.

d-6kjhqwh5-link = Link

d-zh6wrcvk = Video
    .doc = Das Video, das die Seite zeigt, wie das Fenster es zuletzt gelesen hat: das Bild, das Bild im Bild über dem Fenster schweben lässt. Zustand der Engine, nie der eigene Eintrag der Seite.

d-i6yst6vv = Bild im Bild
    .doc = Ob das Video der Seite als Bild im Bild über ihrem Fenster schwebt.

d-gjpou6rr = Seitenvideo
    .doc = Das Video, das eine Seite zeigt, wie die Engine es zuletzt gemeldet hat: was Bild im Bild über dem Fenster schweben lässt, dort wo die Seite es zeichnet. Zustand der Engine wie die Fragen der Seite, nie der eigene Eintrag der Seite.
    .noun = { $count ->
        [one] Seitenvideo
       *[other] Seitenvideos
    }

d-3rpnjayb = Element
    .doc = Die Kennung der Engine für das Element.

d-f3bbhirh = Bezeichnung

d-myq7yqnh = Bild im Bild
    .doc = Lässt das Video der Seite als Bild im Bild über ihrem Fenster schweben, sodass es weiterspielt, während die Person woanders arbeitet, oder legt es dorthin zurück, wo die Seite es zeichnet.

d-myq7yqnh-floating = Schwebend

d-3emd4v5c = Vollbild
    .doc = Die Seite füllt den Bildschirm: Ein Video, eine Präsentation oder die Seite selbst hat sie dorthin gebracht, und ihr Fenster zeigt die Seite allein mit dem Weg hinaus darüber.

d-niox2d32 = Vollbild beenden
    .doc = Verlässt das Vollbild der Seite: Der Motor zeigt die Seite nicht mehr bildschirmfüllend, und ihr Fenster zeigt sie sofort wieder mit ihren eigenen Bedienelementen, weil die Person den Weg hinaus gedrückt hat. Der eigene Bericht des Motors darüber folgt in den Datensatz.

d-s57t5cky = Vollbild merken
    .doc = Schreibt in den Datensatz der Seite, ob sie im Vollbild ist, wie ihr Fenster es übergibt: Ein Video oder eine Präsentation, die den Bildschirm gefüllt hat, oder ihn verlassen hat.

d-s57t5cky-fullscreen = Vollbild

d-m2ajq6vo = Vorschläge
    .w-search-for = Nach „{ $words }“ suchen
