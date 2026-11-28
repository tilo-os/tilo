# Kontakte auf Deutsch. Keys sind die stabilen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-contacts = Kontakte

d-g4vf2are = Eintrag
    .doc = Eine Möglichkeit, jemanden zu erreichen, und was sie ist: eine Telefonnummer, die die private ist, ein Postfach, das das der Arbeit ist.
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }

d-fzyd2a52 = Bezeichnung
    .doc = Was für eine Nummer oder Adresse es ist: privat, Arbeit, mobil.

d-b4cilqkx = Wert

d-cxjt3f6x = Adresse
    .doc = Wohin die Post einer Person geht, so geschrieben, wie Post hier adressiert wird.
    .noun = { $count ->
        [one] Adresse
       *[other] Adressen
    }

d-5iipld72 = Bezeichnung
    .doc = Was für ein Ort es ist: privat, Arbeit.

d-zpxsouby = Straße

d-txsphfy2 = Stadt

d-g55zwvsh = Region

d-xuayok2l = Postleitzahl

d-xlbwdlve = Land

d-w5adcbsy = Kontakt
    .doc = Jemand, mit dem der Arbeitsbereich in Verbindung bleibt.
    .noun = { $count ->
        [one] Kontakt
       *[other] Kontakte
    }
    .untitled = Noch kein Name

d-zut4kvim = Name

d-rfhxkccf = Organisation
    .doc = Wo sie arbeiten.

d-7wq5grnt = Rolle
    .doc = Was sie dort tun.

d-dg7b4wfh = Website

d-fc62qk5v = Geburtstag
    .doc = Der Tag ihrer Geburt; das Jahr kann ihr Geburtsjahr sein oder fehlen.

d-4lfazbdq = Notizen
    .doc = Alles, was über sie festzuhalten lohnt.

d-lurugxra = Telefonnummern

d-hfbs4gxe = E-Mail-Adressen

d-orxuftuv = Adressen

d-vea52w55 = Gruppen
    .doc = Die Gruppen, in denen sie sind, nach Namen: eine Gruppe besteht, solange ihr Name bei jemandem steht.

d-ur3qshor = Favorit

d-kvhikhu4 = Foto
    .doc = Ihr Bild, nach dem Ort seiner Bytes: eine `data:`-Adresse, die ein Foto in einer vCard trug, oder eine Inhaltsadresse, die jemand hier hinterlegt hat.

d-szrijd7k = Person
    .doc = Die id der Person, die das Verzeichnis des Arbeitsbereichs für diese Karte hält, sobald sie mit den Apps geteilt ist; leer bis dahin und nachdem sie nicht mehr geteilt wird.

d-w5adcbsy-filter-favorites = Favoriten

d-w5adcbsy-filter-groups = Gruppen

d-qeo6ddcm = Gruppe
    .doc = Eine Gruppe, die das Buch als Ort anbietet: die Menschen darin, über ein gemeinsames Wort versammelt.
    .noun = { $count ->
        [one] Gruppe
       *[other] Gruppen
    }

d-jn6l7z6v = Name

d-etcwj7o5 = Adressbuch
    .doc = Das Adressbuch des Arbeitsbereichs.
    .noun = { $count ->
        [one] Adressbuch
       *[other] Adressbücher
    }

d-qfj7l6eu = Kontakte

d-uyyz5pg2 = Gruppen

d-glpbyz4c = Feld fürs Zusammenführen
    .doc = Was ein Zusammenführen von der Karte nehmen darf, die geht: die Werte, von denen jede Karte einen hat.

d-ftc5rtae = Ihr Name

d-44vcd3nc = Wo sie arbeiten

d-flwktecf = Was sie dort tun

d-foct2e7s = Ihre Website

d-feye5m7e = Der Tag ihrer Geburt

d-5m5rhfwk = Was über sie geschrieben ist

d-smciqs6j = Neuer Kontakt
    .doc = Ein neuer Kontakt, benannt nach dem, was dort getippt wurde, wo er entstand; der Eintrag eines Buchs gibt den Namen, den er bekommt, damit nichts „Noch kein Name“ bleibt, wenn jemand einen getippt hat.
    .did = hat einen Kontakt hinzugefügt

d-smciqs6j-name = Name

