# Mail auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-mail = Mail

d-so-tilo-mail-keywords = email, e-mail, mail, inbox, mailbox, gmail, imap, smtp, apple mail, evolution, geary, kmail, kontact, emails, post, posteingang, briefe, postfach, mailprogramm, elektronische post, e-mails

d-weesm3yb = Konto
    .doc = Was der Server eines Mailkontos des Arbeitsbereichs hält.
    .noun = { $count ->
        [one] Konto
       *[other] Konten
    }

d-yujnud5c = Schlüssel

d-4lo3ei5n = Adresse

d-j6nqqrit = Name

d-b5k7mvka = Störung
    .doc = Was der Server des Kontos sagte, als es ihn nicht erreichte, in seinen Worten: leer, solange es ihm gut geht.

d-33f3rqw2 = Online

d-hzbwof46 = Geholt
    .doc = Wann die Mail des Kontos zuletzt von seinem Server hereingenommen wurde, nach der Uhr, die die Person las: 1970, wenn nie.

d-jxc5rufs = Signatur
    .doc = Womit die Nachrichten des Kontos enden: die eigene der Person, je Konto, wie jedes Mailprogramm eine Nachricht signiert.

d-yinmk5zb = Identitäten
    .doc = Die weiteren Namen und Adressen, unter denen das Konto sendet, neben seiner eigenen, je eine Zeile `Name <address>`: das, wovon eine Person schreibt, wenn sie als Rolle statt als sie selbst schreibt.

d-ehmoyzk2 = Postfächer

d-qybqg4dl = Unterhaltungen

d-weesm3yb-filter-online = Online

d-weesm3yb-filter-fetched = Geholt

d-ecbhcqdr = Postfach
    .doc = Ein Postfach eines Kontos.
    .noun = { $count ->
        [one] Postfach
       *[other] Postfächer
    }

d-mh47w3p6 = Schlüssel

d-tlasysm5 = Name

d-xhiqdk3p = Rolle

d-dtd4czyv = Ungelesen

d-rxoe5vzb = Gesamt

d-al2vjuet = Mehr
    .doc = Ob der Server des Kontos ältere Nachrichten hat als dieser Arbeitsbereich: das, wofür „Ältere Nachrichten anzeigen“ die Seite davor liest.

d-ecbhcqdr-filter-unread = Ungelesen

d-ecbhcqdr-filter-total = Gesamt

d-ecbhcqdr-filter-more = Mehr

d-xnczt27x = Unterhaltung
    .doc = Eine Unterhaltung: Nachrichten, die einander antworten, die neueste zuletzt.
    .noun = { $count ->
        [one] Unterhaltung
       *[other] Unterhaltungen
    }

d-ecmpoary = Schlüssel

d-zu23m2dm = Konto

d-u732lnuk = Betreff

d-o3knjgos = Neueste

d-sxcvmlon = Auszug

d-vyc7aj7v = Absender

d-izafodav = Anzahl

d-56lwowdg = Ungelesen

d-abe6c5jl = Markiert

d-3vusnp7i = Postfächer

d-ueyfpzep = Nachrichten

d-gy7z7du3 = Labels
    .doc = Die Labels, die Menschen ihr gegeben haben (die von Gmail und Fastmail): die eigenen der Person, nie die des Servers.

d-bdoh33hs = Zurückgestellt bis
    .doc = Zurückgestellt bis zu diesem Zeitpunkt: aus dem Posteingang bis dahin, und zurück, sobald er gekommen ist, ohne dass ein Fenster offen ist.

d-z7qxz544 = Stumm
    .doc = Stumm: die Unterhaltung bleibt aus dem Posteingang, während neue Nachrichten weiter kommen, wie eine geschäftige Unterhaltung stillgelegt wird, und sie ist weiter unter „Stumm“ und „Alle Mails“ zu finden.

d-xnczt27x-filter-inbox = Posteingang

d-xnczt27x-filter-snoozed = Zurückgestellt

d-xnczt27x-filter-labels = Labels

d-xnczt27x-filter-flagged = Markiert

d-xnczt27x-filter-muted = Stumm

d-xnczt27x-filter-sent = Gesendet

d-xnczt27x-filter-archive = Archiv

d-xnczt27x-filter-junk = Spam

d-xnczt27x-filter-trash = Papierkorb

d-xnczt27x-filter-unread = Ungelesen

d-5ynnrlxh = Label
    .doc = Ein Label, das die Person ihren Unterhaltungen gegeben hat: ihr eigenes, nie das des Servers, wie es die von Gmail und Fastmail sind.
    .noun = { $count ->
        [one] Label
       *[other] Labels
    }

d-ammoua3h = Name

d-sshtf3z7 = Nachricht
    .doc = Eine Nachricht.
    .noun = { $count ->
        [one] Nachricht
       *[other] Nachrichten
    }

d-kzjq6hvc = Schlüssel

d-kdxoo3c6 = Konto

d-ntqgnz4f = Betreff

d-l3nc65iv = Von

d-mo6l36a3 = Name des Absenders

d-46k23iwq = An

d-bgzxugi5 = Cc

d-tfxwkfn2 = Empfangen

d-sbrol5fc = Auszug

d-4bchz3ch = Text

d-plb4mg2p = Html
    .doc = Was sie sagt, wie ihr Absender es gestaltet hat: ihr HTML-Teil, die Bilder, die sie an Ort und Stelle zeigt, aus den Inhalten dieses Geräts gelesen.

d-anuds3qt = Kennung der Nachricht

d-asu6ymha = Verweise

