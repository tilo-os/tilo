# Authenticator auf Deutsch. Keys sind die stabilen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-totp = Authenticator

d-so-tilo-totp-keywords = authentifizierung, zwei-faktor, 2fa, totp, otp, einmalcode, bestätigungscode, sicherheitsschlüssel

d-a4pizi6g = Konto
    .doc = Ein Konto, für das ein Dienst einen Einmalcode verlangt: sein Name und das Tresor-Element, das den Schlüssel des Dienstes hält.
    .noun = { $count ->
        [one] Konto
       *[other] Konten
    }
    .untitled = Konto

d-wn7r7rgq = Dienst
    .doc = Der Dienst, für den es ist; er benennt das Konto.

d-k6loujfj = Bezeichnung
    .doc = Womit es sich anmeldet: eine E-Mail-Adresse, ein Benutzername.

d-sj2mtsnu = Eintrag
    .doc = Das Tresor-Element, das den Seed hält, nach seiner Referenz (`vault.item:<id>`).

d-2f4aknif = Farbe
    .doc = Ein Farbplatz, mit dem die Karte es zeigt.

d-g3aq6san = Zuletzt geändert
    .doc = Wann seine Codes zuletzt anders erzeugt wurden: der Zeitpunkt, an dem die letzte Änderung lief.

d-skneduwj = Tresor
    .doc = Der Authenticator des Arbeitsbereichs: die Konten, die seine Mitglieder führen, was das Fenster zuletzt gesagt hat, und das Konto, das ein Bild gelesen hat und das auf Speichern oder Verwerfen wartet.
    .noun = { $count ->
        [one] Tresor
       *[other] Tresore
    }

d-4rdnjfde = Konten

d-tcfimqtj = Hinweis

d-glvalakf = Art des Hinweises

d-djbrztzn = Gelesen
    .doc = Das Tresor-Element, in dem der Schlüssel eines Bildes aufbewahrt wurde, bis die Person das Konto speichert oder verwirft.

d-abeenkwo = Gelesener Dienst
    .doc = Was das Bild als Konto gesagt hat, damit das Formular es zeigt und eine Person es berichtigen kann.

d-t2vgrcpe = Gelesene Bezeichnung

d-kqaty43e = Gelesenes Verfahren

d-nlfjadyy = Gelesene Stellen

d-ouj3ko22 = Gelesener Zeitraum

d-skneduwj-filter-pending_digits = Gelesene Stellen

d-skneduwj-filter-pending_period = Gelesener Zeitraum

d-yv4c73ut = Einlesefrage
    .noun = { $count ->
        [one] Einlesefrage
       *[other] Einlesefragen
    }

d-qhgdgyyn = Adresse

d-hch5m373 = Eingelesene Antwort
    .noun = { $count ->
        [one] eingelesene Antwort
       *[other] eingelesene Antworten
    }

d-x5elnfmo = Dienst

d-ac2s7eqm = Bezeichnung

d-nsnnmxnd = Schlüssel

d-234azfht = Verfahren

d-cikeo42j = Stellen

d-p5k5ujrm = Zeitraum

d-hch5m373-filter-digits = Stellen

d-hch5m373-filter-period = Zeitraum

d-ytz757ob = Lesefrage
    .noun = { $count ->
        [one] Lesefrage
       *[other] Lesefragen
    }

d-j757xqu7 = Adresse

d-hqh5cg2d = Leseantwort
    .noun = { $count ->
        [one] Leseantwort
       *[other] Leseantworten
    }

d-4jvzelry = Adresse

d-ebyvkahl = Prüffrage
    .noun = { $count ->
        [one] Prüffrage
       *[other] Prüffragen
    }

d-shi3ofgj = Schlüssel

d-a6bx5sr3 = Prüfantwort
    .noun = { $count ->
        [one] Prüfantwort
       *[other] Prüfantworten
    }

d-pon234hn = Gültig

d-a6bx5sr3-filter-ok = Gültig

d-d6cmwqlh = Kontoeintrag
    .doc = Ein Konto, wie ein Skript oder ein Agent es liest: der Dienst, für den es ist, womit es sich anmeldet, und der Tresoreintrag, der seinen Schlüssel hält.
    .noun = { $count ->
        [one] Kontoeintrag
       *[other] Kontoeinträge
    }

