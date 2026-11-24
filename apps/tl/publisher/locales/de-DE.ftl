# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-publisher = App-Veröffentlichung

d-so-tilo-publisher-keywords = publish, release, store upload, app release, submit app, package, signing, deploy, distribute, developer, veröffentlichen, veroeffentlichen, bereitstellen, paketieren, signieren, entwickler, app einreichen

d-zzcx6gzr = Prüfung
    .doc = Was die Registry zum Antrag geantwortet hat, als sie gefragt wurde: ihr Zustand, die Namensräume und jeder Grund, mit dem Zeitpunkt der Prüfung.
    .noun = { $count ->
        [one] Prüfung
       *[other] Prüfungen
    }

d-hia3wxhi = Zeitpunkt
    .doc = Wann der Antrag geprüft wurde.

d-ntbsg676 = Zusammenfassung
    .doc = Die Zeile der Registry selbst: der Zustand, die Namensräume und jeder Grund.

d-zzcx6gzr-filter-at = Zeitpunkt

d-3n5i6fkg = App-Veröffentlichung
    .doc = Die App-Veröffentlichung: der Schlüssel, der deine Apps signiert, und die Namensräume, unter denen die Registry sie veröffentlichen lässt.
    .noun = { $count ->
        [one] Veröffentlichung
       *[other] Veröffentlichungen
    }
    .untitled = App-Veröffentlichung

d-izqbnlov = Registry
    .doc = Die Registry, an die der Schlüssel veröffentlicht: zugangsfreies https oder reines http allein auf diesem Rechner.

d-4hwurbpa = Domain
    .doc = Eine Domain, deren Namensraum beansprucht wird, etwa `example.com`.

d-6fcxou37 = Forge-Konto
    .doc = Ein Forge-Konto, dessen Namensraum beansprucht wird, etwa `github:ada`.

d-22elkpx4 = Kontakt
    .doc = Wie der Betreiber der Registry die Person zum Antrag erreicht.

d-rgwkewz7 = Schlüssel
    .doc = Der Schlüssel dieses Rechners, zuletzt gelesen: `missing`, `present` oder `unreadable`.

d-b63dald4 = Öffentlicher Schlüssel
    .doc = Die öffentliche Hälfte des Schlüssels als Hexadezimalwert; leer, solange es keinen gibt.

d-b6h3el5f = Art des Hinweises
    .doc = Was die letzte Operation gesagt hat: ihre Art (`info`, `success` oder `error`), ihre Überschrift und der Text darunter.

d-laxvdhhk = Überschrift des Hinweises

d-ara6pea6 = Text des Hinweises

d-vu53jaba = Auftrag
    .doc = Der laufende Aufruf der Registry, benannt nach dem Auftrag, den dieser Rechner ihm gab; leer, wenn nichts läuft.

d-etzmfdag = Prüfungen
    .doc = Was die Prüfungen des Antrags gesagt haben, neueste zuerst.

d-viqgi64q = Forge-Teile
    .doc = Ein Forge-Konto, wie die Registry es liest: der Host und das Konto, sein Name in Kleinbuchstaben.
    .noun = { $count ->
        [one] Forge-Teil
       *[other] Forge-Teile
    }

d-y36hrf57 = Host

d-iwdc7ejj = Konto

d-yte5dfvf = Schlüsseldatei
    .doc = Die Schlüsseldatei als Text, zum Kopieren oder Exportieren; nie ein Feld.
    .noun = { $count ->
        [one] Schlüsseldatei
       *[other] Schlüsseldateien
    }

d-6347lg4i = Text

d-lq7hftxp = Antragsanfrage
    .doc = Die Namensräume, die das Profil beansprucht, wie die Engine sie liest: ein Antrag für alle.
    .noun = { $count ->
        [one] Antragsanfrage
       *[other] Antragsanfragen
    }

d-gh566qcn = Registry

d-eb4v6asr = Domain

d-hjj2vyhq = Forge-Konto