d-yltl32r3 = Anhänge
    .doc = Die Dateien, mit denen sie kam, jede in den Inhalten des Arbeitsbereichs: eine, deren Bytes noch nicht auf diesem Gerät sind, ist ohne Inhalt genannt.

d-ubfcf6ep = Ungelesen

d-vxxb4sqr = Markiert

d-xlejsz5j = Postfach

d-hgysjvfy = Original
    .doc = Der Inhaltsverweis der Nachricht, wie sie gesendet wurde, bei einer, die von außerhalb des Servers des Kontos übernommen wurde (`deliver_message`): leer bei einer, die ihr Server hält.

d-wuefabwr = Teile
    .doc = Wo jeder große Teil einer von außerhalb übernommenen Nachricht in `original` steht und in welchen Inhaltsstücken er liegt, ein JSON-Text je Zeile: leer bei einer, die ihr Server hält.

d-sshtf3z7-filter-received = Empfangen

d-sshtf3z7-filter-unread = Ungelesen

d-sshtf3z7-filter-flagged = Markiert

d-lidkonvr = Entwurf
    .doc = Eine Nachricht im Schreiben: wohin sie geht, was sie sagt, was sie beantwortet und wie es um ihren Weg nach draußen steht.
    .noun = { $count ->
        [one] Entwurf
       *[other] Entwürfe
    }
    .untitled = Neue Nachricht

d-m2g6zrle = Von

d-zx2b4ax2 = Absenderadresse
    .doc = Die Identität des Absenders (`from`), eine der eigenen des Kontos (`Name <address>`): leer für die eigene Adresse des Kontos und für eine, unter der es nicht mehr sendet.

d-hd3o4gkq = An

d-cwukhrvn = Cc

d-ib44zkec = Bcc
    .doc = Empfänger einer Blindkopie: sie sehen die Nachricht, werden in ihr aber nicht genannt.

d-tyxmravh = Betreff

d-d5u2c7jm = Text

d-w6twdkob = Antwort auf

d-w26tzvt4 = Verweise

d-yfyt2cfh = Gesendet

d-lgrwf5yf = Senden um
    .doc = Wann sie rausgeht: 1970 für einen Entwurf, der noch nicht gesendet ist.

d-h2nvr2op = Zurückgeholt
    .doc = Sie wurde zurückgeholt, bevor sie rausging: sie geht nie raus und ist wieder ein Entwurf.

d-em6rrmae = Einladung
    .doc = Die Kalendereinladung, die sie trägt (iCalendar), neben dem, was sie sagt, damit der Kalender des Gasts sie beantworten kann: leer für keine.

d-2tq2zegc = Anhänge
    .doc = Die Dateien, die sie mitnimmt.

d-lidkonvr-filter-drafts = Entwürfe

d-su4h67i5 = Mail
    .doc = Die Mail der Person: die Konten, die die Einstellungen des Arbeitsbereichs halten, was sie gerade schreibt und die Labels, die sie ihren Unterhaltungen gegeben hat.
    .noun = { $count ->
        [one] Mail
       *[other] Mails
    }

d-blig7xcq = Konten

d-gkuo27e6 = Entwürfe

d-z3zcztpz = Labels

d-bag7bf25 = Übernommene Mail
    .doc = Unterhaltungen aus einem früheren Postfach, außerhalb der Sammlungen des IMAP-Kontos.

d-4ow5ydy5 = Wartezeit
    .doc = Wie viele Sekunden eine gesendete Nachricht wartet, bevor sie rausgeht, damit sie zurückgeholt werden kann: die Einstellung der Person, zehn von Haus aus, und keine Wartezeit, wenn sie null ist.

d-su4h67i5-filter-hold = Wartezeit

d-ejo2352x = Signatur
    .doc = Legt fest, womit das Konto seine Nachrichten signiert, oder löscht es: die eigene der Person, je Konto.

d-ejo2352x-signature = Signatur

d-546ahgq6 = Zeit zum Zurückholen
    .doc = Legt fest, wie viele Sekunden eine gesendete Nachricht wartet, bevor sie rausgeht, damit sie zurückgeholt werden kann: bei null wartet sie gar nicht.
    .w-a-message-cannot-wait-less-than = Eine Nachricht kann nicht kürzer als gar nicht warten

d-546ahgq6-seconds = Sekunden

d-arnkrvgw = Jetzt abrufen
    .doc = Fragt jetzt den Server des Kontos nach seiner Mail: was eine Person drückt, wenn das Fenster sagt, der Server sei nicht erreichbar gewesen oder was es geholt hat, habe sich nicht sichern lassen.

d-wgbxl4or = Ältere Nachrichten anzeigen
    .doc = Bittet den Server des Kontos um die Nachrichten, die älter sind als die älteste, die dieser Arbeitsbereich im Postfach hat, damit sich ein Ordner, der länger als eine Seite ist, bis zu seiner ersten Nachricht zurücklesen lässt.

d-eon6gbrn = Neue Nachricht
    .doc = Beginnt eine Nachricht von dem Konto.

d-ygbvdl5l = Neue Nachricht
    .doc = Beginnt eine Nachricht vom ersten Konto.

d-hom44or2 = Per Mail einladen
    .doc = Beginnt eine Nachricht, die `to` zu einem Termin einlädt, mit ihrer Einladung (`calendar_ics`, iCalendar) neben dem, was sie sagt: was der Kalender Mail für jeden Gast übergibt, den er einlädt.
    .w-add-a-mail-account-in-settings = Füge zuerst in den Einstellungen ein Mailkonto hinzu
    .w-an-invitation-goes-to-someone = Eine Einladung geht an jemanden
    .w-the-invitation-is-not-a-calendar = Die Einladung ist keine Kalenderanfrage

