# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-api-tester = API-Tester

d-so-tilo-api-tester-keywords = postman, insomnia, rest, http, requests, endpoint, graphql, api, client, debug, test

d-awdfqlwu = Zeile
    .doc = Ein Schlüssel und ein Wert einer Anfrage: ein Abfrageparameter, eine Kopfzeile oder ein Formularfeld.
    .noun = { $count ->
        [one] Zeile
       *[other] Zeilen
    }
    .untitled = Neue Zeile

d-gmr3auev = Schlüssel

d-5i27urp3 = Wert

d-vgdlc7cu = Aktiv
    .doc = Eingeschlossen: eine abgeschaltete Zeile bleibt gelistet und wird nicht gesendet.

d-awdfqlwu-filter-enabled = Aktiv

d-jqblcgrx = Zugangsdaten
    .doc = Womit sich eine Anfrage anmeldet.
    .noun = { $count ->
        [one] Zugang
       *[other] Zugänge
    }

d-wu5jqin3 = Verfahren

d-eimiw54w = Benutzername
    .doc = Der Benutzername der Basisauthentifizierung: öffentlich, wie der Name des Kontos.

d-bq35bust = Kopfzeile
    .doc = Die Kopfzeile, in der ein API-Schlüssel reist.

d-doie7hps = Verweis
    .doc = Der geräteeigene Name des Zugangs.

d-jqblcgrx-filter-scheme = Verfahren

d-qpu6dw3a = Anfrage
    .doc = Eine Anfrage, in einer Sammlung gespeichert oder gerade gebaut.
    .noun = { $count ->
        [one] Anfrage
       *[other] Anfragen
    }
    .untitled = Neue Anfrage

d-zerzgb7o = Name

d-plszv7l4 = Methode

d-2myyqmpp = Adresse

d-d26dsrkb = Parameter

d-sp2vfioq = Kopfzeilen

d-oubruxyi = Anmeldung

d-26dsckeq = Rumpfart

d-ejporjru = Rumpf
    .doc = Der Rumpf, wie getippt: JSON, reiner Text oder eine Vorlage.

d-2koo3omi = Formular

d-qpu6dw3a-filter-method = Methode

d-qpu6dw3a-filter-auth = Anmeldung

d-qpu6dw3a-filter-body_mode = Rumpfart

d-cgnrf3nr = Antwort
    .doc = Was ein Dienst geantwortet hat.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }
    .untitled = Antwort

d-cmtsv52e = Status

d-s2rpes4w = Statustext

d-bmbte2qq = Dauer ms

d-7xxkbrmd = Größe Bytes

d-lfculcqs = Größe genau
    .doc = Ob `size_bytes` die Größe des Rumpfes ist und nicht, wie viel von einem nicht angekündigten Rumpf ankam, bevor der Abruf endete.

d-52brcezl = Gezeigte Bytes
    .doc = Wie viele Bytes des Rumpfes die Ansicht zeigt.

d-dczl7ahz = Binär
    .doc = Ob der Rumpf binär ist, sodass die Ansicht einen Hexdump seines Anfangs zeigt.

d-ue25vzen = Inhaltstyp

d-nxs5lmpc = Rumpf
    .doc = Der Rumpf: wie empfangen, oder ein Hexdump, wenn er binär ist.

d-ryqihqco = Schön
    .doc = Der Rumpf, wie JSON ihn liest, für die schöne Ansicht; sonst der Rumpf wie empfangen.

d-anvyklqi = Kopfzeilen

d-cgnrf3nr-filter-status = Status

d-cgnrf3nr-filter-duration_ms = Dauer ms

d-cgnrf3nr-filter-size_bytes = Größe Bytes

d-cgnrf3nr-filter-size_exact = Größe genau

d-cgnrf3nr-filter-shown_bytes = Gezeigte Bytes

d-cgnrf3nr-filter-binary = Binär

d-osuueta7 = Antwortkopfzeile
    .doc = Eine Kopfzeile, die ein Dienst geantwortet hat.
    .noun = { $count ->
        [one] Antwortkopfzeile
       *[other] Antwortkopfzeilen
    }
    .untitled = Kopfzeile

d-q5ydbtzd = Name

d-vyjpofsi = Wert

