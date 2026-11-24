# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-totp = Authenticator

d-so-tilo-totp-keywords = authentifizierung, zwei-faktor, 2fa, totp, otp, einmalcode, bestätigungscode, sicherheitsschlüssel

d-a4pizi6g = Konto
    .doc = Ein Konto, für das ein Dienst einen Einmalcode verlangt: sein Name, der Schlüssel, aus dem es Codes macht, und der Code, den es jetzt macht.
    .noun = { $count ->
        [one] Konto
       *[other] Konten
    }
    .untitled = Konto

d-wn7r7rgq = Dienst
    .doc = Der Dienst, für den es steht, der das Konto benennt.

d-k6loujfj = Bezeichnung
    .doc = Womit es sich anmeldet: eine E-Mail, ein Benutzername.

d-plyagryo = Verfahren
    .doc = Der Hash, mit dem der Code gemacht wird: `SHA1`, `SHA256` oder `SHA512`.

d-yycfj25v = Stellen

d-hjsiidku = Zeitraum
    .doc = Die Sekunden, die ein Code gilt: 30 bei fast jedem Dienst.

d-2f4aknif = Farbe
    .doc = Ein Farbplatz, an dem die Karte es zeigt.

d-owpfvymk = Code
    .doc = Der Code zum Zeitpunkt `at`, in Dreiergruppen.

d-zwq4t3wg = Nächster
    .doc = Der Zeitpunkt, an dem der Code wechselt, und der, an dem er gemacht wurde, in Millisekunden seit 1970.

d-kwphnayw = Geändert

d-skneduwj = Tresor
    .doc = Der Authenticator des Arbeitsbereichs: die Konten, die seine Mitglieder halten, und was das Fenster zuletzt sagte.
    .noun = { $count ->
        [one] Tresor
       *[other] Tresore
    }

d-4rdnjfde = Konten

d-tcfimqtj = Hinweis

d-glvalakf = Art des Hinweises

d-kmyqik2v = Codefrage
    .noun = { $count ->
        [one] Codefrage
       *[other] Codefragen
    }

d-mv6qivu3 = Konto

d-va7bpptd = Verfahren

d-2i4rwubi = Stellen

d-5chme642 = Zeitraum

d-djhsbhlj = Zeitpunkt in ms

d-flvplbfi = Zeitpunkt

d-kmyqik2v-filter-digits = Stellen

d-kmyqik2v-filter-period = Zeitraum

d-kmyqik2v-filter-at_ms = Zeitpunkt in ms

d-hyfnt6bs = Codeantwort
    .noun = { $count ->
        [one] Codeantwort
       *[other] Codeantworten
    }

d-xdlnshvl = Code

d-s4atxkzh = Übrig

d-hyfnt6bs-filter-left = Übrig

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

d-y6idfclf = Behalten-Anfrage
    .noun = { $count ->
        [one] Behalten-Anfrage
       *[other] Behalten-Anfragen
    }

d-amwu66xm = Konto

d-md2augxf = Schlüssel

d-ea4q75q2 = Schlüssel-Anfrage
    .noun = { $count ->
        [one] Schlüssel-Anfrage
       *[other] Schlüssel-Anfragen
    }

d-45a67ziv = Konto

d-sy74ekqy = Schlüssel-Antwort
    .noun = { $count ->
        [one] Schlüssel-Antwort
       *[other] Schlüssel-Antworten
    }

d-zqveq2mg = Schlüssel

d-r5hx7nb7 = Vergessen-Anfrage
    .noun = { $count ->
        [one] Vergessen-Anfrage
       *[other] Vergessen-Anfragen
    }

d-3pw6frem = Konto

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

d-54fksuzg-file = Datei

d-uzlj3vkz = Konto speichern
    .doc = Ändert, was ein Konto sagt, seinen Schlüssel eingeschlossen (ein Dienst, der ihn neu ausgegeben hat).
    .did = änderte ein Konto
    .w-the-account-needs-a-service-name = Das Konto braucht einen Dienstnamen
    .w-the-account-needs-the-key-the = Das Konto braucht den Schlüssel, den der Dienst gab
    .w-a-code-has-4-to-10 = Ein Code hat 4 bis 10 Stellen
    .w-a-code-changes-every-5-to = Ein Code wechselt alle 5 bis 300 Sekunden
    .w-the-key-could-not-be-kept = Der Schlüssel konnte auf diesem Gerät nicht behalten werden

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
    .doc = Kopiert den aktuellen Code eines Kontos in die Zwischenablage.
    .did = kopierte einen Code

d-koebj2v6 = Erneuern
    .doc = Macht den aktuellen Code jedes Kontos: was der Zeitschalter des Fensters jede Sekunde ausführt und was die Liste liest. Ein Code wird nur geschrieben, wenn er sich geändert hat, also macht ein Code eine Änderung je Zeitraum statt je Sekunde.

d-y5xpyf3z = Ein Konto erneuern

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
    .w-add-from-the-link = Aus dem Link hinzufügen
    .w-service-name = Dienstname
    .w-email-or-username = E-Mail oder Benutzername
    .w-key-from-the-service = Schlüssel vom Dienst
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

d-cmp6cz3k = Stand