d-hom44or2-to = An

d-hom44or2-subject = Betreff

d-hom44or2-body = Text

d-hom44or2-calendar_ics = Kalender-ICS

d-grphua5j = Nachricht zustellen
    .doc = Nimmt eine Nachricht auf, die den Arbeitsbereich von außerhalb der Server seiner Konten erreicht hat: eine aus einem anderen Mailprogramm übernommene (`tilo migrate`) oder eine über die API übergebene.
    .w-a-message-says-who-it-is = Eine Nachricht sagt, von wem sie ist

d-grphua5j-from = Von

d-grphua5j-subject = Betreff

d-grphua5j-body = Text

d-grphua5j-folder = Ordner

d-grphua5j-read = Gelesen

d-grphua5j-to = An

d-grphua5j-cc = Cc

d-grphua5j-date = Datum

d-grphua5j-conversation = Unterhaltung

d-grphua5j-message_id = Nachrichten-ID

d-grphua5j-original = Original

d-grphua5j-parts = Teile

d-grphua5j-attachments = Anhänge

d-tr43xnyj = Als Markdown exportieren
    .export = Markdown
    .doc = Die Nachricht als Markdown-Datei: was eine Person exportiert, um sie anderswo zu lesen oder zu behalten.

d-shpafbd7 = Als JSON exportieren
    .export = JSON
    .doc = Die Nachricht als JSON-Datei: woraus ein Skript oder ein Agent ihren Datensatz liest.

d-ydo5gq5e = Als .eml exportieren
    .export = .eml
    .doc = Die Nachricht als `.eml`-Datei, die jedes Mailprogramm öffnet.

d-3vrmj56n = Eine Nachrichtendatei öffnen
    .doc = Öffnet eine Nachricht, die als Datei eines Mailprogramms vorliegt (`.eml`, eine einzelne Nachricht einer mbox oder Apples `.emlx`): ihre Kopfzeilen werden gelesen, und sie wird zu der Mail gelegt, die außerhalb der Konten liegt, gelesen, im Posteingang.
    .w-the-file-holds-no-message = Die Datei enthält keine Nachricht

d-3vrmj56n-file = Datei

d-hirareiw = Antworten
    .doc = Beantwortet die neueste Nachricht der Unterhaltung an wen sie geschrieben hat und zitiert sie unter dem Geschriebenen.

d-72f72tm7 = Allen antworten
    .doc = Beantwortet die neueste Nachricht der Unterhaltung an alle, an die sie ging, und zitiert sie.

d-h5fphq56 = Antworten auf
    .doc = Beantwortet eine Nachricht der Unterhaltung an wen sie geschrieben hat und zitiert sie.

d-ym62maet = Allen antworten
    .doc = Beantwortet eine Nachricht der Unterhaltung an alle, an die sie ging, außer die Person, die antwortet.

d-jokcwung = Nachricht weiterleiten
    .doc = Schickt eine Nachricht der Unterhaltung weiter, mit ihrem Absender, wann sie kam und was sie sagt, und den Dateien, mit denen sie kam.

d-q6ltvsjf = Weiterleiten
    .doc = Schickt die neueste Nachricht der Unterhaltung weiter, mit ihrem Absender, was sie sagt und den Dateien, mit denen sie kam.

d-ipew27sc = Anhang speichern
    .doc = Eine Datei, mit der die Nachricht kam, als eigene Datei: was Speichern in Dateien ablegt und was ein Agent liest oder einer anderen App gibt.
    .w-it-came-with-no = Sie kam ohne { $name }
    .w-is-not-downloaded-yet = { $name } ist noch nicht heruntergeladen

d-ipew27sc-name = Name

d-dshny4x3 = Datei anhängen
    .doc = Nimmt eine Datei mit der Nachricht mit: eine, die die Person wählt, oder eine, die ein Agent übergibt.
    .w-this-message-went-already = Diese Nachricht ist schon raus
    .w-a-message-takes-up-to-16 = Eine Nachricht nimmt bis zu 16 Dateien mit
    .w-is-larger-than-32-mb = { $name } ist größer als 32 MB

d-dshny4x3-file = Datei

d-jk7ep3bb = Anhang entfernen
    .doc = Lässt eine Datei zurück, die die Nachricht mitnehmen sollte.
    .w-this-message-went-already = Diese Nachricht ist schon raus
    .w-it-takes-no = Sie nimmt kein { $name } mit

d-jk7ep3bb-name = Name

d-eqvgypxr = Archivieren
    .doc = Archiviert die Unterhaltung: ihre Nachrichten verlassen den Posteingang.
    .w-it-is-not-in-the-inbox = Sie ist nicht im Posteingang

d-eef4pxgf = In den Posteingang
    .doc = Holt die Unterhaltung in den Posteingang zurück und hebt ihre Stummschaltung auf: was eine Person zurückholt, will sie wieder sehen.
    .w-it-is-in-the-inbox = Sie ist im Posteingang

d-taywgw3n = Verschieben nach
    .doc = Verschiebt die Unterhaltung in ein anderes Postfach ihres Kontos.
    .w-it-is-in-already = Sie ist schon in { $name }

d-taywgw3n-to = Nach

d-xytb2lle = Löschen
    .doc = Löscht die Unterhaltung: ihre Nachrichten gehen in den Papierkorb.
    .w-it-is-in-the-trash = Sie ist im Papierkorb