d-mxmrgmfk = Umgebung
    .doc = Ein Satz `{"{"}{ $variable }{"}"}`-Ersetzungen, die in eine Anfrage aufgelöst werden, kurz bevor sie hinausgeht.
    .noun = { $count ->
        [one] Umgebung
       *[other] Umgebungen
    }
    .untitled = Unbenannte Umgebung

d-2kunn2pf = Name

d-yhhzdhos = Variablen

d-oldlt6c7 = Variable
    .doc = Eine Variable einer Umgebung.
    .noun = { $count ->
        [one] Variable
       *[other] Variablen
    }
    .untitled = Variable

d-i3e2wno7 = Schlüssel

d-qqgvzyfe = Wert

d-7bxonldr = Aktiv

d-og7vyhyy = Geheim
    .doc = Ob der Wert ein Zugang ist, den jedes Gerät für sich behält.

d-v6jf7adc = Verweis
    .doc = Der geräteeigene Name eines geheimen Werts.

d-oldlt6c7-filter-enabled = Aktiv

d-oldlt6c7-filter-secret = Geheim

d-rghtwwim = Gesendet
    .doc = Eine gesendete Anfrage, wie der Verlauf sie behält.
    .noun = { $count ->
        [one] Gesendet
       *[other] Gesendete
    }
    .untitled = Gesendete Anfrage

d-ikluhyoi = Weg
    .doc = Die Methode und die Adresse ohne Zugangsdaten, Abfragewerte oder Fragment, damit der Eintrag überall benannt werden kann.

d-fwwi364f = Methode

d-w7aoohfv = Adresse

d-7pfct73j = Status

d-cno6gxwc = Dauer ms

d-irpsjk53 = Um

d-4mnpilrd = Wiederholung
    .doc = Die Anfrage, wie sie beim Senden geschrieben war, mit Variablen wie getippt und nie einem Zugang, sodass das erneute Öffnen die ganze Anfrage zurückbringt.

d-rghtwwim-filter-method = Methode

d-rghtwwim-filter-status = Status

d-rghtwwim-filter-duration_ms = Dauer ms

d-rghtwwim-filter-at = Um

d-tkaqpxy4 = Gespeicherte Sammlung
    .doc = Wie eine gespeicherte Sammlung von Anfragen heißt.
    .noun = { $count ->
        [one] Gespeicherte Sammlung
       *[other] Gespeicherte Sammlungen
    }
    .untitled = Unbenannte Sammlung

d-6bygepf4 = Name

d-vghsgjnl = Anfragen

d-vyss2ux2 = Arbeitsbereich
    .doc = Der API-Tester des Arbeitsbereichs: Sammlungen von Anfragen, die Umgebungen, die ihre Variablen füllen, die Anfrage im Bau und was gesendet wurde.
    .noun = { $count ->
        [one] Arbeitsbereich
       *[other] Arbeitsbereiche
    }

d-bqk5mejh = Sammlungen

d-7idvazp5 = Umgebungen

d-bnuffmnw = Aktive Umgebung
    .doc = Ihre Variablen fließen in das ein, was gesendet wird.

d-oela3gry = Geöffnet
    .doc = Die gespeicherte Anfrage, von der der Bauer eine Abschrift hält.

d-yughiaxm = Verlauf
    .doc = Was gesendet wurde, das Neueste zuerst.

d-ylmn6amm = Sendet
    .doc = Eine Anfrage ist unterwegs; `job` benennt sie auf diesem Gerät.

d-lzzl5iuz = Auftrag

d-be7aufil = Gesendete Wiederholung
    .doc = Die Anfrage, wie sie beim Hinausgehen geschrieben war, behalten für den Verlaufseintrag, den sie erzeugt.

d-6msktvdz = Antwort
    .doc = Was der Dienst geantwortet hat, sobald er es tat.

d-bhgazpm3 = Beantwortet
    .doc = Ob die Antwortfläche überhaupt eine Antwort oder einen Fehler zeigt.

d-674n5s6g = Fehler
    .doc = Warum das letzte Senden scheiterte, in den Worten des Geräts.

d-si6y5pgr = Hinweis
    .doc = Eine Zeile unter der Antwort über das letzte Kopieren.

d-vyss2ux2-filter-active_environment = Aktive Umgebung

