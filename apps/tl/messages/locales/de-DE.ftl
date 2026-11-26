# Messages auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-messages = Nachrichten

d-so-tilo-messages-keywords = messaging, direct messages, dm, imessage, sms, text messages, whatsapp, signal, nachrichten, kurznachrichten, direktnachrichten, textnachrichten, mitteilungen

d-tahyni4s = Nachrichten
    .doc = Was die App für den Arbeitsbereich hält: die Unterhaltungen, die ihr zum Einreihen übergeben wurden, das Fenster jeder Person und ihre ungesendeten Worte.
    .noun = { $count ->
        [one] Nachricht
       *[other] Nachrichten
    }

d-d4kfrqic = Unterhaltungen

d-4cxg5hns = Sitzungen

d-exvbnjzv = Entwürfe

d-zbjkkwin = Unterhaltung
    .doc = Eine Unterhaltung, die ein Agent, eine Automatisierung oder eine andere App Nachrichten übergeben hat: was in sie zugestellt und was in ihr eingereiht wurde, in dieser Kopie gehalten, bis ein Gerät, das das Konto hält, sie sendet.
    .noun = { $count ->
        [one] Unterhaltung
       *[other] Unterhaltungen
    }
    .untitled = Unterhaltung

d-o5klbtp5 = Mit
    .doc = Mit wem sie ist, so wie die Person, die sie begann, das Gegenüber nannte: ein Name oder eine Telefonnummer.

d-6nzp6ev7 = Nachrichten

d-ql56ktum = Nachricht
    .doc = Eine Nachricht einer gehaltenen Unterhaltung.
    .noun = { $count ->
        [one] Nachricht
       *[other] Nachrichten
    }

d-nt6hshdj = Von
    .doc = Wer sie gesendet hat, bei einer eingegangenen Nachricht; leer bei einer hier geschriebenen.

d-t5kmlxh5 = An
    .doc = Für wen sie ist, wenn das gesagt wurde.

d-nruhubob = Text

d-rp4u57l3 = Zeitpunkt
    .doc = Wann sie gesendet wurde.

d-mnk6zpch = Ausgehend
    .doc = Hier geschrieben statt empfangen.

d-dvtrviir = Eingereiht
    .doc = Wartet auf ein Gerät, das das Konto hält, um sie zu senden.

d-ql56ktum-filter-at = Zeitpunkt

d-ql56ktum-filter-outgoing = Ausgehend

d-ql56ktum-filter-queued = Eingereiht

d-7f2objzu = Sitzung
    .doc = Das Fenster einer Person auf Nachrichten: mit wem sie schreibt, auf welchem Konto, und die Arbeit des Geräts, auf die ihr Fenster wartet.
    .noun = { $count ->
        [one] Sitzung
       *[other] Sitzungen
    }

d-64vye3u2 = Mitglied
    .doc = Das Mitglied des Arbeitsbereichs, mit dem die Unterhaltung ist.

d-altflp3s = Kontakt
    .doc = Die Person des Adressbuchs, mit der die Unterhaltung ist, nach der Id des Verzeichnisses.

d-5svaj5dt = Gehalten
    .doc = Die gehaltene Unterhaltung, offen, nach ihrem Schlüssel.

d-6hidqj4x = Möglichkeit
    .doc = Die Art, wie die Person ihnen schreibt: eine `Choice` als JSON, leer bis eine gewählt ist.

d-e3jgy6gd = Bereich
    .doc = Was in der Inhaltsspalte statt einer Unterhaltung steht: `setup` oder `groups`.

d-gsoaithd = Status
    .doc = Was das Gerät zuletzt über die Konten der Person sagte (`state`), als JSON.

d-6gv6lwk2 = Seiten
    .doc = Die Seiten der offenen Unterhaltung, nach ihren Aufträgen: die neueste zuerst.

d-xv6bzpv2 = Sendeauftrag
    .doc = Das Senden unterwegs, nach seinem Auftrag, und die Operation, die es ist: ein zweiter Versuch derselben Operation wird vom Dienst abgelehnt, damit ein Senden nie zweimal geht.

d-lle6k7pa = Operation

d-i2oqsqp2 = Abgelehnt
    .doc = Warum das letzte Senden nicht ging, in den Worten des Dienstes; leer nach einem, das ging.

d-tyhiuwht = Einrichtungsdienst
    .doc = Das Konto, das mit diesem Gerät verknüpft wird, und der Auftrag, der es verknüpft.

d-42fshvic = Einrichtungskonto

d-kfuauj6j = Verknüpfungsauftrag

d-i4mfm4vt = Verknüpfter Auftrag

d-lwishiq4 = Erster Dienst
    .doc = Das erste Konto, das eingerichtet wird: sein Dienst, der Auftrag, der das Gerät um den Schritt bittet, und was das Gerät zur Prüfung vorgelegt hat.

d-4wfkxuw6 = Erster Auftrag

d-2vy47h63 = Erste Prüfung

d-2aj4gyha = Zuerst geprüft

d-rsmgxfcl = SIM-Auftrag

d-us76a33i = Gruppenaufträge
    .doc = Die aufgeführten Gruppen, je Konto, und die offene Gruppe.

d-c4pjcl4k = Gruppendienst

d-3spju4b7 = Gruppenkonto

d-nfxe55p6 = Gruppe

d-rop6cvmu = Gruppenauftrag

d-iyvoiiqn = Gruppenverlaufsauftrag

d-wgpkwpj4 = Gruppenänderungsauftrag

d-7f2objzu-filter-member = Mitglied

d-7f2objzu-filter-first_reviewed = Zuerst geprüft

d-bgshgsj5 = Entwurf
    .doc = Was eine Person jemandem schreibt und nicht gesendet hat, gehalten je Art, ihnen zu schreiben: jeder Dienst und jedes Konto hält seine eigenen Worte.
    .noun = { $count ->
        [one] Entwurf
       *[other] Entwürfe
    }