d-pz2ntes2 = Endgültig löschen
    .doc = Löscht die gelöschten Nachrichten der Unterhaltung endgültig, auch von ihrem Server, und mit ihnen die Unterhaltung, wenn keine Nachricht von ihr mehr übrig ist.
    .w-it-is-not-in-the-trash = Sie ist nicht im Papierkorb
    .w-nothing-of-it-is-in-the = Nichts davon liegt im Papierkorb

d-le4lvs2c = Markieren
    .doc = Markiert die Unterhaltung oder nimmt die Markierung zurück.
    .w-unflag = Markierung entfernen
    .w-flag = Markieren

d-le4lvs2c-flagged = Markiert

d-6bwt463c = Zurückstellen
    .doc = Stellt die Unterhaltung bis zu einem Zeitpunkt weg: sie verlässt den Posteingang und kommt zurück, sobald der Zeitpunkt gekommen ist, ohne dass ein Fenster offen ist.
    .w-pick-a-moment-still-to-come = Wähle einen Zeitpunkt, der noch kommt

d-6bwt463c-until = Bis

d-kl4ikisg = Eine Stunde zurückstellen
    .doc = Stellt die Unterhaltung für eine Stunde weg, das Zurückstellen, zu dem Menschen am häufigsten greifen.

d-7icvl7qs = Bis morgen zurückstellen
    .doc = Stellt die Unterhaltung bis morgen früh um neun weg, in der eigenen Zeitzone der Person.

d-wl2c3dxh = Zurückholen
    .doc = Holt eine zurückgestellte Unterhaltung jetzt zurück, was auch ihr Zeitpunkt war: sie ist wieder im Posteingang.

d-ck5k6uop = Label
    .doc = Gibt der Unterhaltung ein Label oder nimmt eines weg: `on` sagt, was.
    .w-a-label-needs-a-name = Ein Label braucht einen Namen

d-ck5k6uop-name = Name

d-ck5k6uop-on = An

d-vydc2hnq = Label umbenennen
    .doc = Benennt eines der Labels der Person um, mit allen Unterhaltungen, die es tragen.
    .w-a-label-needs-a-name = Ein Label braucht einen Namen
    .w-you-have-a-label = Du hast schon ein Label { $name }

d-vydc2hnq-l = L

d-vydc2hnq-name = Name

d-phsy7vhs = Label löschen
    .doc = Löscht eines der Labels der Person und nimmt es von jeder Unterhaltung, die es trägt.

d-phsy7vhs-l = L

d-xp4s2kl7 = Geplantes Senden abbrechen
    .doc = Holt eine Nachricht in den Postausgang zurück: sie war gesendet, um zu einem noch kommenden Zeitpunkt rauszugehen, und sie ist wieder ein Entwurf, was auch der Zeitpunkt war.
    .w-it-is-not-waiting-to-go = Sie wartet nicht darauf, rauszugehen
    .w-it-went-already = Sie ist schon rausgegangen
    .w-scheduled-send-cancelled = Geplantes Senden abgebrochen

d-xxxfkczs = Später senden
    .doc = Sendet die Nachricht zu einem Zeitpunkt statt jetzt: sie wartet bis dahin im Postausgang, wie die Wartezeit der Person eine Nachricht warten lässt, und geht raus, ohne dass ein Fenster offen ist.
    .w-this-message-went-already = Diese Nachricht ist schon rausgegangen
    .w-it-goes-to-nobody-yet = Sie geht noch an niemanden
    .w-add-a-mail-account-in-settings = Füge zuerst in den Einstellungen ein Mailkonto hinzu
    .w-pick-a-moment-still-to-come = Wähle einen Zeitpunkt, der noch kommt

d-xxxfkczs-at = Um

d-jysss72s = Morgen früh senden
    .doc = Sendet die Nachricht morgen um neun, den Zeitpunkt, zu dem Menschen am häufigsten greifen, wenn sie abends schreiben und nicht wollen, dass es dann ankommt.

d-sqtuwk6b = Alle als gelesen markieren
    .doc = Markiert jede Unterhaltung des Kontos als gelesen.

d-gkqpzio6 = Stumm schalten
    .doc = Schaltet die Unterhaltung stumm oder hebt die Stummschaltung auf: eine stumme bleibt aus dem Posteingang, bis sie zurückgeholt wird, was ihre neuen Nachrichten auch tun.
    .w-unmute = Stummschaltung aufheben
    .w-mute = Stumm schalten

d-gkqpzio6-muted = Stumm

d-trjwm3gb = Als ungelesen markieren
    .doc = Markiert die Unterhaltung als gelesen oder wieder als ungelesen.
    .w-mark-as-read = Als gelesen markieren
    .w-mark-as-unread = Als ungelesen markieren

d-hk43bpjp = Beim Öffnen lesen
    .doc = Das Öffnen einer Unterhaltung liest sie, egal wo sie geöffnet wird: in der Liste, in der Suche, über einen Link oder durch einen Agenten.

d-trjwm3gb-unread = Ungelesen

d-7zk6ua6p = Senden von
    .doc = Sendet die Nachricht von einem anderen Konto der Person.
    .w-this-message-went-already = Diese Nachricht ist schon rausgegangen

d-7zk6ua6p-a = A

d-naegttbl = Senden als
    .doc = Sendet die Nachricht unter einem anderen Namen oder einer anderen Adresse ihres Kontos, `Name <address>`: das, was eine Person wählt, die als Rolle statt als sie selbst schreibt.
    .w-this-message-went-already = Diese Nachricht ist schon rausgegangen

d-naegttbl-identity = Identität

d-qrcwx7a6 = Entwurf löschen
    .doc = Wirft eine Nachricht im Schreiben weg.