d-zwp7xkyi = Umbenennen
    .doc = Benennt einen Kontakt um.
    .did = hat einen Kontakt umbenannt

d-zwp7xkyi-name = Name

d-kp6evyhz = Zu Favoriten hinzufügen
    .doc = Setzt einen Kontakt zu den Favoriten oder nimmt ihn heraus.
    .w-add-to-favorites = Zu Favoriten hinzufügen
    .w-remove-from-favorites = Aus Favoriten entfernen

d-kp6evyhz-on = An

d-2ctxgtyd = Telefonnummer hinzufügen
    .doc = Gibt einem Kontakt eine Telefonnummer, als Nummer `number` und Art `label`, wo der Aufrufer sie schon kennt.
    .did = hat eine Telefonnummer hinzugefügt

d-2ctxgtyd-number = Nummer

d-2ctxgtyd-label = Bezeichnung

d-spe2pxo4 = Telefonnummer entfernen
    .doc = Nimmt eine der Telefonnummern eines Kontakts weg.

d-spe2pxo4-phone = Telefon

d-ts6f5mgs = E-Mail-Adresse hinzufügen
    .doc = Gibt einem Kontakt ein Postfach, als `address` und Art `label`, wo der Aufrufer sie schon kennt.
    .did = hat eine E-Mail-Adresse hinzugefügt

d-ts6f5mgs-address = Adresse

d-ts6f5mgs-label = Bezeichnung

d-infpb6bb = E-Mail-Adresse entfernen
    .doc = Nimmt eines der Postfächer eines Kontakts weg.

d-infpb6bb-mail = E-Mail

d-qgoswgkz = Adresse hinzufügen
    .doc = Gibt einem Kontakt eine Postadresse zum Ausfüllen.
    .did = hat eine Adresse hinzugefügt

d-xr3q4wsk = Adresse entfernen
    .doc = Nimmt eine der Adressen eines Kontakts weg.

d-xr3q4wsk-address = Adresse

d-pa7wwp6r = Zu Gruppe hinzufügen
    .doc = Setzt einen Kontakt in eine Gruppe und macht die Gruppe zum Ort des Buchs, wenn sie neu ist: dieselbe Gruppe, wie sie auch geschrieben wird, damit „Arbeit“ und „arbeit“ ein Ort sind.
    .w-a-group-needs-a-name = Eine Gruppe braucht einen Namen

d-pa7wwp6r-c = C

d-pa7wwp6r-name = Name

d-qafligvf = Aus Gruppe entfernen
    .doc = Nimmt einen Kontakt aus einer Gruppe.

d-qafligvf-name = Name

d-vi6ajqma = Neue Gruppe
    .doc = Beginnt eine Gruppe, die das Buch als Ort anbietet, noch mit niemandem darin.
    .w-a-group-needs-a-name = Eine Gruppe braucht einen Namen

d-vi6ajqma-name = Name

d-ivtvy7sy = Löschen
    .doc = Löscht einen Kontakt aus dem Buch und mit ihm die Person, die die Apps des Arbeitsbereichs von ihm kennen, wo das Fenster das Verzeichnis übergibt: womit Mail, Kalender und Nachrichten ihn erreichen, geht mit.
    .did = hat einen Kontakt gelöscht

d-ivtvy7sy-c = C

d-ivtvy7sy-directory = Verzeichnis

d-76ulubho-directory = Verzeichnis

d-76ulubho = Kontakte zusammenführen
    .doc = Führt die Karte, die geht, in die, die bleibt: jede Nummer, jedes Postfach, jede Adresse und jede Gruppe beider sind die Karte, die bleibt, die Felder aus `take` kommen von der Karte, die geht, und die Karte, die geht, wird gelöscht.
    .did = hat ein Duplikat zusammengeführt
    .w-a-contact-cannot-be-merged-into = Ein Kontakt kann nicht in sich selbst zusammengeführt werden

d-76ulubho-keep = Bleibt

d-76ulubho-goes = Geht

d-76ulubho-take = Nimm

d-4jncxnhw = Als vCard exportieren
    .export = vCard
    .doc = Ein Kontakt als Datei: seine Karte, nach ihm benannt.

d-4jncxnhw-version = Format