d-aybwzt7u = Route
    .doc = Die Art, wie geschrieben wird: der Schlüssel der Möglichkeit.

d-a3uz6jbp = Person
    .doc = Wem geschrieben wird, so wie das Fenster die Person nennt.

d-acozkvln = Text

d-mzyoczaf = Möglichkeit
    .doc = Eine Art, einer Person zu schreiben: der Dienst, das Konto, von dem es geht, und die Adresse, unter der sie erreicht wird, mit der Bestätigung des Verzeichnisses für diese Adresse.
    .noun = { $count ->
        [one] Möglichkeit
       *[other] Möglichkeiten
    }

d-rfd2qg43 = Dienst

d-oxzt327q = Konto

d-v7u7y7nq = Empfänger

d-vqrybmql = Zuordnung

d-ol5py7y7 = Umfang

d-tzteiyz7 = Revision

d-lrtz2rsz = Route
    .doc = Was dem Messagingdienst für eine Unterhaltung übergeben wird: die Person, die bestätigte Adresse und das Konto, von dem geschrieben wird.
    .noun = { $count ->
        [one] Route
       *[other] Routen
    }

d-74tpcwf2 = Person

d-d7hpucss = Zuordnung

d-oflypcxo = Absender

d-gf3dojii = Dienst

d-j2fc3o45 = Umfang

d-5ygc5vxy = Empfänger

d-l4txcjcv = Revision

d-pg3qz7i6 = Dienstkonto
    .doc = Ein Konto der Person bei einem Messagingdienst, so wie der Dienst es nennt.
    .noun = { $count ->
        [one] Dienstkonto
       *[other] Dienstkonten
    }

d-v4ve5zp3 = Dienst

d-4i7ltdap = Konto

d-hhf4xccq = Statusabfrage
    .doc = Was das Gerät zu den Konten der Person gefragt wird.
    .noun = { $count ->
        [one] Statusabfrage
       *[other] Statusabfragen
    }

d-pdkvne4x = Konten

d-2iofycek = Routen

d-jf4rsi2m = Status
    .doc = Was das Gerät über die Konten der Person sagt: ob der Messagingdienst hier läuft, ob es erste Konten und SIMs einrichtet, und der Zustand jedes Kontos.
    .noun = { $count ->
        [one] Status
       *[other] Status
    }

d-dniqbt5e = Verfügbar

d-rkhxiw72 = Erstes

d-d7nux2dc = SIMs

d-usjfsaru = Akteur

d-g2y43jzp = Konten

d-pomcmapg = Routen

d-jf4rsi2m-filter-available = Verfügbar

d-jf4rsi2m-filter-first = Erstes

d-jf4rsi2m-filter-sims = SIMs

d-lbfrqiku = Kontostatus
    .noun = { $count ->
        [one] Kontostatus
       *[other] Kontostatus
    }

d-frao4txq = Dienst

d-7vdgogxq = Konto

d-xsg2m3md = Zustand

d-tg2cojwc = Phase

d-wbmuc3fg = Verknüpft

d-nvxrtbea = Verknüpfbar

d-e77wla5l = Aufwachen (ms)

d-v2grsi2x = Gruppen

d-lbfrqiku-filter-linked = Verknüpft

d-lbfrqiku-filter-linkable = Verknüpfbar

d-lbfrqiku-filter-wake_ms = Aufwachen (ms)

d-lbfrqiku-filter-groups = Gruppen

d-hsgxzwwb = Seite
    .doc = Eine Seite eines Verlaufs: ihre Nachrichten, älteste zuerst, und ob ältere bleiben.
    .noun = { $count ->
        [one] Seite
       *[other] Seiten
    }

d-pqpwucqf = Nachrichten

d-iblskryi = Mehr

d-hsgxzwwb-filter-more = Mehr

d-usk2vces = Zeile
    .doc = Eine Nachricht aus dem Verlauf eines Dienstes.
    .noun = { $count ->
        [one] Zeile
       *[other] Zeilen
    }

d-pvfqhsdt = Text

d-huhftznw = Eingehend

d-kk4ampgc = Zeit in ms

d-zpiv6gsh = Zeitpunkt

d-vh4gz425 = Zustellung

d-gj2fnjgl = Autor

d-usk2vces-filter-incoming = Eingehend

d-usk2vces-filter-at_ms = Zeit in ms

d-usk2vces-filter-at = Zeitpunkt

d-lhcrc2tw = Verlaufsabfrage
    .noun = { $count ->
        [one] Verlaufsabfrage
       *[other] Verlaufsabfragen
    }

d-nhqaztso = Route

d-rlz74ph7 = Vor

d-lhcrc2tw-filter-route = Route

d-76iahkx4 = Sendeabfrage
    .noun = { $count ->
        [one] Sendeabfrage
       *[other] Sendeabfragen
    }

d-rrcbc4yw = Route

d-sqcvn4gy = Operation

d-olsvmcv6 = Text

d-76iahkx4-filter-route = Route

d-6kveavu5 = Gesendet
    .doc = Was der Auftrag eines Sendens sagt, sobald der Dienst es angenommen hat.
    .noun = { $count ->
        [one] Gesendet
       *[other] Gesendet
    }

d-cnunwsvo = Operation

d-ypegxfwu = Status

d-mp3kxrii = Grund
    .doc = Warum ein Auftrag ohne seine Antwort endete, in den Worten des Dienstes.
    .noun = { $count ->
        [one] Grund
       *[other] Gründe
    }

d-6nkw6omo = Grund

d-guuhxptm = Verknüpfung
    .doc = Der private Code, den ein Telefon scannt, um ein Konto mit diesem Gerät zu verknüpfen.
    .noun = { $count ->
        [one] Verknüpfung
       *[other] Verknüpfungen
    }