d-vyss2ux2-filter-opened = Geöffnet

d-vyss2ux2-filter-sending = Sendet

d-vyss2ux2-filter-answer = Antwort

d-vyss2ux2-filter-answered = Beantwortet

d-suqu77re = Sendeanfrage
    .doc = Was die App ihre Maschine zu senden bittet: die Anfrage wie getippt, ihre Variablen und den Verweis, der ihre Anmeldung auf diesem Gerät benennt.
    .noun = { $count ->
        [one] Sendeanfrage
       *[other] Sendeanfragen
    }

d-natoiy5b = Methode

d-xg26wm2t = Adresse

d-52ykdq3w = Parameter

d-2g54j6fl = Kopfzeilen

d-5hvbl66x = Variablen
    .doc = Jede Variable als Name, Wert und Verweis, einen Verweis nur für ein Geheimnis.

d-o4mypkv3 = Anmeldeverfahren

d-wgtleqed = Anmeldename

d-5wzmu546 = Anmeldekopfzeile

d-6wq4j3vq = Anmeldeverweis

d-dnf4fjhy = Rumpfart

d-um3yzxeu = Rumpf

d-m7c3owzv = Formular

d-gjneojn5 = Behaltanfrage
    .doc = Was die App ihr Gerät zu behalten bittet.
    .noun = { $count ->
        [one] Behaltanfrage
       *[other] Behaltanfragen
    }

d-qxcspakn = Wert

d-y337lll5 = Vergessanfrage
    .doc = Was wieder weggenommen wird.
    .noun = { $count ->
        [one] Vergessanfrage
       *[other] Vergessanfragen
    }

d-42ltyzyi = Verweis

d-anj6vtfw = Behaltener Zugang
    .doc = Der Verweis, mit dem ein behaltener Wert antwortet.
    .noun = { $count ->
        [one] Behaltener Zugang
       *[other] Behaltene Zugänge
    }

d-haaxlsd4 = Verweis

d-77rawkj7 = Sendezustand
    .doc = Wie weit eine Anfrage unterwegs ist: ihr Zustand und was sie geantwortet hat, als ein JSON-Text.
    .noun = { $count ->
        [one] Sendezustand
       *[other] Sendezustände
    }

d-tgafs5zj = Zustand

d-vvonqua3 = Antwort
    .doc = Die Antwort als JSON-Text, leer solange die Anfrage unterwegs ist.

d-or6k3lr7 = Fehler

d-q7tlie2e = Beantwortet
    .doc = Was ein Dienst geantwortet hat, wie die Maschine es schreibt.
    .noun = { $count ->
        [one] Beantwortet
       *[other] Beantwortete
    }

d-s4gqhsru = Status

d-kqbtxrg4 = Statustext

d-3ffoqs7c = Dauer ms

d-aciatrpq = Größe Bytes

d-yjlhfc23 = Größe genau

d-n36ppxll = Gezeigte Bytes

d-f4fheroi = Binär

d-loetbxur = Inhaltstyp

d-5c64ziqe = Rumpf

d-agnxdiuh = Kopfzeilen

d-q7tlie2e-filter-status = Status

d-q7tlie2e-filter-duration_ms = Dauer ms

d-q7tlie2e-filter-size_bytes = Größe Bytes

d-q7tlie2e-filter-size_exact = Größe genau

d-q7tlie2e-filter-shown_bytes = Gezeigte Bytes

d-q7tlie2e-filter-binary = Binär

d-4vsac7p2 = Wiederholung
    .doc = Eine Anfrage, wie sie geschrieben war, damit ein Verlaufseintrag sie zurückbringt.
    .noun = { $count ->
        [one] Wiederholung
       *[other] Wiederholungen
    }

d-7fy3fg22 = Name

d-woxdrlbr = Methode

d-uehndmlh = Adresse

d-dw7wgcqy = Parameter

d-oxqptjsf = Kopfzeilen

d-i66no2tx = Formular

d-c7sufjee = Verfahren

d-c4lbngho = Benutzername

d-p3sqgmt6 = Kopfzeile

d-wczlgfhd = Verweis

d-x5jj7sdv = Rumpfart

d-5ez7t24e = Rumpf