d-5ceccmwk = Alle als vCard exportieren
    .export = vCard 4.0
    .doc = Jeder Kontakt als eine vCard-Datei, die ein anderes Adressbuch ganz liest.

d-5ceccmwk-version = Format

d-mcisoxmg = Alle als vCard 3.0 exportieren
    .export = vCard 3.0
    .doc = Jeder Kontakt als eine vCard-3.0-Datei, die ein Adressbuch öffnet, das keine 4.0-Karte liest.

d-tf7lcwop = Importe
    .doc = Der Import, der gerade läuft, falls einer läuft: Seine Zeile sagt, wie weit die Datei gelesen wurde.

d-6bgl3vfx = Import starten~present

d-jxsjnwj2 = vCards importieren
    .doc = Startet den Import einer vCard-Datei: Die Datei bleibt als der laufende Import, und die Zeile des Fensters zeigt, wo er steht, während `step_import` seine Karten satzweise übernimmt.
    .w-the-file-is-larger-than-the = Die Datei ist größer als die 8 MB, die diese App importiert

d-jxsjnwj2-file = Datei

d-4ywckc76 = Die nächsten Karten eines Imports übernehmen
    .doc = Übernimmt den laufenden Import: liest die Datei einmal und dann einen Satz ihrer Karten, damit das Fenster zwischen den Sätzen zeichnet.

d-btp4fqga = vCards jetzt importieren
    .doc = Importiert die Kontakte, die eine vCard-Datei enthält, ins Buch, jede neue Karte ein eigener Kontakt, und antwortet, was er getan hat. Eine Karte, deren Kennung das Buch schon unverändert führt, ist niemand Neues; eine, die etwas anderes über die Person sagt, wartet in der Prüfung des Buchs, und der Kontakt bleibt, wie er ist, bis jemand sagt, welche Karte die eigene ist. Eine Datei über dem, was diese App liest, wird ganz abgelehnt: die Ablehnung sagt, warum.
    .did = hat Kontakte importiert
    .w-the-file-is-larger-than-the = Die Datei ist größer als die 8 MB, die diese App importiert

d-btp4fqga-file = Datei

d-6vh47rvy = Kontakte hinzufügen~present

d-lcnj23sa = Kontakte hinzufügen
    .doc = Die Karten `cards`, die Worte einer vCard-Datei, als Kontakte dieses Buchs und was aus jeder wurde: was ein Import, ein Agent oder ein Skript einem Buch in einem Aufruf übergibt.
    .did = fügte Kontakte hinzu
    .w-the-file-holds-more-than-the = Die Datei enthält mehr als die 2048 Karten, die diese App importiert

d-lcnj23sa-cards = Karten

d-l64bjt6g = Eine Karte einer vCard hinzufügen
    .doc = Der Kontakt, den eine vCard-Karte nennt, im Buch angelegt, oder keiner für eine Karte, die niemanden nennt.
    .did = hat eine Karte hinzugefügt

d-l64bjt6g-card = Karte

d-xdqrpoil = Den Kontakt für eine Adresse finden
    .doc = Der Kontakt, den das Buch für `address` hält, als Aktion: wonach eine Person, ein Agent oder eine andere App fragt, bevor sie dorthin schreibt.

d-xdqrpoil-address = Adresse

d-jz6v4veg = Konto verknüpfen
    .doc = Bindet `account` von `service` an die Person: das `confirm-endpoint` des Verzeichnisses, oder `confirm-account`, wo der Dienst einen Kontobereich nennt.
    .did = hat ein Konto verknüpft
    .w-a-service-names-the-account-s = Ein Dienst nennt die Art des Kontos
    .w-an-account-address-or-identifier-is = Eine Kontoadresse oder Kennung ist nötig

d-jz6v4veg-d = V

d-jz6v4veg-person = Person

d-jz6v4veg-service = Dienst

d-jz6v4veg-scope = Bereich

d-jz6v4veg-account = Konto

d-o2qwok2w = Verknüpfung ändern
    .doc = Ändert das Konto, das eine Verknüpfung nennt, nach der Regel des Verzeichnisses, dass eine geänderte Adresse geprüft werden muss, bevor etwas dorthin geht.
    .did = hat eine Verknüpfung geändert
    .w-an-account-address-or-identifier-is = Eine Kontoadresse oder Kennung ist nötig