d-7lnh6aj4 = Dienst

d-bmyylvsv = Konto

d-kyhjtto5 = Versuch

d-xjvdi5zy = Generation

d-5o224mqj = Code

d-guuhxptm-filter-generation = Generation

d-qh35e7ph = Abschlussabfrage
    .noun = { $count ->
        [one] Abschlussabfrage
       *[other] Abschlussabfragen
    }

d-vilm5o5d = Dienst

d-t2lovwwx = Konto

d-zv4b637n = Versuch

d-tulwila2 = Prüfung
    .doc = Was die Einrichtung eines ersten Kontos die Person zu prüfen bittet: das Konto, seinen Umfang und die Nummer, die sein Dienst meldet, sobald es verknüpft ist.
    .noun = { $count ->
        [one] Prüfung
       *[other] Prüfungen
    }

d-lvoxnczs = Dienst

d-5gfxct5s = Konto

d-75o4ih6j = Umfang

d-2ctemzh2 = Nummer

d-va6uya4b = Prüfabfrage
    .noun = { $count ->
        [one] Prüfabfrage
       *[other] Prüfabfragen
    }

d-jfml6xf6 = Dienst

d-krfp76j4 = Konto

d-e2ham4du = Anzeigename

d-b7kqz7bp = Dienstabfrage
    .noun = { $count ->
        [one] Dienstabfrage
       *[other] Dienstabfragen
    }

d-k3oliiji = Dienst

d-vhjgcni7 = SIM-Abfrage
    .noun = { $count ->
        [one] SIM-Abfrage
       *[other] SIM-Abfragen
    }

d-lo6z7642 = Auswahl

d-o3hpo5hy = SIMs
    .noun = { $count ->
        [one] SIM
       *[other] SIMs
    }

d-6wsvv3nu = Möglichkeiten

d-glltfbwu = SIM
    .noun = { $count ->
        [one] SIM
       *[other] SIMs
    }

d-7wyloxzb = Auswahl

d-hdpjwkop = Bezeichnung

d-f77zdb7a = Gruppen
    .doc = Die Gruppen eines Kontos.
    .noun = { $count ->
        [one] Gruppe
       *[other] Gruppen
    }

d-noskctim = Dienst

d-mawv33bh = Konto

d-y3gto3of = Gruppen

d-op4nyp6z = Gruppe
    .noun = { $count ->
        [one] Gruppe
       *[other] Gruppen
    }

d-tm4dpnae = Titel

d-ds73nju5 = Gruppenabfrage
    .noun = { $count ->
        [one] Gruppenabfrage
       *[other] Gruppenabfragen
    }

d-2b6c5ims = Dienst

d-q4e63vzy = Konto

d-dtf6fkp2 = Gruppe

d-pghxrpmc = Mitgliedschaft
    .doc = Wer in einer Gruppe ist, so wie das Konto es zuletzt las, und die Prüfung, die eine Änderung oder ein Senden nennt.
    .noun = { $count ->
        [one] Mitgliedschaft
       *[other] Mitgliedschaften
    }

d-kck6cfa7 = Dienst

d-47ijz4ut = Konto

d-hmqqxxxy = Gruppe

d-xoufu7ye = Titel

d-eitw3kr4 = Ankündigung

d-bmsbyoyf = Eigenes Mitglied

d-ip7c3zwj = Prüfung

d-72lgn4ki = Mitglieder

d-pghxrpmc-filter-announcement = Ankündigung

d-gbnaqeh5 = Mitglied
    .noun = { $count ->
        [one] Mitglied
       *[other] Mitglieder
    }

d-gj3eyyfa = Adresse

d-jn4pfyeq = Rolle

d-ygjk6k2d = Gruppenverlauf
    .noun = { $count ->
        [one] Gruppenverlauf
       *[other] Gruppenverläufe
    }

d-nfkcxiy3 = Gruppe

d-ployixf5 = Nachrichten

d-k6yezpls = Gruppen-Sendeabfrage
    .noun = { $count ->
        [one] Gruppen-Sendeabfrage
       *[other] Gruppen-Sendeabfragen
    }

d-ulautvec = Dienst

d-3dwavnj6 = Konto

d-kdnxlkfb = Gruppe

d-fynuz3w6 = Prüfung

d-aruyfh3d = Operation

d-qm4oc5h4 = Text

d-cxnamcs3 = Gruppenänderungsabfrage
    .noun = { $count ->
        [one] Gruppenänderungsabfrage
       *[other] Gruppenänderungsabfragen
    }

d-hknm2z4z = Dienst

d-inc5oyyo = Konto

d-hobp7hdg = Gruppe

d-6eo6mjrt = Prüfung

d-ts4wfecr = Operation

d-rc46qv6u = Änderung

d-npxj6chw = Mitglied

d-sqrjv2e2 = Route

d-cxnamcs3-filter-route = Route

d-mto4u6cl = Gruppe geändert
    .noun = { $count ->
        [one] Gruppe geändert
       *[other] Gruppen geändert
    }

d-kcoj7dba = Mitgliedschaft

d-mto4u6cl-filter-membership = Mitgliedschaft

d-r4viqatk = Gruppe gesendet
    .noun = { $count ->
        [one] Gruppe gesendet
       *[other] Gruppen gesendet
    }

d-uygmvqle = Verlauf

d-263s4dxh = Mitgliedschaft

d-r4viqatk-filter-history = Verlauf

d-r4viqatk-filter-membership = Mitgliedschaft

d-juxv345q = Fund
    .doc = Eine Nachricht, die ein Agent gefunden oder exportiert, mit ihrem Ort.
    .noun = { $count ->
        [one] Fund
       *[other] Funde
    }