d-qrcwx7a6-d = D

d-qcfc2rhb = Senden
    .doc = Sendet die Nachricht von ihrem Konto.
    .w-this-message-went-already = Diese Nachricht ist schon rausgegangen
    .w-it-goes-to-nobody-yet = Sie geht noch an niemanden
    .w-add-a-mail-account-in-settings = Füge zuerst in den Einstellungen ein Mailkonto hinzu
    .w-pick-the-account-it-goes-from = Wähle das Konto, von dem sie geht
    .w-its-addresses-subject-or-words-are = Ihre Adressen, ihr Betreff oder ihre Worte sind länger, als eine Nachricht trägt

d-twebu5cb = Senden rückgängig
    .doc = Holt eine Nachricht zurück, die gesendet ist und noch nicht rausging: sie geht nie raus und die Nachricht ist wieder ein Entwurf, wie sie war.
    .w-it-has-not-been-sent = Sie ist nicht gesendet
    .w-it-went-already = Sie ist schon rausgegangen
    .w-taken-back-before-it-went = Zurückgeholt, bevor sie rausging

d-a4rv5wm6 = Antworten
    .doc = Beantwortet die Unterhaltung mit dem Getippten: die Nachricht geht an wen ihre neueste geschrieben hat, als Antwort darauf, zitiert.
    .w-say-something-first = Sag zuerst etwas
    .w-the-account-for-this-conversation-is = Das Konto für diese Unterhaltung ist nicht verfügbar

d-a4rv5wm6-body = Text

d-t57taubn = Weitergeleitet
    .w-not-downloaded-so-not-sent-on = Nicht heruntergeladen, daher nicht weitergeschickt: { $names }

d-dqdt6fce = Titel des Platzes
    .w-all-mail = Alle Mails

d-k2js7gyd = Zustellung sagt
    .w-taken-back-and-a-draft-again = Zurückgeholt und wieder ein Entwurf
    .w-sent = Gesendet
    .w-sending = Wird gesendet
    .w-not-sent = Nicht gesendet: { $reason }
    .w-not-known-whether-it-was-sent = Nicht bekannt, ob sie gesendet wurde
    .w-sending-in-a-moment = Geht gleich raus
    .w-waiting-to-send = Wartet darauf, gesendet zu werden
    .w-waiting-to-go-online = Wartet auf eine Verbindung: { $reason }

d-2zkppx6r = Zeit zum Zurückholen~present

d-7jf4pn3j = Kalendereinladung vorbereiten~present

d-s2qkkueu = Nachricht zustellen~present

d-vdfbsj4a = Nachrichtendatei öffnen~present

d-swcww42a = Anhang~present

d-fxtnru4s = Anhängen~present

d-p76r6z3v = Entfernen~present

d-bj7uen6f = Archivieren~present

d-bs42bxdn = In den Posteingang~present

d-wfvq5lde = Verschieben nach~present

d-efageosr = Löschen~present

d-djmtvfs7 = Endgültig löschen~present

d-cvmtfflh = Markieren~present

d-3ihnegs3 = Zurückstellen~present

d-p5knuykz = Label~present

d-cstttevp = Label umbenennen~present

d-cxnwxvyu = Geplantes Senden abbrechen~present

d-lnirkyf7 = Später senden~present

d-5zi36ozt = Stumm schalten~present

d-xnse2xt5 = Als gelesen markieren~present

d-iv7e7g52 = Senden von~present

d-5kuylc3o = Senden als~present

d-czouixzd = Senden~present

d-i3cueozg = Senden rückgängig~present

d-4hds3dwy = Antworten~present

d-5wix2354 = Verfassen
    .w-to = An { $to }
    .w-bcc = Bcc { $bcc }

d-dhradxj7 = Kontohinweis
    .w-can-t-save-what-fetched = Was { $name } geholt hat, lässt sich nicht sichern
    .w-can-t-reach = { $name } ist nicht erreichbar
    .w-last-fetched = { $reason }. Zuletzt geholt { $fetched }
    .w-try-again = Erneut versuchen
    .w-can-t-sign-in-to = Anmeldung bei { $address } nicht möglich
    .w-check-its-password-in-settings-under = Prüfe das Passwort in den Einstellungen unter Konten.
    .w-open-account-settings = Kontoeinstellungen öffnen
    .w-mail-has-no-password-for = Mail hat kein Passwort für { $address }
    .w-nothing-was-sent-to-its-server = An seinen Server wurde nichts gesendet. Gib das Passwort in den Einstellungen unter Konten ein.
    .w-mail-can-t-use-s-password = Mail kann das Passwort von { $address } nicht verwenden
    .w-you-re-offline = Du bist offline
    .w-new-mail-arrives-when-you-re = Neue Mail kommt, wenn du wieder online bist.

d-7rcrynvm = Mail

d-4fjgzcq6 = Zusammengeführt
    .w-all-accounts = Alle Konten
    .w-all-inboxes = Alle Posteingänge
    .w-all-mail = Alle Mails
    .w-labels = Labels
    .w-recovered-mail = Übernommene Mail
    .w-drafts = Entwürfe
    .w-outbox = Postausgang
    .w-trusted-senders = Vertrauenswürdige Absender

d-3k7hkyhl = Übernommene Mail
    .w-nothing-here = Nichts hier

d-m5y6nl2m = Alle Posteingänge
    .w-add-a-mail-account-in-settings = Füge in den Einstellungen ein Mailkonto hinzu
    .w-is-inbox = is:inbox