d-o2qwok2w-d = V

d-o2qwok2w-person = Person

d-o2qwok2w-endpoint = Endpunkt

d-o2qwok2w-service = Dienst

d-o2qwok2w-scope = Bereich

d-o2qwok2w-account = Konto

d-rlngk4rk = Verknüpfung entfernen
    .doc = Nimmt eine Verknüpfung weg: Nachrichten von diesem Konto erreichen diesen Kontakt nicht mehr, und was schon angekommen ist, bleibt.
    .did = hat eine Verknüpfung entfernt

d-rlngk4rk-d = V

d-rlngk4rk-person = Person

d-rlngk4rk-endpoint = Endpunkt

d-nhasu5my = Diese Karte teilen
    .doc = Teilt diese Karte mit den Apps des Arbeitsbereichs, unter der Person, die sie ist: den Namen, die Nummern und die Postfächer, die eine andere App von einem Kontakt liest, bevor sie dorthin schreibt.
    .did = hat eine Karte geteilt

d-nhasu5my-d = V

d-nhasu5my-person = Person

d-nhasu5my-c = C

d-4oatdkw2 = Diese Karte teilen
    .doc = Teilt eine Karte, die die Apps des Arbeitsbereichs noch nicht kennen: eine eigene Person wird für sie angelegt, damit Mail, Kalender und Nachrichten sie nennen und über ihre Nummern und Postfächer finden können.
    .did = hat eine Karte geteilt

d-4oatdkw2-d = T

d-4oatdkw2-c = C

d-kqf6yzpo = Diese Karte nicht mehr teilen
    .doc = Nimmt den Kontakt aus dem Verzeichnis und lässt seine Karte im Buch: was die Apps von ihm lesen, nennt ihn nicht mehr, und die Karte ist keine Person des Verzeichnisses mehr, bis sie erneut geteilt wird.
    .did = hat eine Karte nicht mehr geteilt

d-kqf6yzpo-d = V

d-kqf6yzpo-c = C

d-kqf6yzpo-person = Person

d-ts4v4mxm = Favorit~present

d-oqpua326 = Gruppe~present

d-dysygpdo = Neue Gruppe~present

d-qwoobxzv = Zusammenführen in~present

d-dd4gcdnc = vCard importieren~present

d-6o6vmpkz = Adressbuchfenster
    .w-to-review = Zur Prüfung
    .w-dismiss = Schließen
    .w-importing = { $filename } wird importiert
    .w-reading-the-file = Die Datei wird gelesen…
    .w-of-cards = { $done } von { $total } Karten
    .w-sort = Sortieren
    .w-by-name = Nach Name
    .w-by-organization = Nach Organisation
    .w-contacts = Kontakte
    .w-all-contacts = Alle Kontakte
    .w-file = Datei
    .w-import-vcards = vCards importieren…
    .w-new-contact = Neuer Kontakt
    .w-no-contacts-yet = Noch keine Kontakte
    .w-nobody-here = Niemand hier
    .w-other = Sonstige

d-dfmc6gmk = Kontaktzeile

d-nhhj3wnt = Kontaktseite
    .w-name = Name
    .w-favorite = Favorit
    .w-organization = Organisation
    .w-where-they-work = Wo sie arbeiten
    .w-job-title = Position
    .w-what-they-do-there = Was sie dort tun
    .w-website = Website
    .w-https = https://
    .w-birthday = Geburtstag
    .w-the-year = Das Jahr
    .w-part-of-it = Gehört dazu
    .w-left-out = Ohne Jahr
    .w-phones = Telefonnummern
    .w-email = E-Mail
    .w-addresses = Adressen
    .w-groups = Gruppen
    .w-connected-accounts = Verknüpfte Konten
    .w-possibly-the-same-person = Möglicherweise dieselbe Person
    .w-notes = Notizen
    .w-anything-worth-remembering-about-them = Alles, was über sie festzuhalten lohnt
    .w-delete-this-contact = Diesen Kontakt löschen

d-6ltzv265 = Telefonnummern
    .w-no-numbers-yet = Noch keine Nummern.
    .w-home = Zuhause
    .w-phone-number = Telefonnummer
    .w-remove-phone = Telefonnummer entfernen
    .w-add-phone = Telefonnummer hinzufügen
    .w-call = Anrufen
    .w-text = Nachricht
    .w-nothing-to-dial-in-this-number = In dieser Nummer gibt es nichts zu wählen