d-i2dytl6w = Unterhaltung

d-upz4hvew = Mit

d-es4f4py5 = Von

d-llqczpvg = An

d-xyh7ulvd = Text

d-qnoqo2pa = Datum

d-n7bynm5r = Zeitpunkt

d-c3jhumi3 = Ausgehend

d-4bso5lez = Eingereiht

d-juxv345q-filter-at = Zeitpunkt

d-juxv345q-filter-outgoing = Ausgehend

d-juxv345q-filter-queued = Eingereiht

d-wfk2tb7v = Kontakte öffnen
    .doc = Öffnet Kontakte, wo die Personen hinzugefügt werden, denen man Nachrichten schreibt.

d-4ouumkke = Status aktualisieren
    .doc = Fragt das Gerät, wie die Konten der Person stehen, und hält, was es sagte, wenn es sich änderte: die Uhr des Fensters lässt das laufen, während es gezeigt wird.

d-m52zvijk = Status der Messagingkonten auflisten
    .doc = Jedes Messagingkonto und wie es steht: was ein Agent liest, bevor er schreibt.

d-g2ejlkad = Person öffnen
    .doc = Öffnet die Unterhaltung mit einem Mitglied des Arbeitsbereichs, auf Tilo.

d-g2ejlkad-who = Wer

d-nvahsz3v = Person öffnen
    .doc = Öffnet die Unterhaltung mit einer Person des Adressbuchs, auf der ersten Art, ihr zu schreiben, die sie erreicht.
    .w-this-contact-is-unavailable = Dieser Kontakt ist nicht verfügbar.

d-nvahsz3v-person = Person

d-3hjipmdb = Route wählen
    .doc = Schreibt der offenen Person auf eine andere Art: ein Dienst, ein Konto und eine Adresse aus den Möglichkeiten, die das Fenster aufführt.
    .w-choose-a-person = Person wählen
    .w-this-service-address-has-conflicting-contact = Diese Dienstadresse hat widersprüchliche Kontaktzuordnungen. Prüfe sie in Kontakte.
    .w-this-contact-is-unavailable = Dieser Kontakt ist nicht verfügbar.

d-3hjipmdb-choice = Möglichkeit

d-cbureeub = Zurück zu Personen
    .doc = Zurück zur Liste der Personen.
    .w-the-window-shows-the-people-already = Das Fenster zeigt die Personen schon

d-mtv63zl6 = Personen auflisten
    .doc = Wem das Fenster schreiben kann: zuerst die Mitglieder des Arbeitsbereichs, dann die Personen des Adressbuchs.

d-gjp2exty = Möglichkeiten auflisten
    .doc = Die Arten, einer Person des Adressbuchs zu schreiben, jede so, wie das Fenster sie hält.

d-gjp2exty-person = Person

d-tnycbqc2 = Konto prüfen
    .doc = Bestätigt die Adresse, die eine Möglichkeit als die der offenen Person nennt, auf dem Konto, von dem geschrieben wird: das `confirm-account` des Verzeichnisses.
    .did = hat ein Konto mit einem Kontakt verknüpft
    .w-a-verified-service-identity-is-required = Eine verifizierte Dienstidentität ist nötig, bevor dieser Kontakt verbunden werden kann.
    .w-not-configured = Nicht eingerichtet
    .w-this-contact-is-unavailable = Dieser Kontakt ist nicht verfügbar.

d-tnycbqc2-person = Person

d-tnycbqc2-choice = Möglichkeit

d-5flhsei5 = Nachrichten aktualisieren
    .doc = Liest die neueste Seite der offenen Unterhaltung erneut.
    .w-choose-a-service-and-account-before = Wähle einen Dienst und ein Konto, bevor du eine Nachricht schreibst.

d-rqsqaeup = Ältere Nachrichten laden
    .doc = Liest die Seite vor der ältesten gezeigten.
    .w-choose-a-service-and-account-before = Vor dem Schreiben einer Nachricht einen Dienst und ein Konto wählen.
    .w-this-is-the-start-of-the = Dies ist der Anfang der Unterhaltung.

d-4szcuo2q = Entwurf halten
    .doc = Hält einen Entwurf für die Art zu schreiben, die das Fenster offen hat, damit die Eingabe Worte zum Binden hat.

d-4szcuo2q-route = Route

d-4szcuo2q-person = Person

d-6milpwgc = Entwurf verwerfen
    .doc = Verwirft die Worte, die für eine Art zu schreiben gehalten werden.

d-6milpwgc-d = D

d-vzxijzpu = Nachricht senden
    .doc = Sendet, was der Entwurf hält, auf der Art, die das Fenster offen hat: an ein Mitglied über Chat, an alle anderen über den Messagingdienst des Geräts, als eine Operation, die der Dienst nie zweimal sendet.
    .did = hat eine Nachricht gesendet
    .w-enter-a-message-and-select-a = Eine Nachricht eingeben und eine aktuelle Unterhaltung auswählen.
    .w-this-send-was-already-attempted-check = Dieses Senden wurde bereits versucht. Prüfe seinen Status in der Unterhaltung.
    .w-choose-a-service-and-account-before = Vor dem Schreiben einer Nachricht einen Dienst und ein Konto wählen.

d-vzxijzpu-d = D

d-4ecwv7l7 = Den zugestellten Entwurf entfernen
    .doc = Ein Senden endete: die Worte verlassen den Entwurf, sobald der Dienst sie genommen hat, und die Unterhaltung wird neu gelesen; eine Ablehnung bleibt in den Worten des Dienstes im Fenster.

d-4ecwv7l7-d = D

d-4ecwv7l7-j = J