d-6pzmh6yh = HTTP-Methode
    .doc = Die Methoden, mit denen eine Anfrage gesendet wird, in der Reihenfolge der Auswahl.

d-ccq7wyrs = GET

d-rudxo7g7 = POST

d-dgce4rjr = PUT

d-yfhoi6zh = PATCH

d-hs76ooyb = Löschen

d-swq6ve3f = HEAD

d-fsyk6dx3 = OPTIONS

d-egb25ohn = Anmeldeverfahren
    .doc = Wie sich eine Anfrage anmeldet.

d-ylg4gzt5 = Keine

d-hvxjosf7 = Bearer

d-nrg462nj = Basic

d-p2c6ixlq = API-Schlüssel

d-wfohrpkl = Rumpfart
    .doc = Was der Rumpf einer Anfrage ist.

d-wqsqfsd2 = Keine

d-kdrofv7x = JSON

d-dqbyfwmb = Formular

d-rfzohfpq = Rohtext

d-tbiad6gq = Neue Anfrage
    .doc = Beginnt eine leere Anfrage im Bauer und lässt die vorige liegen.
    .did = startete eine Anfrage

d-tsedmdm4 = Neue Sammlung
    .doc = Legt eine Sammlung an, in der Anfragen liegen.
    .w-a-collection-needs-a-name = Eine Sammlung braucht einen Namen

d-tsedmdm4-name = Name

d-ho2h6gbu = Sammlung umbenennen
    .doc = Benennt eine Sammlung um.
    .w-a-collection-needs-a-name = Eine Sammlung braucht einen Namen

d-ho2h6gbu-name = Name

d-g2553ryv = Sammlung löschen
    .doc = Löscht eine Sammlung und die in ihr gespeicherten Anfragen.
    .w-that-collection-is-gone = Diese Sammlung ist fort

d-g2553ryv-c = S

d-5dcdkj3j = Anfrage hinzufügen
    .doc = Fügt einer Sammlung eine Anfrage hinzu.

d-5dcdkj3j-name = Name

d-rs63hvbv = Anfrage speichern
    .doc = Speichert die Anfrage im Bauer in einer Sammlung.
    .did = speicherte eine Anfrage

d-rs63hvbv-c = S

d-tdten6rl = Füllen aus
    .doc = Kopiert alles, was eine Anfrage hält, in eine andere.

d-tdten6rl-src = Quelle

d-rgqkewiq = Anfrage öffnen
    .doc = Öffnet eine gespeicherte Anfrage im Bauer.
    .did = öffnete eine Anfrage

d-rgqkewiq-r = A

d-qallswrm = Anfrage umbenennen
    .doc = Benennt die Anfrage im Bauer um.
    .w-a-request-needs-a-name = Eine Anfrage braucht einen Namen

d-qallswrm-name = Name

d-irb2ga4y = Anfrage löschen
    .doc = Löscht eine gespeicherte Anfrage.

d-irb2ga4y-r = A

d-sjsr23pm = Gespeicherte Anfrage löschen
    .doc = Löscht die gespeicherte Anfrage, von der der Bauer eine Abschrift hält.

d-p2234n73 = Parameter hinzufügen
    .doc = Fügt der Anfrage im Bauer einen Abfrageparameter hinzu.

d-2ewontfg = Parameter löschen
    .doc = Nimmt der Anfrage im Bauer einen Abfrageparameter ab.

d-2ewontfg-entry = Eintrag

d-f6z5aeyu = Kopfzeile hinzufügen
    .doc = Fügt der Anfrage im Bauer eine Kopfzeile hinzu.

d-mygqj473 = Kopfzeile löschen
    .doc = Nimmt der Anfrage im Bauer eine Kopfzeile ab.

d-mygqj473-entry = Eintrag

d-f7x7k7up = Formularfeld hinzufügen
    .doc = Fügt der Anfrage im Bauer ein Formularfeld hinzu.

d-vrocssvn = Formularfeld löschen
    .doc = Nimmt der Anfrage im Bauer ein Formularfeld ab.

d-vrocssvn-entry = Eintrag

d-lfxyfzp2 = Neue Umgebung
    .doc = Legt eine Umgebung an und beginnt, sie zu nutzen.
    .w-an-environment-needs-a-name = Eine Umgebung braucht einen Namen