d-qkxolihp = E-Mail-Adressen
    .w-write = Schreiben
    .w-not-an-address-a-message-can = Keine Adresse, an die eine Nachricht gehen kann
    .w-no-mailboxes-yet = Noch keine Postfächer.
    .w-work = Geschäftlich
    .w-email-address = E-Mail-Adresse
    .w-remove-email = E-Mail-Adresse entfernen
    .w-add-email = E-Mail-Adresse hinzufügen

d-5dnmy6vv = Adressen
    .w-no-addresses-yet = Noch keine Adressen.
    .w-home = Zuhause
    .w-street = Straße
    .w-remove-address = Adresse entfernen
    .w-city = Stadt
    .w-region = Region
    .w-postal-code = Postleitzahl
    .w-country = Land
    .w-add-address = Adresse hinzufügen

d-znqezbnm = Duplikate
    .w-merge-into-this-card = In diese Karte zusammenführen
    .w-they-share = Sie teilen { $shared_with }.
    .w-select-all = Alle auswählen
    .w-select-none = Keine auswählen
    .w-take-their-name = Namen übernehmen
    .w-take-their-organization = Organisation übernehmen
    .w-take-their-job-title = Position übernehmen
    .w-take-their-website = Website übernehmen
    .w-take-their-birthday = Geburtstag übernehmen
    .w-take-their-notes = Notizen übernehmen
    .w-name = Name
    .w-organization = Organisation
    .w-job-title = Position
    .w-website = Website
    .w-birthday = Geburtstag
    .w-notes = Notizen
    .w-phone-numbers = Telefonnummern
    .w-email-addresses = E-Mail-Adressen
    .w-groups = Gruppen
    .w-this-card = Diese Karte
    .w-the-other-card = Die andere Karte

d-425344oj = Gruppen
    .w-not-in-a-group = In keiner Gruppe.
    .w-add-to-a-group = Zu einer Gruppe hinzufügen

d-7ka3b3ve = Kontakte
    .w-no-contacts-yet = Noch keine Kontakte

d-4mv5jww4 = Verknüpfungen
    .w-no-account-of-the-workspace-is = Noch kein Konto des Arbeitsbereichs ist dieser Kontakt.
    .w-associated = Verknüpft
    .w-needs-review = Prüfung nötig
    .w-service-account = Dienstkonto { $scope }
    .w-change = Ändern
    .w-remove-association = Verknüpfung entfernen
    .w-service-such-as-messages = Dienst, etwa Messages
    .w-service-account-where-the-service-names = Dienstkonto, wo der Dienst eines nennt
    .w-account-address-or-identifier = Kontoadresse oder Kennung
    .w-keep-this-account = Dieses Konto behalten
    .w-cancel = Abbrechen
    .w-share-this-card-with-the-apps = Diese Karte mit den Apps teilen
    .w-stop-sharing-this-card = Diese Karte nicht mehr teilen
    .w-the-workspace-s-apps-do-not = Die Apps des Arbeitsbereichs kennen diesen Kontakt noch nicht. Die Karte zu teilen lässt sie ihn nennen; ein Konto wird durch Prüfen verknüpft.

d-wio6wz72 = Prüfen
    .w-service-such-as-messages = Dienst, etwa Messages
    .w-service-account-where-the-service-names = Dienstkonto, wo der Dienst eines nennt
    .w-account-address-or-identifier = Kontoadresse oder Kennung
    .w-review-this-account = Dieses Konto prüfen

d-doi7lzy6 = Einwilligung
    .w-tilo-does-not-check-that-the = Tilo prüft nicht, ob das Konto wirklich ihres ist. Es zu verknüpfen teilt keine Dateien und gewährt keine Rechte.

d-6kvgemtj = Bestätigen~present

d-bhmovs5j = Ändern~present

d-bfuqzfia = Kontakt finden oder hinzufügen
    .doc = Sucht den Kontakt, den der Arbeitsbereich unter `name` führt, oder legt einen an: was jemanden aus einem anderen Dienst , etwa einer Aufgabe, die ihm vor seinem Beitritt gegeben wurde, auf eine Karte bringt, egal wie viele Importe ihn nennen.
    .did = hat einen Kontakt gefunden
    .w-name-the-contact = Kontakt benennen