d-s7tarqlp = Unterhaltungen der Konten
    .w-nothing-has-arrived-yet = Noch nichts angekommen
    .w-no-messages-in = Keine Nachrichten in { $place_title }
    .w-open-account-settings = Kontoeinstellungen öffnen
    .w-show-older-messages-from = Ältere Nachrichten von { $address } anzeigen

d-tfom3oeo = Unterhaltungen des Kontos
    .w-nothing-has-arrived-yet = Noch nichts angekommen
    .w-no-messages-in = Keine Nachrichten in { $name }
    .w-no-messages-in-2 = Keine Nachrichten in { $place_title }
    .w-open-account-settings = Kontoeinstellungen öffnen
    .w-show-older-messages = Ältere Nachrichten anzeigen

d-gka45our = Postfächer
    .w-all-mail = Alle Mails
    .w-labels = Labels

d-zp2cy2sc = Posteingang
    .w-is = is:{ $key }
    .w-nothing-here = Nichts hier

d-jgdzx3la = Unterhaltungsliste

d-u43u7z6w = Unterhaltungszeile

d-77noqogh = Entwurfszeile

d-bbuecgsi = Unterhaltung
    .w-reply-to = Antwort auf { $subject }
    .w-reply = Antworten
    .w-delete-this-conversation-permanently = Diese Unterhaltung endgültig löschen?
    .w-it-is-in-the-trash = Sie ist im Papierkorb
    .w-this-can-t-be-undone = Das lässt sich nicht rückgängig machen.
    .w-it-stays-here-until-you-delete = Sie bleibt hier, bis du sie endgültig löschst.
    .w-delete-permanently = Endgültig löschen
    .w-reply-all = Allen antworten
    .w-forward = Weiterleiten
    .w-flag = Markieren
    .w-unflag = Markierung entfernen
    .w-archive = Archivieren
    .w-move-to-inbox = In den Posteingang
    .w-mark-as-read = Als gelesen markieren
    .w-mark-as-unread = Als ungelesen markieren
    .w-more = Mehr
    .w-snooze-an-hour = Eine Stunde zurückstellen
    .w-snooze-until-tomorrow = Bis morgen zurückstellen
    .w-bring-back = Zurückholen
    .w-mute = Stumm schalten
    .w-unmute = Nicht mehr stumm
    .w-delete = Löschen

d-krn2y5tw = Nachrichten der Unterhaltung

d-n2xgxnbw = Nachricht
    .w-open-contact = Kontakt öffnen
    .w-reply = Antworten
    .w-reply-all = Allen antworten
    .w-forward = Weiterleiten
    .w-to = an { $to }
    .w-hide-quoted-text = Zitierten Text ausblenden
    .w-show-quoted-text = Zitierten Text anzeigen
    .w-one-picture-in-this-message-is = Ein Bild dieser Nachricht wird nicht angezeigt.
    .w-pictures-in-this-message-are-not = { $len } Bilder dieser Nachricht werden nicht angezeigt.
    .w-show-pictures = Bilder anzeigen
    .w-always-show-s = Immer die von { $sender_address } anzeigen
    .w-show-plain-text = Nur Text anzeigen
    .w-show-formatted = Formatiert anzeigen
    .w-pictures-from-are-always-shown = Bilder von { $sender_address } werden immer angezeigt.
    .w-ask-again = Wieder fragen
    .w-one-link-in-this-message = Ein Link dieser Nachricht
    .w-links-in-this-message = { $len } Links dieser Nachricht
    .w-copy-link = Link kopieren

d-llqqdcy5 = Anhang-Chip
    .w-save = Speichern
    .w-not-downloaded = { $name }, nicht heruntergeladen

d-nxayphhi = Nachrichtendatei
    .w-message = Nachricht

d-oqpazwvc = Schreiben
    .w-status = Status
    .w-sending = Wird gesendet
    .w-it-goes-in-a-moment = Sie geht gleich raus.
    .w-undo-send = Senden rückgängig
    .w-from = Von
    .w-no-mail-account-to-send-from = Kein Mailkonto zum Senden
    .w-add-an-account-in-settings = Mailkonto in den Einstellungen hinzufügen
    .w-to = An
    .w-name-example-org = name@example.org
    .w-cc = Cc
    .w-bcc = Bcc
    .w-subject = Betreff
    .w-bold = Fett
    .w-italic = Kursiv
    .w-underline = Unterstrichen
    .w-bulleted-list = Aufzählung
    .w-numbered-list = Nummerierte Liste
    .w-heading = Überschrift
    .w-quote = Zitat
    .w-cc-bcc = Cc/Bcc
    .w-link = Link
    .w-https-example-org = https://example.org
    .w-add-link = Link hinzufügen
    .w-write-your-message = Schreib deine Nachricht
    .w-attach = Anhängen
    .w-send = Senden
    .w-add-an-account-to-send = Konto zum Senden hinzufügen
    .w-save-in-drafts = In Entwürfen speichern
    .w-this-answers = Dies antwortet auf { $subject }

d-ozukvva2 = Zur Seite gelegt
    .doc = Was die frühere Mail an diesem Entwurf hielt, wofür diese keinen Platz hat (Bcc, Anhänge, private Notizen), in Worten gesagt: nie mit ihm versendet.

d-v7wxxiaj = Frühere Mail übernehmen
    .doc = Das Postfach, das die frühere Mail vor der Übernahme dieser Kennung hielt, einmalig in dieses übernommen: jeder Entwurf, den sie schrieb, und jede noch in ihrem Postausgang wartende Nachricht ist hier ein Entwurf, vom selben Konto, an dieselben Personen, mit demselben Betreff, Text und derselben Einladung.