d-rn5ejfpf = Nachricht zustellen
    .doc = Hält eine Nachricht mit `from` als Absender, in der Unterhaltung, die sie nennt, oder in seiner eigenen.
    .did = hat eine Nachricht zugestellt
    .w-name-who-sent-it = Nenne, wer sie gesendet hat
    .w-a-message-says-something = Eine Nachricht sagt etwas
    .w-is-not-a-date-write-2026 = `{ $date }` ist kein Datum: schreibe 2026-10-11T09:30:00Z
    .w-names-no-one-a-conversation-can = `{ $named_or }` nennt niemanden, mit dem eine Unterhaltung sein kann

d-rn5ejfpf-from = Von

d-rn5ejfpf-body = Text

d-rn5ejfpf-to = An

d-rn5ejfpf-date = Datum

d-rn5ejfpf-conversation = Unterhaltung

d-kviwa7dq = Nachricht senden
    .doc = Reiht eine Nachricht an jemanden ein, in der Unterhaltung, die sie nennt, oder in seiner, damit ein Gerät, das das Konto hält, sie sendet.
    .did = hat eine Nachricht eingereiht
    .w-names-no-one-to-write-to = `{ $to }` nennt niemanden, dem geschrieben werden kann
    .w-a-message-says-something = Eine Nachricht sagt etwas

d-kviwa7dq-to = An

d-kviwa7dq-body = Text

d-kviwa7dq-conversation = Unterhaltung

d-qwblr2u6 = Antworten
    .doc = Reiht eine Antwort in einer gehaltenen Unterhaltung ein.
    .did = hat eine Antwort eingereiht
    .w-is-not-a-conversation-id = `{ $conversation }` ist keine Unterhaltungs-Id
    .w-a-message-says-something = Eine Nachricht sagt etwas
    .w-no-conversation-send-message-starts-one = keine Unterhaltung `{ $conversation }`: `send_message` beginnt eine

d-qwblr2u6-conversation = Unterhaltung

d-qwblr2u6-body = Text

d-3db4glre = Unterhaltung öffnen
    .doc = Öffnet eine gehaltene Unterhaltung.

d-3db4glre-c = C

d-7xltzdqb = Nachrichten durchsuchen
    .doc = Die gehaltenen Nachrichten, die jedes Wort von `query` enthalten, neueste zuerst, höchstens 200.
    .w-search-for-some-words = nach einigen Worten suchen

d-7xltzdqb-query = Abfrage

d-tuuwoq7b = Markdown exportieren
    .doc = Die gehaltenen Unterhaltungen, oder die genannte, als Markdown.
    .export = Markdown

d-refjsrjy = Unterhaltungen exportieren
    .doc = Die gehaltenen Unterhaltungen, oder die genannte, als Markdown (`md`) oder JSON (`json`).
    .w-no-conversation = keine Unterhaltung `{ $conversation }`
    .w-is-not-a-format-md-or = `{ $format }` ist kein Format: md oder json

d-refjsrjy-format = Format

d-refjsrjy-conversation = Unterhaltung

d-qgkk2ueu = Konten
    .doc = Öffnet die Kontoeinrichtung in der Inhaltsspalte.

d-u34kt4cp = Konto wählen
    .doc = Wählt, welches der Konten der Person dieses Gerät verknüpft.
    .w-private-linking-is-unavailable-for-this = Private Verknüpfung ist für dieses Konto auf diesem Gerät nicht verfügbar.
    .w-this-account-already-has-a-linking = Dieses Konto hat bereits einen Verknüpfungsversuch. Schließe ihn ab oder beende ihn, bevor ein weiterer beginnt.
    .w-open-the-account-setup-first = Öffne zuerst die Kontoeinrichtung

d-u34kt4cp-service = Dienst

d-u34kt4cp-account = Konto

d-6nrs5xav = Dieses Gerät verknüpfen
    .doc = Bittet den Dienst um den privaten Code, den ein Telefon scannt, um das gewählte Konto hier zu verknüpfen.
    .w-select-the-service-and-account-to = Wähle den Dienst und das Konto, die auf diesem Gerät verknüpft werden sollen.
    .w-this-account-already-has-a-linking = Dieses Konto hat bereits einen Verknüpfungsversuch. Schließe ihn ab oder beende ihn, bevor ein weiterer beginnt.
    .w-open-the-account-setup-first = Öffne zuerst die Kontoeinrichtung

d-e3gsy5gl = Verknüpfung abschließen
    .doc = Schließt die Verknüpfung ab, sobald das Telefon den Code gescannt hat.
    .w-linking-stopped-return-to-account-selection = Die Verknüpfung wurde beendet. Zurück zur Kontenauswahl, um einen neuen Versuch zu beginnen.
    .w-open-the-account-setup-first = Öffne zuerst die Kontoeinrichtung

d-z5rlda62 = Einrichtung schließen
    .doc = Schließt die Einrichtung und beendet damit einen Verknüpfungsversuch, der nicht abgeschlossen wurde.
    .w-open-the-account-setup-first = Öffne zuerst die Kontoeinrichtung

d-wccjxxqn = Ein privates Nachrichten-Konto einrichten
    .doc = Beginnt ein erstes Konto auf `service`: WhatsApp bittet zuerst, seinen geschützten Speicher zu prüfen; Signal zeigt seinen Code sofort.
    .w-private-linking-is-unavailable-for-this = Private Verknüpfung ist für dieses Konto auf diesem Gerät nicht verfügbar.
    .w-this-account-already-has-a-linking = Dieses Konto hat bereits einen Verknüpfungsversuch. Schließe ihn ab oder beende ihn, bevor ein weiterer beginnt.
    .w-messaging-isn-t-running-here-so = Messaging läuft hier nicht, daher kann kein Konto verknüpft werden.

d-wccjxxqn-service = Dienst