d-so-tilo-contacts-keywords = kontakte, adressbuch, personen, telefon, e-mail, vcard

d-bfuqzfia-name = Name

d-zfqz2htt = Benannt oder neu~present

d-q3um3t7j = Kennung
    .doc = Die eigene Kennung der Karte in dem Format, das sie herbrachte (die `UID` einer vCard): woran ein erneutes Lesen derselben Datei sie findet, damit ein Buch einmal importiert wird, so oft seine Datei auch gelesen wird.

d-35oinh4z = Importkarte
    .untitled = Eine Karte zur Prüfung
    .doc = Eine Karte, die eine vCard-Datei enthielt und deren Kennung ein Kontakt des Buchs schon mit anderem Inhalt hat: Das Buch führt sie, bis jemand sagt, welche Karte die Person ist, und nichts an dem Kontakt, den das Buch führt, ändert sich vorher.
    .noun = { $count ->
        [one] Importkarte
       *[other] Importkarten
    }

d-op5ercnd = Karte
    .doc = Die Worte, die die Datei für die Karte schrieb, erneut gelesen, wenn jemand sie übernimmt.

d-a4s7qvbf = Name
    .doc = Wie die Karte die Person nennt, damit eine Prüfung sie benennt.

d-kcnlll33 = Kennung
    .doc = Die Kennung, die die Karte in ihrem Format führt: der Kontakt des Buchs, der sie zu sein sagt.

d-rsywb3bl = Bestehender Kontakt
    .doc = Der Kontakt des Buchs, dessen Kennung sie nennt, solange das Buch ihn führt.

d-35oinh4z-filter-existing = Bestehender Kontakt

d-5zvrpq56 = Importbericht
    .noun = { $count ->
        [one] Importbericht
       *[other] Importberichte
    }
    .doc = Was ein Import tat: wie viele Karten das Buch übernahm, wie viele es schon führte und wie viele warten, bis jemand sagt, welche Karte die Person ist.

d-ydus2juj = Importiert
    .doc = Wie viele Karten zu Kontakten wurden.

d-bqy264j6 = Unverändert
    .doc = Wie viele Karten das Buch schon unverändert führte.

d-jm2un2fr = Zu prüfen
    .doc = Wie viele Karten einen Kontakt des Buchs ändern und in der Prüfung des Buchs warten.

d-sikc2jgn = Nicht gelesen
    .doc = Wie viele Karten die Datei enthielt, die diese App nicht übernehmen konnte.

d-azh3qbqs = Gesehen
    .doc = Ob die Person das Ergebnis gelesen hat: ein gelesener Bericht ist kein Hinweis mehr.

d-jzj4nmcl = Ausgang je Karte
    .doc = Was eine Karte einer Datei für das Buch ist.

d-ry76oxel = Übernommen
    .doc = Die Karte ist jetzt ein Kontakt.

d-g5ihbvrg = Vorhanden
    .doc = Das Buch führte sie schon, unverändert.

d-rnflvy5o = Zu prüfen
    .doc = Die Karte ändert einen Kontakt des Buchs und wartet in der Prüfung des Buchs.

d-t7zhhkyy = Übersprungen
    .doc = Die Karte nennt niemanden, den diese App übernehmen kann.

d-zc4aqhgg = Zur Prüfung
    .doc = Karten einer Datei, deren Kennung das Buch schon mit anderem Inhalt hat: jede wartet, bis jemand sagt, welche Karte die Person ist.

d-5zvrpq56-filter-imported = Importiert

d-5zvrpq56-filter-unchanged = Unverändert

d-5zvrpq56-filter-awaiting = Zu prüfen

d-5zvrpq56-filter-skipped = Nicht gelesen

d-5zvrpq56-filter-seen = Gesehen

d-i7k6s5iv = Importlauf
    .doc = Ein Import, der seine Datei liest und ihre Karten übernimmt, damit das Fenster zeigt, wo er steht: ein Satz Karten nach dem anderen, die Wörter der Datei nur bis zur letzten Karte behalten.
    .noun = { $count ->
        [one] Importlauf
       *[other] Importläufe
    }