d-lfxyfzp2-name = Name

d-74wev3q6 = Umgebung umbenennen
    .doc = Benennt eine Umgebung um.
    .w-an-environment-needs-a-name = Eine Umgebung braucht einen Namen

d-74wev3q6-name = Name

d-6akuoupy = Umgebung löschen
    .doc = Löscht eine Umgebung und die Variablen, die sie hielt.

d-6akuoupy-e = U

d-tc2ujjsw = Umgebung löschen
    .doc = Löscht die genutzte Umgebung.

d-khft6shk = Umgebung nutzen
    .doc = Wechselt zu der Umgebung, deren Variablen in das einfließen, was gesendet wird.
    .did = wechselte die Umgebung
    .w-that-environment-is-gone = Diese Umgebung ist fort

d-khft6shk-e = U

d-tksdzarq = Keine Umgebung
    .doc = Sendet ohne Umgebung: jedes `{"{"}{ $variable }{"}"}` bleibt wie getippt.
    .did = hörte auf, eine Umgebung zu nutzen

d-cg6szno5 = Variable hinzufügen
    .doc = Fügt einer Umgebung eine Variable hinzu.
    .w-a-variable-needs-a-key = Eine Variable braucht einen Schlüssel

d-cg6szno5-key = Schlüssel

d-cg6szno5-secret = Geheim

d-6ga5m6wc = Variable löschen
    .doc = Nimmt einer Umgebung eine Variable ab und diesem Gerät ihren Wert.

d-6ga5m6wc-v = V

d-e54vruws = Anmeldung behalten
    .doc = Behält den getippten Wert der Anmeldung dort, wo der Arbeitsbereich ihn nicht lesen kann, und merkt sich nur den Verweis.
    .did = behielt eine Anmeldung
    .w-type-the-credential-first = Tippe zuerst den Zugang

d-e54vruws-value = Wert

d-c53ttbvc = Wert behalten
    .doc = Behält den getippten Wert einer geheimen Variable dort, wo der Arbeitsbereich ihn nicht lesen kann.
    .did = behielt einen Wert
    .w-this-variable-s-value-is-shared = Der Wert dieser Variable ist geteilt; ändere ihn in seinem Feld
    .w-type-the-value-first = Tippe zuerst den Wert

d-c53ttbvc-value = Wert

d-pop77kab = Wert vergessen
    .doc = Nimmt diesem Gerät einen behaltenen Wert ab.

d-pop77kab-reference = Verweis

d-rlv4ljnh = Verfahren setzen
    .doc = Ändert das Verfahren und nimmt diesem Gerät den Zugang des vorigen.

d-rlv4ljnh-scheme = Verfahren

d-b4rb4yd6 = Anmeldung vergessen
    .doc = Nimmt diesem Gerät die Anmeldung der Anfrage ab.

d-w3dgdots = Senden
    .doc = Sendet, was der Bauer hält, an die Adresse, die er nennt, in der genutzten Umgebung.
    .did = sendete eine Anfrage
    .w-a-request-is-already-on-its = Eine Anfrage ist bereits unterwegs
    .w-enter-a-url-to-send-the = Gib eine Adresse ein, an die die Anfrage geht

d-gw3xhdma = Anfrage abbrechen
    .doc = Hört auf, auf die Antwort des Dienstes zu warten.

d-atpeap4b = Senden abfragen
    .doc = Fragt das Gerät, wie es der Anfrage unterwegs geht, und behält, was es geantwortet hat.

d-gfuhdzbs = Gesendete öffnen
    .doc = Öffnet eine früher gesendete Anfrage mit dem, was sie beim Hinausgehen hielt.
    .did = öffnete eine gesendete Anfrage

d-gfuhdzbs-s = G

d-zxrevged = Gesendete Anfrage speichern
    .doc = Speichert eine gesendete Anfrage als Anfrage in einer Sammlung, benannt nach ihrem Weg, wenn sie keinen Namen hatte.

d-zxrevged-s = G

d-zxrevged-c = S

d-y3twmqop = Gesendete Anfrage vergessen
    .doc = Vergisst eine gesendete Anfrage.

d-y3twmqop-s = G