d-xnczt27x-filter-latest = Empfangen

d-lnhvmglv = Nachricht entwerfen
    .doc = Schreibt eine Nachricht, die die Person liest und sendet: an wen, ihr Betreff und was sie sagt, als Antwort auf `in_reply_to` (eine Message-ID), wenn sie eine nennt.
    .w-it-goes-to-nobody-yet = Sie geht noch an niemanden

d-lnhvmglv-to = An

d-lnhvmglv-subject = Betreff

d-lnhvmglv-body = Text

d-lnhvmglv-cc = Cc

d-lnhvmglv-in_reply_to = Antwort auf

d-lnhvmglv-from = Von

d-lehcqyhl = Entwurf einer Nachricht~present

d-d6irjool = Thread-Eintrag
    .w-preview = Vorschau
    .w-flag = Markieren
    .w-unflag = Markierung entfernen
    .w-archive = Archivieren
    .w-delete = Löschen
    .w-delete-permanently = Endgültig löschen
    .w-reply = Antworten
    .w-reply-all = Allen antworten
    .w-forward = Weiterleiten
    .w-mark-as-read = Als gelesen markieren
    .w-mark-as-unread = Als ungelesen markieren
    .w-move-to-inbox = In den Posteingang
    .w-mute = Stumm schalten
    .w-snooze-an-hour = Eine Stunde zurückstellen

d-u5uuebo4 = Inspizieren
    .w-no-subject = Kein Betreff
    .w-unread = Ungelesen
    .w-it-stays-unread-until-its-conversation = Sie bleibt ungelesen, bis ihre Unterhaltung geöffnet wird.
    .w-to = an { $to }

d-z3itgb5e = Art der Störung
    .doc = Von welcher Art die Störung ist, damit das Fenster sagt, was hilft: `signed-out` (der Server hat das Passwort abgelehnt), `no-password` (das Konto hat auf diesem Gerät keines), `withheld` (die Regeln des Arbeitsbereichs haben es zurückgehalten), `offline` (das Gerät hat kein Netz) oder `unreachable` für alles andere.

d-ypxqazxd = Angekommen
    .doc = Wann zuletzt Mail im Posteingang dieser Unterhaltung ankam, während die Menschen, die sie lesen, noch nicht davon erfahren haben: der Zeitpunkt, für den die Ankündigung läuft, und nichts, wenn es keine gibt.

d-ualhak62 = Unterhaltung
    .doc = Die Unterhaltung, auf die sie antwortet, nach ihrem Schlüssel: leer für eine Nachricht, die auf keine antwortet.

d-cngng3tb = Server durchsuchen
    .doc = Bittet den Server des Kontos, seine Mail nach `text` zu durchsuchen: was Mail findet, die der Arbeitsbereich nicht hält, und Mail in Ordnern, die er nicht abgleicht.
    .w-say-what-to-look-for = Sag, wonach du suchst

d-hxawl5lh = Papierkorb leeren
    .doc = Löscht jede Unterhaltung im Papierkorb des Kontos endgültig aus dem Postfach und von seinem Server, jede wie `purge` es tut.
    .w-the-trash-is-empty = Der Papierkorb ist leer

d-3opocmel = In Entwürfen speichern
    .doc = Hält die Nachricht im Entwürfe-Ordner des Servers des Kontos, so wie sie gerade geschrieben ist, damit ein anderes Mailprogramm des Kontos sie dort auch sieht.
    .w-this-message-is-on-its-way = Diese Nachricht ist unterwegs
    .w-its-addresses-subject-or-words-are = Ihre Adressen, ihr Betreff oder ihre Worte sind länger, als eine Nachricht trägt
    .w-add-a-mail-account-in-settings = Füge zuerst in den Einstellungen ein Mailkonto hinzu

d-hlefh2br = Erneut versuchen
    .doc = Sendet eine Nachricht, die der Server des Kontos abgelehnt hat, erneut, von wo sie steht: die Fassung, die scheiterte, ist nicht fortgegangen, also geht sie einmal.
    .w-it-is-not-waiting-to-go = Sie wartet nicht darauf, rauszugehen
    .w-it-did-not-fail-or-nobody = Sie ist nicht gescheitert, oder niemand weiß, ob sie ging

d-ehxfqozw = Die angekommene Mail ankündigen
    .doc = Sagt allen, die die Unterhaltung lesen, dass Mail ankam: wer schrieb, wie sie heißt und an welches Konto sie kam, während der Arbeitsbereich mehrere liest, und öffnet die Unterhaltung.
    .w-no-mail-came-to-this-conversation = Seit der letzten Ankündigung kam keine Mail in diese Unterhaltung

d-jasftdqa = Ohne Konten
    .w-recovered-mail = Übernommene Mail
    .w-drafts = Entwürfe
    .w-mail = Mail
    .w-add-a-mail-account-in-settings = Füge in den Einstellungen ein Mailkonto hinzu
    .w-add-an-account-in-settings = Mailkonto in den Einstellungen hinzufügen

d-dbquqaba = Postausgangsliste
    .w-nothing-is-waiting-to-be-sent = Nichts wartet darauf, gesendet zu werden

d-jecmuxlv = Papierkorb-Leiste
    .w-delete-conversations-permanently = { $len } Unterhaltungen endgültig löschen?
    .w-empty-the-trash = Papierkorb leeren
    .w-this-can-t-be-undone = Das lässt sich nicht rückgängig machen.
    .w-every-conversation-in-it-is-deleted = Jede Unterhaltung darin wird aus diesem Postfach und vom Server gelöscht.
    .w-delete-permanently = Endgültig löschen
    .w-empty-trash = Papierkorb leeren