d-y2kw3j5m = Verknüpfung abschließen
    .doc = Schließt die Verknüpfung des ersten Kontos ab, sobald sein Code gescannt wurde.
    .w-linking-stopped-return-to-account-selection = Die Verknüpfung wurde beendet. Zurück zur Kontenauswahl, um einen neuen Versuch zu beginnen.
    .w-open-the-account-setup-first = Öffne zuerst die Kontoeinrichtung

d-m6oimxe2 = Identität bestätigen
    .doc = Bestätigt, was das Gerät zur Prüfung vorgelegt hat: den geschützten Speicher von WhatsApp, dann die Identität, die der Dienst meldet.
    .w-linking-stopped-return-to-account-selection = Die Verknüpfung wurde beendet. Zurück zur Kontenauswahl, um einen neuen Versuch zu beginnen.
    .w-open-the-account-setup-first = Öffne zuerst die Kontoeinrichtung

d-pv25uk4m = SIM für SMS wählen
    .doc = Führt die SIMs des Geräts auf, die ein SMS-Konto nutzen kann.
    .w-this-account-already-has-a-linking = Dieses Konto hat bereits einen Verknüpfungsversuch. Schließe ihn ab oder beende ihn, bevor ein weiterer beginnt.
    .w-this-device-has-no-sim-to = Dieses Gerät hat keine SIM, um SMS zu senden.

d-pem7jjwu = Diese SIM verwenden
    .doc = Verwendet eine der für ein SMS-Konto aufgeführten SIMs.
    .w-this-device-has-no-sim-to = Dieses Gerät hat keine SIM, um SMS zu senden.

d-pem7jjwu-selection = Auswahl

d-4sw6rh6p = Erster Schritt
    .doc = Ein Schritt der Einrichtung des ersten Kontos endete: was er zu prüfen bittet, wird gehalten, und ein Fehler lässt die Einrichtung unterbrochen.

d-4sw6rh6p-op = Op

d-4sw6rh6p-j = J

d-evnplmns = Zurück zur Kontenauswahl
    .doc = Zurück zur Wahl eines Dienstes, nachdem eine Einrichtung gestoppt hat: das Gerät vergisst den Versuch.
    .w-messaging-isn-t-running-here-so = Messaging läuft hier nicht, daher kann kein Konto verknüpft werden.

d-yt5oiw4j = Private Gruppen auflisten
    .doc = Öffnet die privaten Gruppen der WhatsApp-Konten der Person.

d-jliatnk2 = Gruppen schließen
    .doc = Schließt die Gruppen.
    .w-open-the-private-groups-first = Öffne zuerst die privaten Gruppen

d-h65q42xz = Gruppenmitgliedschaft lesen
    .doc = Öffnet eine Gruppe: wer darin ist, und was dort gesagt wurde.
    .w-open-the-private-groups-first = Öffne zuerst die privaten Gruppen

d-h65q42xz-service = Dienst

d-h65q42xz-account = Konto

d-h65q42xz-group = Gruppe

d-cf2x72ld = Gruppennachrichten lesen
    .doc = Liest die Nachrichten der offenen Gruppe erneut.
    .w-group-membership-is-unavailable-for-this = Die Gruppenmitgliedschaft ist für dieses Konto nicht verfügbar.
    .w-open-the-private-groups-first = Öffne zuerst die privaten Gruppen

d-zvk2i6qw = An diese Gruppe senden
    .doc = Sendet, was der Entwurf der Gruppe hält, an die offene Gruppe.
    .did = hat an eine Gruppe gesendet
    .w-group-membership-is-unavailable-for-this = Die Gruppenmitgliedschaft ist für dieses Konto nicht verfügbar.
    .w-enter-a-message-and-select-a = Eine Nachricht eingeben und eine aktuelle Unterhaltung auswählen.

d-zvk2i6qw-d = D

d-clj7rp5x = Gruppe gesendet

d-clj7rp5x-d = D

d-clj7rp5x-j = J

d-zr5urj45 = Geprüften Kontakt hinzufügen
    .doc = Ändert, wer in der offenen Gruppe ist: fügt einen geprüften Kontakt über eine Route zu ihm hinzu, entfernt ein Mitglied oder verlässt die Gruppe.
    .w-group-membership-is-unavailable-for-this = Die Gruppenmitgliedschaft ist für dieses Konto nicht verfügbar.
    .w-no-group-change-is-called = keine Gruppenänderung heißt `{ $change }`
    .w-a-verified-service-identity-is-required = Eine verifizierte Dienstidentität ist nötig, bevor dieser Kontakt verbunden werden kann.
    .w-open-the-private-groups-first = Öffne zuerst die privaten Gruppen

d-zr5urj45-change = Änderung

d-zr5urj45-member = Mitglied

d-zr5urj45-choice = Möglichkeit

d-r445vu5i = Dienstname
    .w-whatsapp = WhatsApp
    .w-signal = Signal
    .w-sms = SMS
    .w-tilo = Tilo

d-sxkllhst = Kontoname
    .w-not-configured = Nicht eingerichtet

d-eavkjm2x = Bezeichnung der Möglichkeit
    .w-tilo = Tilo
    .w-from = { $service_name } von { $account_name }
    .w-from-to = { $service_name } von { $account_name } an { $recipient }

d-rjkuq7tt = Worte zum Zustand
    .w-add-this-service-account-in-settings = Füge dieses Dienstkonto in den Einstellungen hinzu, um es zu verbinden.
    .w-this-account-s-messaging-service-is = Der Messagingdienst dieses Kontos ist nicht verfügbar. Prüfe seine Einrichtung in den Einstellungen.
    .w-linking-this-account-confirm-the-request = Dieses Konto wird verknüpft… Bestätige die Anfrage auf deinem Telefon.
    .w-unlock-this-account-to-use-messaging = Entsperre dieses Konto, um Messaging zu nutzen.
    .w-this-account-is-offline-your-conversation = Dieses Konto ist offline. Die Unterhaltung wird fortgesetzt, sobald es sich wieder verbindet.
    .w-account-ready = Konto bereit
    .w-link-this-account-again-to-resume = Verknüpfe dieses Konto erneut, um Messaging fortzusetzen.
    .w-this-account-s-connection-stopped-check = Die Verbindung dieses Kontos wurde beendet. Prüfe seine Verbindung und Kontoeinrichtung.
    .w-connection-status-is-unavailable = Der Verbindungsstatus ist nicht verfügbar.