d-gb4ogkr2 = Dateiname
    .doc = Wie die Datei heißt, wie die Zeile des Fensters sie nennt.

d-lsfbimes = Datei
    .doc = Die Datei, die gelesen wird.

d-3nstjakm = Karten
    .doc = Die Karten, die die Datei enthält; leer, bis sie gelesen wurde.

d-rojyhv53 = Gesamt
    .doc = Wie viele Karten die Datei enthält: `-1`, bis die Datei gelesen wurde.

d-frkp37sw = Erledigt
    .doc = Wie viele Karten übernommen wurden.

d-qd4vubqa = Importiert
    .doc = Was bis jetzt aus ihnen wurde.

d-hqshoqch = Unverändert

d-sd56tbl5 = Wartend

d-p3ealiag = Nicht gelesen

d-i7k6s5iv-filter-total = Gesamt

d-i7k6s5iv-filter-done = Erledigt

d-i7k6s5iv-filter-imported = Importiert

d-i7k6s5iv-filter-unchanged = Unverändert

d-i7k6s5iv-filter-awaiting = Wartend

d-i7k6s5iv-filter-skipped = Nicht gelesen

d-etcwj7o5-filter-last_import = Letzter Import

d-scky6ngy = Letzter Import
    .doc = Was der letzte Import tat, bis die Person ihn gelesen hat.

d-5ekvqkm6 = Importbericht schließen
    .doc = Die Person hat gelesen, was der letzte Import tat: Der Hinweis zeigt es nicht mehr.

d-eng26pua = Eine Karte einer vCard einsortieren
    .doc = Bringt eine Karte einer vCard-Datei ins Buch: übernimmt sie als Kontakt, findet das Buch sie schon unverändert, legt sie zur Prüfung vor, wenn sie etwas anderes über einen Kontakt mit derselben Kennung sagt, oder lehnt eine Karte ab, die niemanden nennt. `uid` ist die Kennung, die der Aufrufer einer Karte gibt, deren Datei keine nennt, damit er den Kontakt wiederfindet, den er macht.
    .did = hat eine Karte einsortiert

d-eng26pua-card = Karte

d-eng26pua-uid = Kennung

d-ktpgvkx4 = Als neuen Kontakt importieren
    .doc = Übernimmt eine Karte, die zur Prüfung wartete, als eigenen Kontakt neben dem, den das Buch unter der genannten Kennung führt: Die Karte der Datei ist die der Person, und die Karte, die das Buch behielt, behält alles Eigene. Ihre Kennung ist neu, weil die beiden Karten jetzt zwei sind.
    .did = hat eine Karte als neuen Kontakt importiert

d-ktpgvkx4-p = Zu prüfende Karte

d-26oiuqrc = Bestehenden Kontakt behalten
    .doc = Lässt den Kontakt, den das Buch führt, wie er ist, und nimmt die Karte der Datei aus der Prüfung.
    .did = hat einen bestehenden Kontakt behalten

d-26oiuqrc-p = Zu prüfende Karte

d-srkiqvld = Import ablehnen
    .doc = Der Bericht eines Imports von `skipped` Karten und sonst nichts: was eine Datei antwortet, die nicht gelesen werden konnte.

d-srkiqvld-skipped = Nicht gelesen

d-yq5wfs7z = Import berichten
    .doc = Was ein Import tat, am Buch geführt, damit der Hinweis des Fensters es liest und beantwortet.

d-yq5wfs7z-imported = Importiert

d-yq5wfs7z-unchanged = Unverändert

d-yq5wfs7z-awaiting = Zu prüfen

d-yq5wfs7z-skipped = Nicht gelesen

d-t6oakqpq = Eine Karte einer vCard übernehmen
    .doc = Der Kontakt, den eine Karte im Buch macht, unter der Kennung `id`: was eine neue Karte wird und was eine Karte wird, die jemand über die Karte des Buchs gewählt hat. Ihre Zeilen werden gefüllt, nachdem sie im Buch steht, wie bei jeder Ressource.
    .did = hat eine Karte übernommen

d-t6oakqpq-card = Karte

d-t6oakqpq-id = Kennung