d-zhf4j5mk = Dienst

d-7uzo7hvl = Bezeichnung

d-ue7yimiu = Eintrag

d-zxlss5cc = Konten auflisten
    .doc = Die Konten, die der Arbeitsbereich hält, in der Reihenfolge des Fensters: wonach ein Skript, ein Agent oder eine andere App fragt.

d-ec2dszgu = Konto hinzufügen
    .doc = Startet ein Konto aus dem, was die Person eingetippt hat, oder aus dem, was ein Einrichtungslink gesagt hat: wonach ein Agent, ein Skript oder das Formular fragt.
    .did = fügte ein Konto hinzu
    .w-the-account-needs-a-service-name = Ein Konto braucht einen Dienstnamen
    .w-the-account-needs-the-key-the = Ein Konto braucht den Schlüssel, den der Dienst gegeben hat
    .w-a-code-has-4-to-10 = Ein Code hat 4 bis 10 Stellen
    .w-a-code-changes-every-5-to = Ein Code wechselt alle 5 bis 300 Sekunden

d-ec2dszgu-issuer = Dienst

d-ec2dszgu-label = Bezeichnung

d-ec2dszgu-secret = Schlüssel

d-ec2dszgu-algorithm = Verfahren

d-ec2dszgu-digits = Stellen

d-ec2dszgu-period = Zeitraum

d-ec2dszgu-color = Farbe

d-ev44bsqe = Aus einem Einrichtungslink hinzufügen
    .doc = Fügt das Konto hinzu, das ein Einrichtungslink nennt: worin der QR-Scan und das Einfügefeld beide enden.
    .did = fügte ein Konto aus einem Einrichtungslink hinzu

d-ev44bsqe-uri = URI

d-kv7v7kvj = Bild einlesen
    .doc = Liest das Konto, das der QR-Code eines Bildes nennt, und bewahrt seinen Schlüssel im Tresor auf, damit das Formular ihn zeigt: nichts wird hinzugefügt, bis die Person liest, was das Bild gesagt hat, und speichert.
    .w-qr-code-read = QR-Code gelesen

d-kv7v7kvj-file = Datei

d-nztihvkg = Konto hinzufügen
    .doc = Fügt das Konto hinzu, das das Bild gelesen hat, mit dem Schlüssel, den der Tresor bereits dafür hält.
    .did = fügte ein Konto hinzu
    .w-there-is-no-account-read-from = Es gibt kein aus einem Bild gelesenes Konto zum Hinzufügen

d-7vp4wyzb = Konto aus dem Bild verwerfen
    .doc = Lässt das Konto des Bildes los: der Schlüssel, den der Tresor dafür aufbewahrt hat, wird ebenfalls vergessen.
    .did = verwarf das Konto eines Bildes

d-w2yjeczm = Umbenennen
    .doc = Benennt den Dienst eines Kontos um: was eine Person tat, die den angezeigten Namen änderte.
    .did = benannte ein Konto um
    .w-the-account-needs-a-service-name = Ein Konto braucht einen Dienstnamen

d-w2yjeczm-issuer = Dienst

d-w2yjeczm-algorithm = Verfahren

d-w2yjeczm-digits = Stellen

d-w2yjeczm-period = Zeitraum

d-pyqdda5k = Kontonamen ändern
    .doc = Ändert, womit sich ein Konto anmeldet: was eine Person tat, die seinen Namen änderte.
    .did = änderte den Kontonamen

d-pyqdda5k-label = Bezeichnung

d-pyqdda5k-algorithm = Verfahren

d-pyqdda5k-digits = Stellen

d-pyqdda5k-period = Zeitraum

d-fb5iwznq = Umfärben
    .doc = Gibt einem Konto eine eigene Farbe: was eine Person tat, die es umfärbte.
    .did = färbte ein Konto um

d-fb5iwznq-color = Farbe

d-gps7ofew = Code-Erzeugung ändern
    .doc = Ändert den Schlüssel, aus dem ein Konto seine Codes erzeugt, und das Verfahren dafür: was eine Person tat, die einen neuen Schlüssel tippte oder das Verfahren änderte.
    .did = änderte, wie ein Konto seine Codes erzeugt
    .w-that-key-is-not-base32-copy = Dieser Schlüssel ist nicht Base32; kopieren Sie ihn erneut vom Dienst

