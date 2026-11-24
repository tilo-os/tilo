# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-totp = Authenticator

d-so-tilo-totp-keywords = authentifizierung, zwei-faktor, 2fa, totp, otp, einmalcode, bestätigungscode, sicherheitsschlüssel

d-a4pizi6g = Konto
    .doc = Ein Konto, für das ein Dienst einen Einmalcode verlangt: sein Name und der Tresoreintrag, der den Schlüssel des Dienstes hält.
    .noun = { $count ->
        [one] Konto
       *[other] Konten
    }
    .untitled = Konto

d-wn7r7rgq = Dienst
    .doc = Der Dienst, für den es steht, der das Konto benennt.

d-k6loujfj = Bezeichnung
    .doc = Womit es sich anmeldet: eine E-Mail, ein Benutzername.

d-sj2mtsnu = Eintrag
    .doc = Der Tresoreintrag, der den Schlüssel hält, nach seiner Referenz (`vault.item:<id>`).

d-2f4aknif = Farbe
    .doc = Ein Farbplatz, an dem die Karte es zeigt.

d-skneduwj = Tresor
    .doc = Der Authenticator des Arbeitsbereichs: die Konten, die seine Mitglieder halten, und was das Fenster zuletzt sagte.
    .noun = { $count ->
        [one] Tresor
       *[other] Tresore
    }

d-4rdnjfde = Konten

d-tcfimqtj = Hinweis

d-glvalakf = Art des Hinweises

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

d-ec2dszgu = Konto hinzufügen
    .doc = Beginnt ein Konto aus dem, was die Person getippt hat, oder dem, was ein Einrichtungslink sagte: was ein Agent, ein Skript oder das Formular verlangt.
    .did = fügte ein Konto hinzu
    .w-the-account-needs-a-service-name = Das Konto braucht einen Dienstnamen
    .w-the-account-needs-the-key-the = Das Konto braucht den Schlüssel, den der Dienst gab
    .w-a-code-has-4-to-10 = Ein Code hat 4 bis 10 Stellen
    .w-a-code-changes-every-5-to = Ein Code wechselt alle 5 bis 300 Sekunden

d-ec2dszgu-issuer = Dienst

d-ec2dszgu-label = Bezeichnung

d-ec2dszgu-secret = Schlüssel

d-ec2dszgu-algorithm = Verfahren

d-ec2dszgu-digits = Stellen

d-ec2dszgu-period = Zeitraum

d-ev44bsqe = Aus einem Einrichtungslink hinzufügen
    .doc = Fügt das Konto hinzu, das ein Einrichtungslink nennt: womit der QR-Scan und das Einfügefeld beide enden.
    .did = fügte ein Konto aus einem Einrichtungslink hinzu

d-ev44bsqe-uri = URI

d-54fksuzg = Aus einem Bild hinzufügen
    .doc = Fügt das Konto hinzu, das der QR-Code eines Bildes nennt.
    .did = fügte ein Konto aus einem Bild hinzu
    .w-qr-code-read = QR-Code gelesen

d-54fksuzg-file = Datei

d-uzlj3vkz = Konto speichern
    .doc = Ändert, was ein Konto sagt, seinen Schlüssel eingeschlossen (ein Dienst, der ihn neu ausgegeben hat).
    .did = änderte ein Konto
    .w-the-account-needs-a-service-name = Das Konto braucht einen Dienstnamen
    .w-a-code-has-4-to-10 = Ein Code hat 4 bis 10 Stellen
    .w-a-code-changes-every-5-to = Ein Code wechselt alle 5 bis 300 Sekunden

d-uzlj3vkz-issuer = Dienst

d-uzlj3vkz-label = Bezeichnung

d-uzlj3vkz-secret = Schlüssel

d-uzlj3vkz-algorithm = Verfahren

d-uzlj3vkz-digits = Stellen

d-uzlj3vkz-period = Zeitraum

d-2jxhhwxw = Nach oben
    .doc = Verschiebt ein Konto eine Stelle nach oben.
    .did = verschob ein Konto nach oben