d-26oqffl5 = Karten prüfen
    .w-nothing-waits-for-review = Nichts wartet auf Prüfung
    .w-these-cards-name-a-contact-the = Diese Karten nennen einen Kontakt, den das Buch führt, und sagen etwas anderes über ihn. Übernimm eine Karte als eigenen Kontakt, oder lass die Karte des Buchs, wie sie ist.

d-feo5aoss = Zu prüfende Karte
    .w-the-book-keeps-under-the-same = Das Buch führt { $other_name } unter derselben Kennung.
    .w-name = Name
    .w-organization = Organisation
    .w-job-title = Position
    .w-website = Website
    .w-notes = Notizen
    .w-phone-numbers = Telefonnummern
    .w-email-addresses = E-Mail-Adressen
    .w-groups = Gruppen
    .w-in-the-book = Im Buch
    .w-in-the-card = In der Karte
    .w-the-contact-it-named-is-gone = Der Kontakt, den sie nannte, ist nicht mehr im Buch; die Karte kann trotzdem für sich hereinkommen.
    .w-import-as-new = Als neu importieren
    .w-keep-existing = Bestehenden behalten

d-z62dycv3 = Gegenüberstellung

d-32tuvb6l = Mit Jahr
    .doc = Ob das Jahr zum Geburtstag gehört: ein ohne Jahr geführter Geburtstag ist der Tag, auf den er jedes Jahr fällt, wie ihn viele Adressbücher führen.

d-2bwzp3iz = Reihenfolge
    .doc = Die Reihenfolgen, in denen das Fenster die Menschen eines Buchs auflistet: nach Name und danach, wo sie arbeiten.

d-g7sih7as = Name
    .doc = Nach Name, ohne auf Groß- und Kleinschreibung zu achten.

d-oejs3zct = Organisation
    .doc = Nach der Organisation, bei der sie arbeiten, und innerhalb einer nach Name.

d-5xqjhh2n = vCard-Format
    .doc = Die Versionen des Kartenformats, die ein anderes Adressbuch liest.

d-ipm55zgk = vCard 3.0
    .doc = vCard 3.0, das ältere Adressbücher lesen: Daten mit Bindestrichen und Arten in Großbuchstaben.

d-4vlms6uf = vCard 4.0
    .doc = vCard 4.0, das aktuelle Format: Daten in seiner kurzen Form und Arten in Kleinschreibung.

d-brgsmbes = Monatswort
    .w-january = Januar
    .w-february = Februar
    .w-march = März
    .w-april = April
    .w-may = Mai
    .w-june = Juni
    .w-july = Juli
    .w-august = August
    .w-september = September
    .w-october = Oktober
    .w-november = November
    .w-december = Dezember

d-egcmsdld = Importtitel
    .w-no-contacts-were-imported = Es wurden keine Kontakte importiert
    .w-imported-1-contact = 1 Kontakt importiert
    .w-imported-contacts = { $imported } Kontakte importiert

d-y6pio4ne = Importworte
    .w-already-current = { $unchanged } bereits aktuell
    .w-1-card-needs-review = 1 Karte muss geprüft werden
    .w-cards-need-review = { $awaiting } Karten müssen geprüft werden
    .w-could-not-be-read = { $skipped } konnten nicht gelesen werden

d-qekysgef = Das Jahr des Geburtstags
    .doc = Führt einen Geburtstag mit seinem Jahr oder ohne: Ein ohne Jahr geführter Geburtstag ist der Tag, auf den er jedes Jahr fällt, wie ihn viele Adressbücher führen.
    .did = hat das Jahr des Geburtstags geändert

d-qekysgef-on = An

d-jdajyfbl = Jahr des Geburtstags~present

d-inkvofy4 = Den vCard-Text exportieren
    .doc = Jeder Kontakt als Text einer Kartendatei, oder der eine, den `name` nennt: für einen Agenten oder ein Skript, das die Karten weitergibt, wo der Dateiauswähler einer Person es nicht kann, als die Adresse, unter der die Bytes liegen. Ein Name, den das Buch zweimal führt, wird abgelehnt statt geraten.
    .w-name-one-contact-the-book-keeps = Einen Kontakt nennen, den das Buch unter diesem Namen führt, oder den Namen weglassen

d-inkvofy4-name = Name

d-do6vedet = vCard-Text exportieren~present