d-m7mspkdz = Rumpf kopieren
    .doc = Kopiert den Antwortrumpf, genau wie die Antwortfläche ihn zeigt.
    .w-there-is-no-response-to-copy = Es gibt noch keine Antwort zum Kopieren

d-w7v6ggv4 = Kopfzeilen kopieren
    .doc = Kopiert jede Antwortkopfzeile, eine `Name: value` je Zeile.
    .w-there-is-no-response-to-copy = Es gibt noch keine Antwort zum Kopieren

d-csun3fww = Anfrage kopieren
    .doc = Kopiert die Anfrage wie geschrieben, mit ihren `{"{"}{ $variables }{"}"}` und nie einem Zugang.

d-mfnmmp63 = Hinweis ausblenden
    .doc = Blendet die Zeile unter der Antwort aus.

d-obhk4ydu = Antwort behalten
    .doc = Behält die Antwort im Arbeitsbereich, damit Fenster und Agent dieselbe lesen.

d-54zyn23o = Gesendete festhalten
    .doc = Fügt die eben zurückgekommene Anfrage dem Verlauf des Arbeitsbereichs hinzu, das Neueste zuerst.

d-umcbmlvs = Rumpfart setzen

d-umcbmlvs-mode = Art

d-l6jtvmn5 = Neue Sammlung

d-wle7pvzd = Sammlung umbenennen

d-tmkehrdk = Sammlung löschen

d-wn5cuc2q = Anfrage umbenennen

d-xxkpfuka = Neue Umgebung

d-prn3oxjx = Umgebung umbenennen

d-ubm223na = Umgebung nutzen

d-jhmm6cq2 = Variable hinzufügen

d-idikzlq5 = Anmeldung behalten

d-4pwx6lsh = Wert behalten

d-5bgid526 = Senden

d-jakzld6e = Rumpf kopieren

d-fmqxk3ta = Kopfzeilen kopieren

d-6z4z4ef7 = Tester
    .w-api-tester = API-Tester
    .w-new-collection = Neue Sammlung
    .w-no-environment = Keine Umgebung
    .w-new-environment = Neue Umgebung…
    .w-rename-environment = Umgebung umbenennen…
    .w-add-variable = Variable hinzufügen…
    .w-delete-the-variable = Variable { $key } löschen
    .w-set-the-value-of = Wert von { $key } setzen…
    .w-delete-environment = Umgebung löschen…
    .w-rename-the-collection = Sammlung { $name } umbenennen…
    .w-delete-the-collection = Sammlung { $name } löschen…
    .w-collections = Sammlungen
    .w-no-collections-yet = Noch keine Sammlungen
    .w-history = Verlauf
    .w-cancel-request = Anfrage abbrechen
    .w-send = Senden
    .w-save-request = Anfrage speichern
    .w-the-sent-request-into = Die gesendete Anfrage nach { $name }
    .w-new-collection-2 = Neue Sammlung…
    .w-request-actions = Anfragebefehle
    .w-new-request = Neue Anfrage
    .w-rename-request = Anfrage umbenennen…
    .w-delete-the-saved-request = Gespeicherte Anfrage löschen…
    .w-copy-request = Anfrage kopieren
    .w-forget-the-sent-request = Gesendete Anfrage vergessen…
    .w-request = Anfrage
    .w-params = Parameter
    .w-headers = Kopfzeilen
    .w-auth = Anmeldung
    .w-body = Rumpf
    .w-query-parameters = Abfrageparameter
    .w-add-parameter = Parameter hinzufügen
    .w-no-query-parameters = Keine Abfrageparameter
    .w-add-header = Kopfzeile hinzufügen
    .w-no-headers = Keine Kopfzeilen
    .w-no-response-yet = Noch keine Antwort
    .w-send-the-request-to-see-what = Sende die Anfrage, um zu sehen, was zurückkommt.
    .w-waiting-for-the-server-to-respond = Warte auf die Antwort des Dienstes
    .w-couldn-t-complete-the-request = Die Anfrage ließ sich nicht abschließen
    .w-try-again = Erneut versuchen
    .w-dismiss = Ausblenden
    .w-copy-body = Rumpf kopieren
    .w-copy-headers = Kopfzeilen kopieren
    .w-response = Antwort
    .w-pretty = Schön
    .w-raw = Rohtext
    .w-create = Anlegen
    .w-cancel = Abbrechen
    .w-collection-name = Name der Sammlung
    .w-rename-collection = Sammlung umbenennen
    .w-rename = Umbenennen
    .w-delete-the-collection-2 = Die Sammlung löschen?
    .w-delete = Löschen
    .w-new-environment-2 = Neue Umgebung
    .w-environment-name = Name der Umgebung
    .w-rename-environment-2 = Umgebung umbenennen
    .w-delete-the-environment = Die Umgebung löschen?
    .w-rename-request-2 = Anfrage umbenennen
    .w-request-name = Name der Anfrage
    .w-delete-the-saved-request-2 = Die gespeicherte Anfrage löschen?
    .w-the-saved-request-is-deleted-this = Die gespeicherte Anfrage wird gelöscht. Das lässt sich nicht rückgängig machen.
    .w-add-variable-2 = Variable hinzufügen
    .w-variable-key = Schlüssel der Variable
    .w-its-value-is-a-credential-kept = Ihr Wert ist ein Zugang, den jedes Gerät für sich behält
    .w-set-the-value = Den Wert setzen
    .w-store = Behalten
    .w-value = Wert
    .w-variables = Variablen…
    .w-variables-2 = Variablen
    .w-done = Fertig
    .w-use-this-variable = Diese Variable nutzen
    .w-no-value-kept = Kein Wert behalten
    .w-kept-on-this-device = Auf diesem Gerät behalten
    .w-variable-value = Wert der Variable
    .w-delete-variable = Variable löschen
    .w-no-variables-yet = Noch keine Variablen
    .w-set-the-value-2 = Den Wert setzen