d-2jxhhwxw-a = Konto

d-esml4kb5 = Nach unten
    .doc = Verschiebt ein Konto eine Stelle nach unten.
    .did = verschob ein Konto nach unten

d-esml4kb5-a = Konto

d-g3zipnp4 = Konto löschen
    .doc = Löscht ein Konto und den Schlüssel, den dieses Gerät dafür behalten hat.
    .did = löschte ein Konto

d-g3zipnp4-a = Konto

d-bqscqz5d = Code kopieren
    .doc = Kopiert den aktuellen Code eines Kontos in die Zwischenablage und sagt es.
    .did = kopierte einen Code
    .w-copied-code = Code für { $issuer } kopiert
    .w-could-not-copy-the-code = Der Code konnte nicht kopiert werden

d-rhivupj2 = Ablehnen

d-rhivupj2-why = Grund

d-jcvyrcwb = Formular speichern
    .doc = Speichert das Formular: beginnt das Konto oder ändert das, über dem es geöffnet wurde.

d-jcvyrcwb-editing = In Arbeit

d-jcvyrcwb-issuer = Dienst

d-jcvyrcwb-label = Bezeichnung

d-jcvyrcwb-secret = Schlüssel

d-jcvyrcwb-algorithm = Verfahren

d-jcvyrcwb-digits = Stellen

d-jcvyrcwb-period = Zeitraum

d-htudz4b7 = Hinweis ausblenden

d-vmyv2aoh = Konto hinzufügen

d-3gvwp5ni = Konto speichern

d-4dgdsgtn = Tresorfenster
    .w-authenticator = Authenticator
    .w-add-account = Konto hinzufügen
    .w-back = Zurück
    .w-scan-a-qr-image = QR-Bild scannen
    .w-save = Speichern
    .w-edit-account = Konto bearbeiten
    .w-scan-a-qr-code-paste-a = Scan einen QR-Code, füge einen Einrichtungslink ein oder tippe die Angaben, die der Dienst gab.
    .w-setup-link = Einrichtungslink
    .w-fill-from-the-link = Aus dem Link ausfüllen
    .w-service-name = Dienstname
    .w-email-or-username = E-Mail oder Benutzername
    .w-key-from-the-service = Schlüssel vom Dienst
    .w-new-key-or-leave-empty-to = Neuer Schlüssel, oder leer lassen, um den im Tresor zu behalten
    .w-advanced-options = Erweiterte Optionen
    .w-algorithm = Verfahren
    .w-sha1 = SHA1
    .w-sha256 = SHA256
    .w-sha512 = SHA512
    .w-digits = Stellen
    .w-period-in-seconds = Zeitraum in Sekunden
    .w-dismiss = Ausblenden
    .w-no-accounts-yet = Noch keine Konten
    .w-scan-a-qr-code-or-paste = Scan einen QR-Code oder füge einen Einrichtungslink ein, um seine Einmalcodes hier zu sehen.
    .w-account = Konto
    .w-code-lifetime = Lebensdauer des Codes
    .w-copy = Kopieren
    .w-edit = Bearbeiten
    .w-more = Mehr
    .w-move-up = Nach oben
    .w-move-down = Nach unten
    .w-delete-account = Konto löschen
    .w-accounts = Konten
    .w-copy-code-for = Code für { $issuer } kopieren
    .w-reveal = Anzeigen
    .w-reveal-code-for = Code für { $issuer } anzeigen

d-cmp6cz3k = Stand

d-wkaimxea = Kontofenster
    .w-account = Konto
    .w-code-lifetime = Lebensdauer des Codes
    .w-copy = Kopieren
    .w-copy-code-for = Code für { $issuer } kopieren
    .w-reveal = Anzeigen
    .w-reveal-code-for = Code für { $issuer } anzeigen

d-2sy5ygua = Uhrwarnung
    .w-this-device-s-clock-is-not = Die Uhr dieses Geräts ist nicht synchronisiert, deshalb kann ein Dienst seine Codes ablehnen. Schalte die automatische Zeit ein und versuche es dann noch einmal.