d-tfequwhn = Worte zur Ablehnung
    .w-the-outcome-could-not-be-confirmed = Das Ergebnis konnte nicht bestätigt werden. Prüfe diese Unterhaltung, bevor du erneut sendest.
    .w-this-send-was-already-attempted-check = Dieses Senden wurde bereits versucht. Prüfe seinen Status in der Unterhaltung.
    .w-enter-a-message-and-select-a = Eine Nachricht eingeben und eine aktuelle Unterhaltung auswählen.
    .w-this-service-address-has-conflicting-contact = Diese Dienstadresse hat widersprüchliche Kontaktzuordnungen. Prüfe sie in Kontakte.
    .w-this-computer-is-not-one-of = Dieser Computer ist keines deiner eigenen Tilo-Geräte und sendet daher von keinem deiner Konten. Sende von einem eigenen Gerät.
    .w-messaging-connection-unavailable = Messaging-Verbindung nicht verfügbar

d-bkb5pucf = Nachricht prüfen
    .w-a-sender-or-recipient-is-at = ein Absender oder Empfänger hat höchstens 256 Zeichen
    .w-a-message-is-at-most-16384 = eine Nachricht hat höchstens 16384 Zeichen

d-24giztyf = Worte zu Rollen
    .w-administrator = Administrator
    .w-group-creator = Gruppenersteller
    .w-member = Mitglied

d-nllb6gtg = Person öffnen~present

d-5wr6bt5n = Route wählen~present

d-rw657cqx = Konto prüfen~present

d-2usw5vs3 = Älteren Verlauf lesen~present

d-5mkcb6fx = Senden~present

d-eunwbhp4 = Nachricht zustellen~present

d-bzr4z7ko = Nachricht senden~present

d-cuu3lsm7 = Antworten~present

d-ucieda6n = Nachrichten durchsuchen~present

d-vppayo4f = Inhalt exportieren~present

d-gmd2rgdo = Einrichtungskonto wählen~present

d-g6qkpq7h = Verknüpfung beginnen~present

d-np632ebu = Erstes Konto beginnen~present

d-fsptxrfj = SIMs auflisten~present

d-qjdwlqot = Gruppenverlauf lesen~present

d-udvzamwb = An eine Gruppe senden~present

d-72qlok6o = Gruppe ändern~present

d-l545ybpl = Nachrichtenfenster
    .w-messages = Nachrichten
    .w-accounts = Konten
    .w-private-groups = Private Gruppen
    .w-this-conversation-is-no-longer-here = Diese Unterhaltung ist nicht mehr hier.

d-3ku6ypy7 = Niemand
    .w-no-conversations-yet = Noch keine Unterhaltungen
    .w-add-people-in-contacts-to-start = Füge in Kontakte Personen hinzu, um eine Unterhaltung zu beginnen.
    .w-open-contacts = Kontakte öffnen
    .w-choose-a-person = Person wählen
    .w-your-conversations-stay-separate-for-each = Unterhaltungen bleiben je Dienst und Konto getrennt.

d-xaxp2x74 = Tilo-Unterhaltung
    .w-message = Nachricht an { $name }
    .w-conversation-with = Unterhaltung mit { $name }

d-u7tveux4 = Eingabe von
    .w-attach = Anhängen
    .w-send = Senden
    .w-write-a-message = Nachricht schreiben

d-unfnvjbl = Unterhaltung
    .w-message-using = Nachricht über
    .w-refresh-messages = Nachrichten aktualisieren
    .w-this-way-of-writing-to-is = Dieser Weg, { $label } zu schreiben, ist nicht mehr bestätigt.
    .w-choose-another-way-above-or-review = Wähle oben einen anderen Weg oder prüfe die Adresse in Kontakte.
    .w-not-configured = Nicht eingerichtet
    .w-confirm-a-service-address-for-this = Bestätige eine Dienstadresse für diese Person.
    .w-verify-this-address-belongs-to-the = Stelle sicher, dass diese Adresse zur Person oben gehört. Ähnliche Namen oder Telefonnummern sind kein Beweis.
    .w-this-service-address-has-conflicting-contact = Diese Dienstadresse hat widersprüchliche Kontaktzuordnungen. Prüfe sie in Kontakte.
    .w-this-service-s-message-history-is = Der Nachrichtenverlauf dieses Dienstes ist nicht verfügbar.
    .w-write-a-message = Nachricht schreiben
    .w-loading-messages = Nachrichten werden geladen…
    .w-load-older-messages = Ältere Nachrichten laden
    .w-this-is-the-start-of-the = Dies ist der Anfang der Unterhaltung.
    .w-no-messages-on-this-service-yet = Noch keine Nachrichten über diesen Dienst
    .w-conversation-with = Unterhaltung mit { $label }
    .w-this-contact-is-unavailable = Dieser Kontakt ist nicht verfügbar.

d-nflmjgpn = Gehalten
    .w-kept-in-this-copy-of-messages = In dieser Kopie von Nachrichten gehalten. Hier geschriebene Worte warten auf ein Gerät, das das Konto hält, um sie zu senden.
    .w-conversation-with = Unterhaltung mit { $with }