d-buu2swtx = Entwurfsliste
    .w-no-drafts = Keine Entwürfe

d-lidkonvr-filter-outbox = Postausgang

d-cngng3tb-text = Text

d-3opocmel-d = E

d-2kw66kbv = Server durchsuchen~present

d-365pn6ut = Papierkorb leeren~present

d-gipue46k = In Entwürfen speichern~present

d-rfonvbk2 = Erneut versuchen~present

d-uh5rorqx = Ankündigen~present

d-uyjucyg5 = Postausgang

d-at5x6rlq = Als Werbung markieren
    .doc = Markiert die Konversation als Werbung: ihre Nachrichten verlassen den Posteingang und gehen in den Werbeordner des Kontos, und was davon erneut kommt, bleibt dort.
    .w-it-is-in-the-junk-already = Sie ist schon in der Werbung

d-2w3gnzeg = Werbung~present

d-kyezjfsj = Bilder anzeigen
    .doc = Holt die Bilder herein, die die Seite des Absenders nennt: jedes wird von dort gelesen und im Inhalt des Arbeitsbereichs behalten, damit die Nachricht sie zeigt, wie sie geschrieben wurde.
    .w-this-message-is-not-written-as = Diese Nachricht ist nicht als Seite geschrieben

d-agv4f5yf = Immer seine Bilder zeigen
    .doc = Zeigt die Bilder jeder Nachricht, die `address` sendet, von jetzt an und in dem, was schon hier ist.
    .w-an-address-is-needed = Eine Adresse ist nötig

d-zzqmqnvv = Wieder fragen
    .doc = Fragt erneut, bevor gezeigt wird, was `address` sendet.

d-zzqmqnvv-address = Adresse

d-3ufitiaz = Bilder anzeigen~present

d-yxt3b36u = Absender vertrauen~present

d-c263r6w3 = Bilder
    .doc = Die Bilder, die die Seite ihres Absenders nennt, sobald jemand sie angefordert hat: im Inhalt des Arbeitsbereichs unter der Adresse abgelegt, mit der die Nachricht sie nannte.

d-nis6ckzm = Bild
    .doc = Ein Bild, das ein formatierter Teil einer Nachricht nennt, im Inhalt des Arbeitsbereichs abgelegt: was jemand mit der Bitte, die Bilder zu sehen, einholt und alle danach lesen.
    .noun = { $count ->
        [one] Bild
       *[other] Bilder
    }

d-gb5hqtag = Quelle
    .doc = Die Adresse, mit der die Nachricht es nannte, wie ihr HTML sie schrieb.

d-7hhcy4ay = Bild
    .doc = Seine Bytes, im Inhalt des Arbeitsbereichs abgelegt.

d-nx5cmagd = Vertrauenswürdiger Absender
    .doc = Eine Adresse, deren Bilder immer gezeigt werden: was jemand einmal über einen Absender sagt, dessen Post sie so sehen will, wie er sie geschickt hat.
    .noun = { $count ->
        [one] vertrauenswürdiger Absender
       *[other] vertrauenswürdige Absender
    }

d-5eoon66w = Adresse

d-zzpnzlp2 = Vertrauenswürdig
    .doc = Die Absender, deren Bilder ohne Nachfrage gezeigt werden, nach ihrer Adresse.

d-agv4f5yf-address = Adresse

d-dsurl5gh = Link öffnen
    .doc = Öffnet einen `mailto:`-Link: eine Nachricht an den, den er nennt, in den Entwürfen, damit die Person sie liest und sendet. Ein Link, der keine Adresse nennt, öffnet nichts, da er niemanden zum Schreiben nennt. Der Host veröffentlicht dies als `open-link` des Systems am Postfach, wohin eine Shell einen Link gibt.

d-dsurl5gh-link = Link

d-b7h5czv4 = Links dieser Nachricht
    .doc = Die Links, die eine Nachricht nennt, jeden einmal, in der Reihenfolge, in der sie stehen: wohin ihr formatierter Teil verlinkt, oder die Web- und Mail-Adressen, die ihre Worte tragen.

d-syjlwh4r = Link öffnen
    .doc = Öffnet einen der Links einer Nachricht in der App, die Links ihres Schemas öffnet, wie ein Druck auf einen Link: den Browser für eine Web-Adresse. Eine Nachricht öffnet nur einen Link, den sie nennt.
    .w-the-message-has-no-such-link = Die Nachricht hat keinen solchen Link

d-syjlwh4r-url = Adresse

d-bs2m6ms4 = Link öffnen~present

d-unyf7jiq = Link kopieren
    .doc = Kopiert einen der Links einer Nachricht, die Adresse allein, wie eine Person sie weitergibt.
    .w-the-message-has-no-such-link = Die Nachricht hat keinen solchen Link

d-unyf7jiq-url = Adresse

d-cj2mz2rq = Link kopieren~present

d-gwzlmpqh = Vertrauenswürdige Absender
    .w-no-sender-s-pictures-are-always = Von keinem Absender werden Bilder immer angezeigt
    .w-ask-again = Wieder fragen

d-h7kwugun = Anhang öffnen
    .doc = Öffnet eine Datei, die eine Nachricht mitbrachte, in der App, die ihre Art öffnet, wie Files eine öffnet: die Datei erreicht die App als derselbe Inhalt, den beide lesen, nie als Pfad, den keiner hat.

d-h7kwugun-name = Name

d-hrcqmsg3 = Wartende Nachricht
    .w-take-back = Zurücknehmen
    .w-try-again = Erneut versuchen