d-gps7ofew-algorithm = Verfahren

d-gps7ofew-digits = Stellen

d-gps7ofew-period = Zeitraum

d-gps7ofew-secret = Schlüssel

d-icbtggmx = Stellen ändern
    .doc = Ändert, wie viele Stellen die Codes eines Kontos haben: der Verlauf nennt das Konto und seine neue Stellenzahl.
    .did = änderte die Stellenzahl der Codes eines Kontos
    .w-a-code-has-4-to-10 = Ein Code hat 4 bis 10 Stellen

d-icbtggmx-digits = Stellen

d-icbtggmx-algorithm = Verfahren

d-icbtggmx-period = Zeitraum

d-fosvkc5j = Gültigkeit eines Codes ändern
    .doc = Ändert, wie lange die Codes eines Kontos gelten: der Verlauf nennt das Konto und seinen neuen Zeitraum.
    .did = änderte die Gültigkeit der Codes eines Kontos
    .w-a-code-changes-every-5-to = Ein Code wechselt alle 5 bis 300 Sekunden

d-fosvkc5j-period = Zeitraum

d-fosvkc5j-digits = Stellen

d-fosvkc5j-algorithm = Verfahren

d-uzlj3vkz = Konto speichern
    .doc = Ändert, was ein Konto sagt, sowie seinen Schlüssel und wie seine Codes erzeugt werden.
    .did = änderte ein Konto
    .w-the-account-needs-a-service-name = Ein Konto braucht einen Dienstnamen
    .w-a-code-has-4-to-10 = Ein Code hat 4 bis 10 Stellen
    .w-a-code-changes-every-5-to = Ein Code wechselt alle 5 bis 300 Sekunden

d-uzlj3vkz-issuer = Dienst

d-uzlj3vkz-label = Bezeichnung

d-uzlj3vkz-secret = Schlüssel

d-uzlj3vkz-algorithm = Verfahren

d-uzlj3vkz-digits = Stellen

d-uzlj3vkz-period = Zeitraum

d-2jxhhwxw = Nach oben
    .doc = Verschiebt ein Konto eine Stelle nach oben in der Liste.
    .did = verschob ein Konto nach oben

d-2jxhhwxw-a = Konto

d-esml4kb5 = Nach unten
    .doc = Verschiebt ein Konto eine Stelle nach unten in der Liste.
    .did = verschob ein Konto nach unten

d-esml4kb5-a = Konto

d-g3zipnp4 = Konto löschen
    .doc = Löscht ein Konto und den Schlüssel, den der Tresor dafür aufbewahrt hat.
    .did = löschte ein Konto

d-g3zipnp4-a = Konto

d-bqscqz5d = Code kopieren
    .doc = Kopiert den aktuellen Code eines Kontos in die Zwischenablage und sagt es: die Person hat darum gebeten, und ein Fenster, das nichts zeigt, ließe sie raten, ob es geklappt hat.
    .did = kopierte einen Code
    .w-copied-code = Code von { $issuer } kopiert
    .w-could-not-copy-the-code = Der Code konnte nicht kopiert werden

d-rhivupj2 = Ablehnen

d-rhivupj2-why = Grund

d-fovma3g4 = Konto speichern
    .doc = Speichert das Formular: startet das Konto oder ändert das, über dem es geöffnet wurde, und antwortet, ob es gespeichert hat.

d-fovma3g4-editing = In Bearbeitung

d-fovma3g4-issuer = Dienst

d-fovma3g4-label = Bezeichnung

d-fovma3g4-secret = Schlüssel

d-fovma3g4-algorithm = Verfahren

d-fovma3g4-digits = Stellen

d-fovma3g4-period = Zeitraum

d-fovma3g4-color = Farbe

d-fovma3g4-was_algorithm = Vorheriges Verfahren

d-fovma3g4-was_digits = Vorherige Stellen

d-fovma3g4-was_period = Vorheriger Zeitraum

d-htudz4b7 = Hinweis ausblenden

d-hq2cgxdt = Kontofarbe
    .w-7a5cc4 = #7a5cc4
    .w-b0568f = #b0568f
    .w-bf5b45 = #bf5b45
    .w-a8792a = #a8792a
    .w-4f8f4a = #4f8f4a
    .w-2f8f88 = #2f8f88
    .w-6b7280 = #6b7280
    .w-3f7fbf = #3f7fbf