d-hjoxkzmz = Einrichtung
    .w-accounts = Konten
    .w-your-accounts = Eigene Konten
    .w-device-linked-messaging-resumes-after-its = Gerät verknüpft. Messaging wird fortgesetzt, sobald Verbindung und Posteingang bereit sind.
    .w-linking-stopped-return-to-account-selection = Die Verknüpfung wurde beendet. Zurück zur Kontenauswahl, um einen neuen Versuch zu beginnen.
    .w-scan-this-private-code-from-linked = Scanne diesen privaten Code unter Verknüpfte Geräte auf deinem Telefon und schließe dann die Verknüpfung ab.
    .w-finish-linking = Verknüpfung abschließen
    .w-private-linking-is-unavailable-for-this = Private Verknüpfung ist für dieses Konto auf diesem Gerät nicht verfügbar.
    .w-select-the-service-and-account-to = Wähle den Dienst und das Konto, die auf diesem Gerät verknüpft werden sollen.
    .w-link-this-device = Dieses Gerät verknüpfen
    .w-close-setup = Einrichtung schließen

d-thggyhpr = Privater Code
    .w-private-device-linking-code-for-the = Privater Geräte-Verknüpfungscode für das gewählte Konto

d-ydd2hv7c = Erstes Konto
    .w-linking-stopped-return-to-account-selection = Die Verknüpfung wurde beendet. Zurück zur Kontenauswahl, um einen neuen Versuch zu beginnen.
    .w-return-to-account-selection = Zurück zur Kontenauswahl
    .w-scan-this-private-code-from-linked = Scanne diesen privaten Code unter Verknüpfte Geräte auf deinem Telefon und schließe dann die Verknüpfung ab.
    .w-refresh-private-linking-code = Privaten Verknüpfungscode erneuern
    .w-finish-linking = Verknüpfung abschließen
    .w-device-linked-messaging-resumes-after-its = Gerät verknüpft. Messaging wird fortgesetzt, sobald Verbindung und Posteingang bereit sind.
    .w-review-access-to-whatsapp-s-private = Prüfe den Zugriff auf den privaten verschlüsselten Speicher von WhatsApp und bestätige dann die Identität, die WhatsApp meldet. Textnachrichten und private Gruppentexte funktionieren, auch Mitgliedschaftsänderungen, die du prüfst. Communitys, selbstlöschende Nachrichten, Medien und Anrufe funktionieren nicht.
    .w-verify-the-selected-service-identity = Bestätige die Identität des gewählten Dienstes: { $value }
    .w-show-private-linking-code = Privaten Verknüpfungscode anzeigen
    .w-review-protected-storage-access = Zugriff auf geschützten Speicher prüfen
    .w-confirm-identity = Identität bestätigen
    .w-no-available-sims-were-found-on = Auf diesem Gerät wurden keine verfügbaren SIMs gefunden.
    .w-use = { $label } verwenden
    .w-link-an-account = Konto verknüpfen
    .w-messaging-isn-t-running-here-so = Messaging läuft hier nicht, daher kann kein Konto verknüpft werden. Verknüpfe WhatsApp oder Signal stattdessen auf einem Tilo-Computer.
    .w-link-a-whatsapp-account = WhatsApp-Konto verknüpfen
    .w-scan-a-code-with-whatsapp-on = Code mit WhatsApp auf dem Telefon scannen
    .w-link-a-signal-account = Signal-Konto verknüpfen
    .w-scan-a-code-with-signal-on = Code mit Signal auf dem Telefon scannen
    .w-choose-an-sms-sim = SIM für SMS wählen

d-pctpehrv = Gruppen
    .w-private-groups = Private Gruppen
    .w-close-groups = Gruppen schließen
    .w-these-groups-belong-to-the-selected = Diese Gruppen gehören zum gewählten WhatsApp-Konto, getrennt von den Mitgliedern des Arbeitsbereichs.
    .w-group-membership-is-unavailable-for-this = Die Gruppenmitgliedschaft ist für dieses Konto nicht verfügbar.
    .w-whatsapp-groups-for = WhatsApp-Gruppen für { $account_name }
    .w-no-participating-groups = Keine Gruppen mit Beteiligung.
    .w-untitled-group = Unbenannte Gruppe

d-hs72mqlm = Gruppe
    .w-untitled-group = Unbenannte Gruppe
    .w-remove = { $address } entfernen
    .w-refresh-membership = Mitgliedschaft aktualisieren
    .w-leave-this-group = Diese Gruppe verlassen
    .w-add-reviewed-contact = Geprüften Kontakt hinzufügen
    .w-add-using = { $label } über { $choice_label } hinzufügen
    .w-send-to-this-group = An diese Gruppe senden
    .w-attach = Anhängen
    .w-send = Senden
    .w-write-a-message = Nachricht schreiben
    .w-loading-messages = Nachrichten werden geladen…

d-27e3n3dc = Nachrichten starten
    .doc = Legt den Fensterzustand der Person an, wenn sie Nachrichten zum ersten Mal öffnet.
    .w-messages-is-open-already = Nachrichten ist schon geöffnet

d-4ouumkke-state = Zustand

d-4sw6rh6p-state = Zustand

d-4szcuo2q-state = Zustand

d-e724wgv5 = Schlüssel
    .doc = Der Schlüssel, unter dem Agenten sie nennen: die Telefonnummer oder der Name, mit dem sie geführt wird, wie `send_message` und `deliver_message` ihn zurückgeben.

d-hpfc37is = Schlüssel
    .doc = Die Kennung der Nachricht beim Dienst.

d-lbbnci2x = Geöffnet
    .doc = Der Fensterzustand der Person ab dem Öffnen der App, und wie ihre Konten stehen.

d-yjein2jt = Marke
    .doc = Woran dieselbe zweimal zugestellte Nachricht erkannt wird, damit sie nur einmal bleibt.

d-ymx5azso = Schlüssel
    .doc = Die Kennung der Gruppe beim Dienst.