d-a6fjnueq = Adressleiste
    .w-method = Methode
    .w-enter-a-url = Adresse eingeben

d-u5vthwsc = Parameterzeile
    .w-use-this-parameter = Diesen Parameter nutzen
    .w-key = Schlüssel
    .w-value = Wert
    .w-delete-parameter = Parameter löschen

d-xira45uc = Kopfzeilenzeile
    .w-use-this-header = Diese Kopfzeile nutzen
    .w-header-name = Name der Kopfzeile
    .w-value = Wert
    .w-delete-header = Kopfzeile löschen

d-xyfp5r4l = Formularzeile
    .w-use-this-field = Dieses Feld nutzen
    .w-key = Schlüssel
    .w-value = Wert
    .w-delete-field = Feld löschen

d-t7usmncz = Anmeldefläche
    .w-authentication = Anmeldung
    .w-scheme = Verfahren
    .w-no-auth = Keine Anmeldung
    .w-bearer-token = Bearer-Token
    .w-basic-auth = Basisauthentifizierung
    .w-api-key = API-Schlüssel
    .w-this-request-does-not-use-authentication = Diese Anfrage nutzt keine Anmeldung.
    .w-a-sign-in-is-kept-on = Eine Anmeldung wird auf diesem Gerät behalten.
    .w-no-sign-in-is-kept-on = Auf diesem Gerät ist noch keine Anmeldung behalten.
    .w-username = Benutzername
    .w-header-name = Name der Kopfzeile
    .w-forget-sign-in = Anmeldung vergessen
    .w-set-the-sign-in = Die Anmeldung setzen
    .w-store = Behalten
    .w-cancel = Abbrechen

d-dvg27g6l = Rumpffläche
    .w-body = Rumpf
    .w-none = Keine
    .w-json = JSON
    .w-form = Formular
    .w-raw = Rohtext
    .w-this-request-does-not-have-a = Diese Anfrage hat keinen Rumpf.
    .w-add-form-field = Formularfeld hinzufügen
    .w-no-form-fields = Keine Formularfelder
    .w-the-body = Der Rumpf

d-3n3wrhwy = Antwortkopfzeilen
    .w-response-headers = Antwortkopfzeilen
    .w-name = Name
    .w-value = Wert

d-t7bbwzg4 = Builder
    .doc = Die Anfrageanlage des Arbeitsbereichs: die Anfrage, die gerade getippt wird, überschrieben, wenn eine andere geöffnet wird.

d-vyss2ux2-filter-builder = Builder

d-obhk4ydu-answer = Antwort

d-54zyn23o-answer = Antwort