d-jahmvkfr = Fenstertitel
    .w-authenticator = Authenticator
    .w-and-more = { $name_of } und { $value } weitere

d-wyg2oify = Name von
    .w-account = Konto

d-3fqtuxxw = Anzahl
    .w-1-account = 1 Konto
    .w-accounts = { $n } Konten

d-vmyv2aoh = Konto hinzufügen

d-gd3332fr = Gelesenes Konto hinzufügen

d-5yyrzfhd = Konto umbenennen

d-arisd42y = Code-Erzeugung ändern

d-3gvwp5ni = Konto speichern

d-p4suiuad = Stellen ändern

d-23tyzk55 = Gültigkeit ändern

d-4dgdsgtn = Tresorfenster
    .w-add-account = Konto hinzufügen
    .w-back = Zurück
    .w-scan-a-qr-image = QR-Bild einlesen
    .w-save = Speichern
    .w-edit-account = Konto bearbeiten
    .w-read-from-a-picture = Aus einem Bild lesen
    .w-service-name = Dienstname
    .w-email-or-username = E-Mail oder Benutzername
    .w-the-key-it-read-is-kept = Der Schlüssel, den es gelesen hat, wird im Tresor aufbewahrt, bis Sie speichern.
    .w-drop = Verwerfen
    .w-scan-a-qr-code-paste-a = Scannen Sie einen QR-Code, fügen Sie einen Einrichtungslink ein, oder tippen Sie die Angaben ein, die der Dienst gegeben hat.
    .w-read-a-code = Einen Code lesen
    .w-setup-link = Einrichtungslink
    .w-fill-from-the-link = Aus dem Link füllen
    .w-fills-in-the-service-account-and = Füllt Dienst, Konto und Schlüssel ein
    .w-details = Details
    .w-key-from-the-service = Schlüssel vom Dienst
    .w-new-key-or-leave-empty-to = Neuer Schlüssel, oder leer lassen, um den zu behalten, den der Tresor hält
    .w-colour = Farbe
    .w-advanced-options = Erweiterte Optionen
    .w-algorithm = Verfahren
    .w-sha1 = SHA1
    .w-sha256 = SHA256
    .w-sha512 = SHA512
    .w-digits = Stellen
    .w-period-in-seconds = Zeitraum in Sekunden
    .w-dismiss = Ausblenden
    .w-no-accounts-yet = Noch keine Konten
    .w-scan-a-qr-code-or-paste = Scannen Sie einen QR-Code oder fügen Sie einen Einrichtungslink ein, um hier seine Einmalcodes zu sehen.
    .w-accounts = Konten
    .w-account = Konto
    .w-code-lifetime = Lebensdauer des Codes
    .w-copy = Kopieren
    .w-copy-code-for = Code für { $issuer } kopieren
    .w-reveal = Anzeigen
    .w-reveal-code-for = Code für { $issuer } anzeigen
    .w-edit = Bearbeiten
    .w-read-the-code-from-a-screenshot = Liest den Code aus einem Bildschirmfoto oder Foto
    .w-more = Mehr
    .w-move-up = Nach oben
    .w-move-down = Nach unten
    .w-delete-account = Konto löschen
    .w-not-set-up-on-this-device = Auf diesem Gerät nicht eingerichtet
    .w-set-up = Einrichten
    .w-set-up-on-this-device = Auf diesem Gerät einrichten
    .w-codes-are-hidden-until-you-ask = Codes bleiben verborgen, bis Sie einen anzeigen lassen; sie bleiben zehn Sekunden lesbar.

d-wkaimxea = Kontofenster
    .w-account = Konto
    .w-code-lifetime = Lebensdauer des Codes
    .w-copy = Kopieren
    .w-copy-code-for = Code für { $issuer } kopieren
    .w-reveal = Anzeigen
    .w-reveal-code-for = Code für { $issuer } anzeigen

d-2sy5ygua = Uhrwarnung
    .w-this-device-s-clock-is-not = Die Uhr dieses Geräts ist nicht synchronisiert, daher kann ein Dienst diese Codes ablehnen. Schalten Sie die automatische Zeit ein und versuchen Sie es erneut.

d-cmp6cz3k = Stand