d-e2wp4tw7 = Kontakt

d-addusg2k = Statusanfrage
    .doc = Die Registry, deren Antrag gelesen wird.
    .noun = { $count ->
        [one] Statusanfrage
       *[other] Statusanfragen
    }

d-lbble236 = Registry

d-v7l43wxq = Auftragsstand
    .doc = Wie weit ein Auftrag gekommen ist.
    .noun = { $count ->
        [one] Auftragsstand
       *[other] Auftragsstände
    }

d-jqgcvhvb = Zustand

d-wu2vhifg = Zusammenfassung

d-ghygxnrv = Fehler

d-g665cy3q = Auftragsantwort
    .doc = Der Auftrag, den ein Aufruf bekam.
    .noun = { $count ->
        [one] Auftragsantwort
       *[other] Auftragsantworten
    }

d-q55hkx2s = Auftrag

d-mo2pqvcq = Schlüsselstand
    .doc = Der Schlüssel dieses Rechners, wie die Engine ihn liest.
    .noun = { $count ->
        [one] Schlüsselstand
       *[other] Schlüsselstände
    }

d-iyvbvvra = Zustand

d-5gktvyxf = Öffentlicher Teil

d-tu4ielzu = Schlüssel lesen
    .doc = Liest den Publisher-Schlüssel dieses Rechners: ob es einen gibt und seine öffentliche Hälfte.

d-2wikw7t7 = Schlüssel beim Öffnen lesen
    .doc = Das Öffnen der App liest den Schlüssel dieses Rechners.

d-xjn27let = Schlüssel anlegen
    .doc = Legt den Publisher-Schlüssel dieses Rechners an.
    .w-created-your-publisher-key = Deinen Publisher-Schlüssel angelegt
    .w-back-up-publisher-key-json-it = Sichere publisher-key.json: Es ist die einzige Kopie. Öffentlicher Schlüssel { $public }.

d-7rwt45qt = Nachweiszeile kopieren
    .doc = Kopiert die Nachweiszeile: die Worte, die dort zu veröffentlichen sind, wo nur die Person schreiben kann.
    .w-this-computer-has-no-publisher-key = Dieser Rechner hat noch keinen Publisher-Schlüssel.
    .w-copied-the-proof-line = Die Nachweiszeile kopiert.

d-cp6smsn5 = Schlüssel kopieren
    .doc = Kopiert die Schlüsseldatei für die CI der Person: ein Secret, `TILO_PUBLISHER_KEY`, das sie hält.
    .w-copied-the-key = Den Schlüssel kopiert
    .w-paste-it-into-your-ci-s = Füge ihn als Secret TILO_PUBLISHER_KEY in deine CI ein. Die Zwischenablage behält ihn, bis etwas anderes kopiert wird.

d-mje63lgo = Schlüssel exportieren…
    .doc = Die Schlüsseldatei, damit die Person sie sicher aufbewahrt: daraus stellt sie den Schlüssel wieder her.
    .w-exported-your-publisher-key = Deinen Publisher-Schlüssel exportiert
    .w-keep-publisher-key-json-somewhere-safe = Bewahre publisher-key.json sicher auf; es ist die einzige Kopie außerhalb dieses Geräts.

d-uiuxe7rl = Beantragen
    .doc = Bittet die Registry um die Namensräume, die Domain und Forge-Konto des Profils nennen.
    .did = hat Namensräume beantragt
    .w-the-registry-has-not-answered-yet = Die Registry hat noch nicht geantwortet
    .w-a-registry-is-reached-over-credential = Eine Registry wird über zugangsfreies https erreicht oder über reines http allein auf diesem Rechner

d-cyealnod = Antrag prüfen
    .doc = Liest, was die Registry zum Antrag des Schlüssels dieses Rechners hält.
    .did = hat den Antrag geprüft
    .w-the-registry-has-not-answered-yet = Die Registry hat noch nicht geantwortet
    .w-a-registry-is-reached-over-credential = Eine Registry wird über zugangsfreies https erreicht oder über reines http allein auf diesem Rechner

d-ekk7qvwc = Nachfragen
    .doc = Fragt die Registry, wie der laufende Aufruf steht, einmal je Sekunde, solange einer läuft.
    .w-no-application-yet = Noch kein Antrag
    .w-this-registry-has-no-application-from = Diese Registry hat noch keinen Antrag von diesem Schlüssel.
    .w-application-status = Antragsstatus
    .w-the-registry-call-was-cancelled = Der Aufruf der Registry wurde abgebrochen.

d-as6a7tpg = Namensräume beantragen

d-3p63wowc = Status prüfen

d-32nhaxk2 = Fenster der App-Veröffentlichung
    .w-that-didn-t-work = Das hat nicht funktioniert

d-l2p3ionf = Schlüsselbereich
    .w-signing-key = Signaturschlüssel
    .w-public-key = Öffentlicher Schlüssel
    .w-proof-line = Nachweiszeile
    .w-publish-this-line-where-only-you = Veröffentliche diese Zeile dort, wo nur du schreiben kannst, um einen Namensraum zu belegen.
    .w-copy-proof-line = Nachweiszeile kopieren
    .w-copy-key-for-ci = Schlüssel für CI kopieren
    .w-export-key = Schlüssel exportieren…
    .w-check-application = Antrag prüfen
    .w-this-computer-has-no-publisher-key = Dieser Rechner hat noch keinen Publisher-Schlüssel. Er signiert deine Apps und deine Anfragen an die Registry.
    .w-create-key = Schlüssel anlegen
    .w-the-publisher-key-can-t-be = Der Publisher-Schlüssel lässt sich nicht lesen
    .w-publisher-key-json-is-not-a = publisher-key.json ist kein gültiger Publisher-Schlüssel. Stelle ihn aus einer Sicherung wieder her oder lege ihn beiseite und lege einen neuen an.
    .w-reading-this-computer-s-key = Der Schlüssel dieses Rechners wird gelesen…

d-osoofaou = Orte des Nachweises
    .w-where-to-publish-the-proof = Wo der Nachweis zu veröffentlichen ist
    .w-name-a-domain-or-a-forge = Nenne eine Domain oder ein Forge-Konto, um zu sehen, wohin der Nachweis gehört.
    .w-put-the-proof-line-where-only = Lege die Nachweiszeile dort ab, wo nur du schreiben kannst, und beantrage dann.
    .w-publish-at = Veröffentlichen bei

d-p2cd4gqi = Namensräume
    .w-namespaces = Namensräume
    .w-claim-the-domains-and-forge-accounts = Belege die Domains und Forge-Konten, deren Apps du veröffentlichst.
    .w-domain = Domain
    .w-example-com = example.com
    .w-proved-with-a-dns-txt-record = Belegt mit einem DNS-TXT-Eintrag oder einer .well-known-Datei auf der Domain.
    .w-forge-account = Forge-Konto
    .w-github-ada = github:ada
    .w-proved-with-a-tilo-publisher-repository = Belegt mit einem tilo-publisher-Repository auf diesem Konto.
    .w-contact = Kontakt
    .w-you-example-com = du@example.com
    .w-how-the-registry-s-owner-can = Wie der Betreiber der Registry dich zum Antrag erreicht.
    .w-registry = Registry
    .w-https-apps-tilo-so = https://apps.tilo.so
    .w-a-registry-is-reached-over-credential = Eine Registry wird über zugangsfreies https erreicht oder über reines http allein auf diesem Rechner.

d-66hkefo7 = Beantragen-Zeile
    .w-apply = Beantragen
    .w-waiting-for-the-registry = Die Registry wird gefragt…

d-kn4pythb = Statusbereich
    .w-application-status = Antragsstatus
    .w-no-application-status-has-been-checked = In diesem Fenster wurde noch kein Antragsstatus geprüft.
