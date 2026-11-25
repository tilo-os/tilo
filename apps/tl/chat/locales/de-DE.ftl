# Chat auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-chat = Chat

d-lmdoxbnn = Unterhaltungen
    .doc = Die Unterhaltungen des Arbeitsbereichs in der Reihenfolge der Seitenleiste: Kanäle außerhalb jeder Kategorie, die Kategorien mit ihren eigenen, und die Direktnachrichten, in der Liste als Personen.
    .noun = { $count ->
        [one] Unterhaltung
       *[other] Unterhaltungen
    }

d-5kzdptyg = Kanäle

d-n2xgh234 = Fähigkeiten

d-fe2st4jb = Kategorie
    .doc = Eine benannte Gruppe von Kanälen in der Seitenleiste.
    .noun = { $count ->
        [one] Kategorie
       *[other] Kategorien
    }

d-hbhrds5c = Name

d-hvqgmwrk = Kanäle
    .doc = Die Kanäle des Servers.

d-q7llygsa = Textkanal
    .doc = Nachrichten in einem Strom; jede Nachricht kann einen Thread beginnen.
    .noun = { $count ->
        [one] Textkanal
       *[other] Textkanäle
    }

d-x3haugpo = Name

d-u6b35mgm = Thema

d-ksuxpopi = Nachrichten

d-ztou5nin = Threads
    .doc = Die Threads, die ihre Nachrichten begonnen haben, jeder der, den `thread` einer Nachricht nennt.

d-hk2t6bvk = Nachricht
    .doc = Eine Nachricht.
    .noun = { $count ->
        [one] Nachricht
       *[other] Nachrichten
    }

d-xfrqdy3y = Text

d-4uwxpnw2 = Antwort auf
    .doc = Die Nachricht, die diese hier im Text beantwortet, über ihr zitiert.

d-kxekobh6 = Thread
    .doc = Die Seitenunterhaltung, die von dieser Nachricht abzweigt.

d-mwily5kw = Teile
    .doc = Was eine Nachricht sagte, bevor Nachrichten Rich Text waren, so aufbewahrt, wie es geschrieben wurde.

d-4zr7hti6 = Angeheftet
    .doc = Ob sie an ihrem Kanal angeheftet ist, was die Suche findet.

d-ofad24rc = Reaktionen
    .doc = Wer reagiert hat, nach Emoji.

d-dh6346no = Gesendete Ansicht
    .doc = Eine Ansicht, die die Nachricht sendet, so wie ihr Absender sie unter seinen Ansichten hatte: ihre tilolang-Definition, die wer sie liest den eigenen Ansichten hinzufügt (das `add` von Views), ihr Autor wer die Nachricht gesendet hat.

d-tz5yw5dt = Von
    .doc = Wer sie im nativen Chat geschrieben hat, wie jene App sie nannte, für eine von dort übernommene Nachricht: ihr eigener Verlauf nennt, wer sie übernommen hat und wann.

d-mmlcexsa = Geschrieben
    .doc = Wann sie im nativen Chat geschrieben wurde, in Millisekunden seit 1970; 0 für eine hier geschriebene Nachricht.

d-jcgolgdf = Reagiert
    .doc = Die Reaktionen, die sie im nativen Chat hatte, jedes Emoji mit der Zahl derer, die damit reagiert haben: wer dort reagiert hat, wird nicht übernommen, da diese App Personen nur als ihre eigenen Mitglieder nennt.

d-6luxdz2x = Weitergeleitete Reaktionen
    .doc = Was ihr dort, wo sie geschrieben wurde, gegeben wurde, bei einer hierher weitergeleiteten Nachricht aus einem anderen Dienst: jedes Emoji, wer es dort gab und der Name, den jener Ort dafür zeigt. Getrennt von `reactions`, das die Mitglieder dieser App hält, und dort unter diesem Namen gezeigt.

d-hk2t6bvk-filter-pinned = Angeheftet

d-6qpql3pg = Thread
    .doc = Eine Seitenunterhaltung, die von einer Nachricht abzweigt.
    .noun = { $count ->
        [one] Thread
       *[other] Threads
    }

d-xuvfifa5 = Name

d-ijy454g6 = Nachrichten

d-2ki7h2z5 = Archiviert

d-6qpql3pg-filter-archived = Archiviert

d-5nkbpj6q = Direkt
    .doc = Eine private Unterhaltung zwischen Personen des Arbeitsbereichs, im selben Nachrichtenmodell wie ein Kanal: wer sie geöffnet hat und mit wem, oder eine Person allein für Notizen an sich selbst.
    .noun = { $count ->
        [one] Direktnachricht
       *[other] Direktnachrichten
    }

d-5stv6ro7 = Name

d-wph25aa5 = Personen
    .doc = Wer darin ist, jede Person einmal.

d-ycadyqh5 = Person
    .doc = Die eine Person, mit der eine frühere Version sie führte, bevor sie sagte, wer darin ist: was ihre Migration liest (`carry_direct_people`).

d-y65whtrn = Nachrichten

d-3p73kpuw = Threads

d-5nkbpj6q-filter-person = Person

d-yx5sbebu = Teil
    .doc = Ein Stück einer Nachricht von bevor Nachrichten Rich Text waren: ihr Text, oder die Quelle eines Blocks.
    .noun = { $count ->
        [one] Teil
       *[other] Teile
    }

d-zevnbs3i = Art
    .doc = `text` oder `block`.

d-3hkzkqvr = Text

d-gfk6pm4a = Name
    .doc = Wie sie heißt, in wenigen Worten: womit ein Agent sie benennt.

d-gnfldsvz = Wofür sie gilt
    .doc = Die Art Arbeit, für die sie gilt, in einem oder zwei Sätzen: wann sie zutrifft.

d-bn3w5doj = So wird sie getan
    .doc = Wie diese Arbeit hier getan wird.

d-ijpa4i7d = Form des Ergebnisses
    .doc = Die Form, die das Ergebnis hat; leer, wenn es keine feste Form hat.

d-3wlkwi6k = Modell
    .doc = Das Modell, mit dem die Arbeit getan wird, als `provider/model`; leer für die Wahl der Person für diese Art Arbeit.

d-bxx5rykt = Beispiele
    .doc = Ergebnisse, die die Person angenommen und abgelehnt hat, älteste zuerst: wie die Arbeit getan wird, gezeigt an dem, was daraus wurde.

d-so72kbuk = Beispiel
    .doc = Ein Ergebnis, das eine Person angenommen oder abgelehnt hat, bei seiner Fähigkeit aufbewahrt als Beispiel, wie diese Arbeit getan wird: mehr davon, oder weniger davon.
    .noun = { $count ->
        [one] Beispiel
       *[other] Beispiele
    }

d-fspydtu6 = Text
    .doc = Das Ergebnis, oder der Teil davon, der gemeint ist.

d-2u2ngevv = Angenommen
    .doc = Ob die Person es angenommen (mehr davon) oder abgelehnt (weniger davon) hat.

d-agcun6lt = Notiz
    .doc = Was die Person dazu gesagt hat; leer, wenn sie nichts weiter gesagt hat.

d-3ccutu3r = Wann
    .doc = Wann es aufbewahrt wurde, woher diese Fähigkeit kommt, in Millisekunden seit 1970; 0 für eines, das hier aufbewahrt wurde, dessen Zeit der Verlauf der Fähigkeit sagt.

d-so72kbuk-filter-accepted = Angenommen

d-so72kbuk-filter-at = Wann

d-peb7lz2j = Senden
    .doc = Veröffentlicht eine Nachricht in einem Kanal, einem Thread oder einer Direktunterhaltung als der, der es aufträgt, und antwortet auf eine andere, wenn `reply_to` eine nennt.
    .did = { $count ->
        [one] schrieb eine Nachricht
       *[other] schrieb { $count } Nachrichten
    }
    .w-type-a-message-first = Schreibe zuerst eine Nachricht

d-qaa5kdra = Eine Ansicht senden
    .doc = Sendet eine Ansicht als Nachricht: ihren Titel als das, was die Nachricht sagt, ihre Definition, damit jeder, der sie liest, sie den eigenen Ansichten hinzufügen kann.
    .w-the-view-is-empty = Die Ansicht ist leer

d-vhmhrq2t = Löschen
    .doc = Löscht eine Nachricht aus dem Kanal oder Thread, in dem sie steht.

d-6uysx57s = Thread beginnen
    .doc = Beginnt einen Thread aus einer Nachricht eines Kanals oder einer Direktunterhaltung, benannt nach dem, was die Nachricht sagt.

d-dgrvpxre = Ansicht senden~present

d-dzzuovny = Ungelesen
    .w-unread = Ungelesen
    .w-show-all = Alle { $n } zeigen

d-ou6h2s2u = Fähigkeiten
    .w-new-skill = Neue Fähigkeit
    .w-no-skills-yet = Noch keine Fähigkeiten
    .w-skills-say-how-work-is-done = Fähigkeiten sagen, wie Arbeit in diesem Arbeitsbereich getan wird, und seine Agenten lesen sie.

d-2dkm5gja = Fähigkeit
    .w-what-it-is-called = Wie sie heißt
    .w-when-it-applies = Wann sie gilt
    .w-the-kind-of-work-this-is = Die Art Arbeit, für die sie gilt, in einem oder zwei Sätzen
    .w-how-it-is-done-here = So wird sie hier getan
    .w-what-to-do-step-by-step = Was zu tun ist, Schritt für Schritt, wo das hilft
    .w-the-form-of-the-result = Die Form des Ergebnisses
    .w-empty-when-it-has-no-fixed = Leer, wenn es keine feste Form hat
    .w-the-model = Das Modell
    .w-provider-model-empty-for-the-person = provider/model, leer für die Wahl der Person
    .w-examples = Beispiele

d-gnhwq4qd = Beispielzeile
    .w-accepted-more-like-this = Angenommen: mehr davon
    .w-rejected-less-like-this = Abgelehnt: weniger davon
    .w-remove = Entfernen

d-5rkfnkg4 = Seitenleiste
    .w-channels = Kanäle
    .w-no-channels-yet = Noch keine Kanäle
    .w-channels-are-where-your-workspace-talks = Kanäle sind, wo dein Arbeitsbereich spricht, jeder ein Thema.
    .w-notes-to-self = Notizen an mich
    .w-assistant = Assistent
    .w-sub-agents = Unteragenten
    .w-start-a-sub-agent = Unteragenten starten
    .w-you = Du
    .w-supervision = Aufsicht
    .w-direct-messages = Direktnachrichten
    .w-everyone = Alle
    .w-workspace = Arbeitsbereich
    .w-workspace-skills = Fähigkeiten im Arbeitsbereich
    .w-no-channel-open = Kein Kanal offen
    .w-rename-category = Kategorie umbenennen
    .w-move-up = Nach oben
    .w-move-down = Nach unten
    .w-delete-category = Kategorie löschen
    .w-rename-channel = Kanal umbenennen
    .w-set-the-topic = Thema setzen
    .w-restore-channel = Kanal wiederherstellen
    .w-archive-channel = Kanal archivieren
    .w-reports = Meldungen

d-ymtess4x = Kanal
    .w-replay = { $name } erneut abspielen
    .w-about = Über { $name }
    .w-pinned-messages = Angeheftete Nachrichten
    .w-members = Mitglieder
    .w-channel = Kanal
    .w-rename-channel = Kanal umbenennen
    .w-set-the-topic = Thema setzen
    .w-restore-channel = Kanal zurückholen
    .w-archive-channel = Kanal archivieren
    .w-move-up = Nach oben
    .w-move-down = Nach unten
    .w-delete-channel = Kanal löschen
    .w-message = Nachricht an #{ $name }
    .w-attach = Anhängen
    .w-send = Senden
    .w-wrote-this-in = { $name } schrieb dies in { $service }
    .w-pinned = Angeheftet
    .w-edit-message = Nachricht bearbeiten
    .w-cancel = Abbrechen
    .w-save = Speichern
    .w-the-host-didn-t-keep-it = Der Host behält { $name } nicht: Die Datei ist größer als die { $mib }, die er je Datei behält.
    .w-the-host-didn-t-keep-this = Der Host behält { $name } nicht: Die Dateien dieser Person nehmen mehr als ihre { $mib } ein.
    .w-the-host-didn-t-keep-this-2 = Der Host behält { $name } nicht: Diese Person hat in dieser Stunde mehr als { $mib } gepostet.
    .w-the-host-didn-t-keep-the = Der Host behält { $name } nicht: Der Arbeitsbereich hält mehr als { $mib } auf ihm.
    .w-the-host-didn-t-keep-it-2 = Der Host behält { $name } nicht: Sie ist mehr, als dieser Host behält ({ $mib }).
    .w-the-host-s-person-can-raise = Die Person des Hosts kann sie in den Einstellungen des Servers erhöhen.
    .w-the-workspace-s-owners-can-raise = Die Eigentümer des Arbeitsbereichs können sie in den Einstellungen unter Inhaltsgrenzen erhöhen.
    .w-reply = Antworten
    .w-edit = Bearbeiten
    .w-1-thread = 1 Thread
    .w-threads = { $len } Threads
    .w-1-reply = 1 Antwort
    .w-replies = { $len } Antworten

d-27cntb22 = Mitglieder des Kanals
    .w-members = Mitglieder
    .w-agent = Agent
    .w-remove = Entfernen
    .w-not-in-the-channel = Nicht im Kanal
    .w-add = Hinzufügen

d-jbezqvlc = Direkt
    .w-replay = { $name } erneut abspielen
    .w-about = Über { $name }
    .w-message = Nachricht an { $name }
    .w-attach = Anhängen
    .w-send = Senden
    .w-wrote-this-in = { $name } schrieb dies in { $service }
    .w-pinned = Angeheftet
    .w-edit-message = Nachricht bearbeiten
    .w-cancel = Abbrechen
    .w-save = Speichern
    .w-the-host-didn-t-keep-it = Der Host behält { $name } nicht: Die Datei ist größer als die { $mib }, die er je Datei behält.
    .w-the-host-didn-t-keep-this = Der Host behält { $name } nicht: Die Dateien dieser Person nehmen mehr als ihre { $mib } ein.
    .w-the-host-didn-t-keep-this-2 = Der Host behält { $name } nicht: Diese Person hat in dieser Stunde mehr als { $mib } gepostet.
    .w-the-host-didn-t-keep-the = Der Host behält { $name } nicht: Der Arbeitsbereich hält mehr als { $mib } auf ihm.
    .w-the-host-didn-t-keep-it-2 = Der Host behält { $name } nicht: Sie ist mehr, als dieser Host behält ({ $mib }).
    .w-the-host-s-person-can-raise = Die Person des Hosts kann sie in den Einstellungen des Servers erhöhen.
    .w-the-workspace-s-owners-can-raise = Die Eigentümer des Arbeitsbereichs können sie in den Einstellungen unter Inhaltsgrenzen erhöhen.
    .w-reply = Antworten
    .w-edit = Bearbeiten

d-q45utnmh = Kanalübersicht

d-enkldjck = Direktübersicht

d-oqg63mfd = Kategorieübersicht

d-hrbsxhor = Fähigkeitszeile

d-enkgjyn7 = Thread
    .w-replay = { $name } erneut abspielen
    .w-about = Über { $name }
    .w-message = Nachricht an { $name }
    .w-attach = Anhängen
    .w-send = Senden
    .w-wrote-this-in = { $name } schrieb dies in { $service }
    .w-pinned = Angeheftet
    .w-edit-message = Nachricht bearbeiten
    .w-cancel = Abbrechen
    .w-save = Speichern
    .w-reply = Antworten
    .w-edit = Bearbeiten
    .w-the-host-didn-t-keep-it = Der Host behielt { $name } nicht: es ist größer als die { $mib }, die er von einer Datei behält.
    .w-the-host-didn-t-keep-this = Der Host behielt { $name } nicht: die Dateien dieser Person brauchen mehr als ihre { $mib }.
    .w-the-host-didn-t-keep-this-2 = Der Host behielt { $name } nicht: diese Person hat in dieser Stunde mehr als { $mib } eingestellt.
    .w-the-host-didn-t-keep-the = Der Host behielt { $name } nicht: der Arbeitsbereich braucht mehr als { $mib } auf ihm.
    .w-the-host-didn-t-keep-it-2 = Der Host behielt { $name } nicht: es ist mehr als die { $mib }, die dieser Host behält.
    .w-the-host-s-person-can-raise = Die Person des Hosts kann es in den Einstellungen des Servers erhöhen.
    .w-the-workspace-s-owners-can-raise = Die Eigentümer des Arbeitsbereichs können es in den Einstellungen unter Inhaltsgrenzen erhöhen.
    .w-this-thread-is-archived-a-reply = Dieser Thread ist archiviert. Eine Antwort öffnet ihn wieder.
    .w-reopen-thread = Thread wieder öffnen
    .w-archive-thread = Thread archivieren

d-rydgecpc = Hier wird geschrieben
    .w-is-typing = { $who } schreibt …
    .w-and-others-are-typing = { $who } und andere schreiben …

d-tx3zjawu = Hier wird direkt geschrieben
    .w-is-typing = { $who } schreibt …
    .w-and-others-are-typing = { $who } und andere schreiben …

d-dcprca6g = Hier wird im Thread geschrieben
    .w-is-typing = { $who } schreibt …
    .w-and-others-are-typing = { $who } und andere schreiben …

d-h327uiql = Frage und Antwort

d-hjw5xd6b = Gesagt
    .w-a-view-to-add-to-your = Eine Ansicht, die du den eigenen hinzufügen kannst

d-peb7lz2j-to = An

d-peb7lz2j-body = Text

d-peb7lz2j-reply_to = Antwort auf

d-qaa5kdra-to = An

d-qaa5kdra-title = Titel

d-qaa5kdra-definition = Definition

d-vhmhrq2t-to = An

d-vhmhrq2t-m = M

d-6uysx57s-c = C

d-6uysx57s-m = M

d-6uysx57s-name = Name

d-xr347kui = Sprachkanal
    .doc = Ein Ort, um live zu sprechen: Wer beitritt, ist im Anruf des Systems, mit allen, die sonst beigetreten sind.
    .noun = { $count ->
        [one] Sprachkanal
       *[other] Sprachkanäle
    }

d-olqjezxu = Name

d-qnkkcusk = Sprachkanal

d-lkjppscz = Sprachkanalübersicht

d-4da5gcom = Geschrieben in
    .doc = Wo eine hierher übernommene oder weitergeleitete Nachricht geschrieben wurde.

d-y3mwdnru = Meldung
    .doc = Die Meldung einer Nachricht an die Moderatoren des Arbeitsbereichs, aufbewahrt von der Unterhaltung, in der die Nachricht steht, damit sie die Nachricht überdauert: wer sie eingereicht hat, steht in ihrem Verlauf.
    .noun = { $count ->
        [one] Meldung
       *[other] Meldungen
    }

d-jqcabz2h = Nachricht
    .doc = Die gemeldete Nachricht; keine mehr, sobald sie gelöscht wurde.

d-e3zqz33a = Person
    .doc = Wer die gemeldete Nachricht geschrieben hat.

d-adip7bw4 = Inhalt
    .doc = Was die Nachricht sagte, als sie gemeldet wurde, auch nach ihrer Löschung lesbar.

d-y3mwdnru-filter-message = Nachricht

d-y3mwdnru-filter-person = Person

d-y3mwdnru-filter-banned = Gesperrt

d-kgihivmr = Meldungen
    .doc = Die Meldungen zu seinen Nachrichten, für die Moderatoren des Arbeitsbereichs.

d-ep7fvntj = Meldungen
    .doc = Die Meldungen zu seinen Nachrichten, für die Moderatoren des Arbeitsbereichs.

d-doiukw5i = Meldungen
    .doc = Die Meldungen zu ihren Nachrichten, für die Moderatoren des Arbeitsbereichs.

d-bmfbgqt6 = Grund
    .doc = Warum, in den Worten der meldenden Person.

d-3oner4zg = Entscheidung
    .doc = Was eine Moderatorin oder der Moderationsagent dazu entschieden hat; leer, solange sie offen ist.

d-6372fg6z = Weiterleiten
    .doc = Stellt eine Nachricht ein, die auf einem anderen Dienst geschrieben wurde, so wie sie dort geschrieben wurde.
    .did = hat eine Nachricht weitergeleitet
    .w-say-who-wrote-the-message = Sag, wer die Nachricht geschrieben hat
    .w-the-person-a-relayed-message-is = Die Person, mit der eine weitergeleitete Nachricht verknüpft ist, ist nicht in diesem Arbeitsbereich

d-jozxp5qy = Kanal löschen
    .doc = Löscht einen Kanal und was darin gesagt wurde, aus der Seitenleiste oder der Kategorie, in der er ist.
    .w-only-the-workspace-s-moderators-delete = Nur die Moderierenden des Arbeitsbereichs löschen einen Kanal

d-7sjai347 = Melden
    .doc = Meldet eine Nachricht einer Unterhaltung an die Moderatoren des Arbeitsbereichs, mit Grund, wenn die meldende Person einen nennt.
    .did = hat eine Nachricht gemeldet

d-7sjai347-to = In

d-7sjai347-m = Nachricht

d-7sjai347-reason = Grund

d-6372fg6z-to = In

d-6372fg6z-body = Text

d-6372fg6z-by = Von

d-6372fg6z-written_in = Geschrieben in

d-6372fg6z-written = Geschrieben

d-6372fg6z-reply_to = Antwort auf

d-jozxp5qy-from = Aus

d-jozxp5qy-ch = Kanal

d-psjj65el = Weiterleiten~present

d-n7d27huf = Anhänge
    .doc = Die Dateien, die sie mitbringt: was eine Brücke aus einem anderen Dienst weiterleitet, und was jemand mit einer Nachricht sendet.

d-5rctnita = Anhang
    .doc = Eine Datei, die eine Nachricht mitbringt, so wie die Stelle sie hält, von der sie kopiert wurde: ihr Name, was sie ist, wie viele Bytes, und die Adresse ihres Inhalts.
    .noun = { $count ->
        [one] Anhang
       *[other] Anhänge
    }

d-qsdc77jc = Name

d-h5lqkkjt = Medientyp

d-g7nx7a7g = Größe

d-wu2n7dpm = Inhalt
    .doc = Die Adresse, unter der ihre Bytes liegen: `tilo-blob://<hash>`.

d-5rctnita-filter-size = Größe

d-3kzifrct = Weitergeleitete Reaktion
    .doc = Eine Reaktion, die jemand einer Nachricht dort gab, wo sie geschrieben wurde, und die mit ihr hierher weitergeleitet wurde: ihr Emoji, das Konto dort, das sie gab, und der Name, den jener Ort dafür zeigt.
    .noun = { $count ->
        [one] weitergeleitete Reaktion
       *[other] weitergeleitete Reaktionen
    }

d-r2zcptjt = Emoji

d-awgtnj33 = Konto
    .doc = Wer sie dort gab, wie die Weiterleitung die Person nennt: der Dienst und das Konto (`discord/3001`).

d-hammizbi = Von
    .doc = Der Name, den jener Ort dafür zeigt.

d-6372fg6z-attachments = Anhänge

d-peb7lz2j-attachments = Anhänge

d-so-tilo-chat-keywords = Messenger, Sofortnachrichten, Unterhaltung

d-64vv225r = Entwürfe
    .doc = Was jede Person in einer Unterhaltung schreibt und nicht gesendet hat.

d-5pfbdqqj = Dienst
    .doc = Der fremde Dienst, zu dem diese Unterhaltung getragen wird, so wie seine Leute ihn lesen („Discord“); leer für eine, die hier bleibt.

d-vvs4ecqz = Fremde Id
    .doc = Ihre Id dort, wenn eine Brücke sie trägt: der Kanal, die Kategorie oder die Unterhaltung dort.

d-enzjdbyp = Dienst
    .doc = Wie der Ort heißt, von dem sie schrieben, so wie seine Leute ihn lesen („Discord“).

d-fld77g72 = Zusammenfassung
    .doc = Was hier bisher gesagt wurde, in ein paar Absätzen, so wie ein Agent es zuletzt aufgeschrieben hat: was ein Zug anstelle der Nachrichten liest, die es zusammenfasst.

d-q7llygsa-filter-archived = Archiviert

d-3ngvkwox = Fremder Autor
    .doc = Wer eine Nachricht anderswo schrieb, so wie jener Ort die Person zeigt: der allgemeine fremde Autor einer Brücke, nie die Form eines einzelnen Dienstes.
    .noun = { $count ->
        [one] fremder Autor
       *[other] fremde Autoren
    }

d-5fxndhc2 = Name
    .doc = Wie sie heißt, in wenigen Worten: womit ein Agent sie benennt.

d-zkszdahn = Avatar
    .doc = Der Discord-Avatar-Hash des Kontos.

d-p3wfphnr = Fremdes Emoji
    .doc = Ein Bild, das eine Nachricht von außen in ihren Worten mitbringt: ein eigenes Emoji jenes Dienstes.
    .noun = { $count ->
        [one] fremdes Emoji
       *[other] fremde Emojis
    }

d-ldtylgfu = Name

d-jjutyqgx = Bild
    .doc = Bei einem von der Brücke hochgeladenen Emoji: die Adresse seines Bildes.

d-hj4axvbu = Zug
    .doc = Der Zug des Agenten, der sie geschrieben hat: die Id des `#answer`-Aufrufs, in dem er antwortet, den Stop abbricht und dessen Schritte, Fragen und Kosten dem Zug gehören.

d-wqxsenb3 = Fremd
    .doc = Wer sie schrieb, wenn sie anderswo geschrieben wurde: so wie jener Ort die Person zeigt, mit dem Bild, das sie dort trägt, und dem Namen des Dienstes.

d-bbu6225x = Fremdes Emoji
    .doc = Die Bilder, die ihre Worte von dort mitbringen, wo sie geschrieben wurde: die eigenen Emojis jenes Dienstes, jedes über die Adresse gezeichnet, die er angibt.

d-cv664irm = Zusammenfassung
    .doc = Was hier bisher gesagt wurde, in ein paar Absätzen, so wie ein Agent es zuletzt aufgeschrieben hat: was ein Zug anstelle der Nachrichten liest, die es zusammenfasst.

d-s7ol3kl4 = Zusammenfassung
    .doc = Was hier bisher gesagt wurde, in ein paar Absätzen, so wie ein Agent es zuletzt aufgeschrieben hat: was ein Zug anstelle der Nachrichten liest, die es zusammenfasst.

d-5nkbpj6q-filter-people = Personen

d-lhl5rxcq = Entwurf
    .doc = Was eine Person in einer Unterhaltung schreibt und nicht gesendet hat.
    .noun = { $count ->
        [one] Entwurf
       *[other] Entwürfe
    }

d-5shwuvrs = Unterhaltung
    .doc = Die Unterhaltung, in der er geschrieben wird: ein Kanal, ein Thread oder eine Direktunterhaltung.

d-rxmavdj2 = Text

d-fp7zx7jn = Fähigkeit
    .noun = { $count ->
        [one] Fähigkeit
       *[other] Fähigkeiten
    }
    .untitled = Neue Fähigkeit

d-ldhzcfxm = Überbrücken
    .doc = Trägt eine Unterhaltung zu einem fremden Dienst und seine Nachrichten hierher, so wie der Dienst sie zeigt.
    .w-say-which-service-carries-the-conversation = Sag, welcher Dienst die Unterhaltung trägt

d-ldhzcfxm-c = Ka

d-yczlubh7 = Überbrückung lösen
    .doc = Behält eine Unterhaltung hier: kein Dienst trägt sie mehr.

d-yczlubh7-c = Ka

d-4scklxqs = Neuer Kanal
    .doc = Fügt der Seitenleiste einen Textkanal hinzu, unter `category`, wenn angegeben, und zu `topic`, wenn das gesetzt ist.
    .w-new-channel = neuer-kanal

d-4scklxqs-name = Name

d-4scklxqs-topic = Thema

d-4scklxqs-category = Kategorie

d-cq7rcfwg = Neue Kategorie
    .doc = Fügt der Seitenleiste eine Kategorie hinzu.
    .w-new-category = Neue Kategorie

d-cq7rcfwg-name = Name

d-f4qj2rwa = Neuer Sprachkanal
    .doc = Fügt der Seitenleiste einen Sprachkanal hinzu, unter `category`, wenn angegeben.
    .w-voice = Sprache

d-f4qj2rwa-name = Name

d-f4qj2rwa-category = Kategorie

d-daluhfsk = Neue Fähigkeit
    .doc = Beginnt eine Fähigkeit namens `name`.
    .w-the-workspace-already-has-a-skill = Der Arbeitsbereich hat schon eine Fähigkeit namens { $name }

d-daluhfsk-name = Name

d-6jgrwebi = Fähigkeit korrigieren
    .doc = Korrigiert eine Fähigkeit: nur was angegeben ist, ändert sich, und was fehlt, bleibt wie es ist.
    .w-the-workspace-already-has-a-skill = Der Arbeitsbereich hat schon eine Fähigkeit namens { $name }

d-6jgrwebi-name = Name

d-6jgrwebi-work = Wofür sie gilt

d-6jgrwebi-how = So wird sie getan

d-6jgrwebi-form = Form des Ergebnisses

d-6jgrwebi-model = Modell

d-hfw4gkd3 = Fähigkeit vergessen
    .doc = Vergisst eine Fähigkeit: kein späterer Zug liest sie.

d-gmpqlube = Ein Beispiel behalten
    .doc = Behält ein Beispiel einer Fähigkeit: ein Ergebnis, das die Person angenommen (eher so) oder abgelehnt (eher nicht so) hat, mit dem, was sie dazu sagte.
    .w-an-example-says-what-the-result = Ein Beispiel sagt, was das Ergebnis war

d-gmpqlube-text = Text

d-gmpqlube-accepted = Angenommen

d-gmpqlube-note = Notiz

d-44nvfzzq = Beispiel entfernen
    .doc = Nimmt ein Beispiel aus einer Fähigkeit.

d-44nvfzzq-example = Beispiel

d-fom6d6su = Direktpersonen übernehmen
    .doc = Die Direktunterhaltungen, die eine frühere Version mit einer Person allein führte (`person`), die jedes Mitglied lesen konnte: in jeder sind jene Person und wer sie öffnete, wie ihr Verlauf sagt.

d-bi2i7p2d = Nativen Chat übernehmen
    .doc = Die Unterhaltungen, die der native Chat hielt, bevor diese App seine Id übernahm, einmal in diese übernommen: seine Kategorien und Kanäle in der Reihenfolge der Seitenleiste, jede mit Name und Thema, und jede Nachricht eines Kanals mit dem, was sie sagte, ob sie angeheftet war und der Id, die sie dort hatte.

d-yqispavu = Problem melden
    .w-a-recording-is-a-video = Eine Aufnahme ist ein Video
    .doc = Legt einen Problembericht ab, was jemand bei der Nutzung von Tilo erlebt hat, im #feedback-Kanal des Arbeitsbereichs, den der erste Bericht öffnet.
    .did = hat ein Problem gemeldet
    .w-say-what-went-wrong-first = Sag zuerst, was schiefging

d-yqispavu-body = Text

d-yqispavu-recording = Aufnahme

d-yqispavu-attachments = Anhänge

d-4cfw6ty2 = Nachricht
    .doc = Eine Nachricht.
    .w-is-not-in-this-workspace = { $name } ist nicht in diesem Arbeitsbereich

d-4cfw6ty2-who = Wer

d-aaqqwyl3 = Unterhaltung mit
    .doc = Die Direktunterhaltung zwischen dem Aufrufer und `who`, keine, solange die beiden keine haben: was ein Agent, ein Skript oder eine andere App liest, um sie zu finden, ohne etwas zu öffnen oder anzulegen.

d-aaqqwyl3-who = Wer

d-uxndjwde = Ungelesen
    .doc = Wie viele Nachrichten einer Direktunterhaltung die fragende Person nicht gelesen hat: die Zahl, die ihre Zeile neben der anderen Person zeigt, und was ein Agent oder ein Skript davon liest.

d-q5e45hla = Kanal öffnen
    .doc = Einen Kanal zu öffnen macht den Entwurf, in dem seine Person schreibt, wenn sie keinen hat: ein Weg und zurück findet, was sie tippte, auf jedem ihrer Geräte, und niemand sonst liest es.

d-yfjnxpbn = Direkt öffnen
    .doc = {""}[`open_channel`] für eine Direktunterhaltung.

d-hs67kfnp = Thread öffnen
    .doc = {""}[`open_channel`] für einen Thread.

d-peb7lz2j-turn = Zug

d-peb7lz2j-draft = Entwurf

d-obp6lcdr = Dateien anhängen
    .doc = Fügt die Dateien, die eine Person gewählt hat, der Nachricht hinzu, die sie in `to` schreibt: die Dateiauswahl des Systems führt dies mit dem Gewählten aus, und die Dateien warten dort, wie `chosen_in` sie liest, bis die Nachricht gesendet wird.
    .did = hat Dateien angehängt

d-obp6lcdr-files = Dateien

d-qkcjgxr6 = Datei entfernen
    .doc = Nimmt eine Datei von der Nachricht, die eine Person in `to` schreibt, bevor sie gesendet wird.

d-qkcjgxr6-file = Datei

d-mfxqzosk = Anhang öffnen
    .doc = Öffnet die Datei `f` in der App, die ihre Art öffnet: die Datei, die eine Nachricht mitbringt, erreicht jene App als derselbe Inhalt, den beide lesen, nie als Pfad, den keine hat.

d-kn2iqm4w = Schreiben an
    .doc = Schreibt `body` an `who`, eine Person oder einen Agenten, in der einen Direktunterhaltung mit ihr: der bereits offenen, sonst einer neuen in der Seitenleiste.

d-kn2iqm4w-who = Wer

d-kn2iqm4w-body = Text

d-v3slhm65 = Zusammenfassen
    .doc = Schreibt auf, was in einer Unterhaltung bisher gesagt wurde, und ersetzt das vorher Geschriebene: was der nächste Zug eines Agenten dort anstelle der Nachrichten liest, die es abdeckt.

d-v3slhm65-to = An

d-v3slhm65-summary = Zusammenfassung

d-6372fg6z-avatar = Avatar

d-shvpmvqy = Reaktion weiterleiten
    .doc = Leitet eine Reaktion weiter, die jemand einer Nachricht dort gab, wo sie geschrieben wurde, so wie jener Ort die Person nennt: das Gegenstück zu `relay` für eine Reaktion.
    .did = hat eine Reaktion weitergeleitet
    .w-say-which-emoji = Sag, welches Emoji
    .w-say-who-gave-the-reaction = Sag, wer die Reaktion gab
    .w-say-what-they-are-called-there = Sag, wie sie dort heißen

d-shvpmvqy-emoji = Emoji

d-shvpmvqy-persona = Konto

d-shvpmvqy-by = Von

d-shvpmvqy-given = Gegeben

d-e3rwn5pb = Kategorie löschen
    .doc = Löscht eine Kategorie, die keinen Kanal hält.
    .w-only-the-workspace-s-moderators-delete = Nur die Moderierenden des Arbeitsbereichs löschen eine Kategorie
    .w-move-or-delete-its-channels-first = Verschiebe oder lösche zuerst ihre Kanäle

d-e3rwn5pb-c = Ka

d-4kkr24cm = Kanal verschieben
    .doc = Verschiebt einen Kanal an einen Platz in der Seitenleiste: unter `category` oder außerhalb jeder, an `at` unter den dort gelisteten Kanälen (am Ende, wenn negativ).

d-4kkr24cm-ch = Kanal

d-4kkr24cm-category = Kategorie

d-4kkr24cm-at = Wann

d-6rigb6x4 = Kategorie verschieben
    .doc = Verschiebt eine Kategorie an einen Platz in der Seitenleiste, an `at` unter den dort gelisteten Kategorien und Kanälen (am Ende, wenn negativ).

d-6rigb6x4-c = Ka

d-6rigb6x4-at = Wann

d-k5m36ngd = Kanal umbenennen
    .doc = Benennt einen Textkanal um.
    .w-only-the-workspace-s-moderators-rename = Nur die Moderierenden des Arbeitsbereichs benennen einen Kanal um
    .w-name-the-channel-first = Benenne zuerst den Kanal

d-k5m36ngd-name = Name

d-tekxv77d = Sprachkanal umbenennen
    .doc = Benennt einen Sprachkanal um.
    .w-only-the-workspace-s-moderators-rename = Nur die Moderierenden des Arbeitsbereichs benennen einen Kanal um
    .w-name-the-channel-first = Benenne zuerst den Kanal

d-tekxv77d-name = Name

d-sgvix76a = Thema setzen
    .doc = Sagt, wofür ein Kanal da ist, in seinem Thema.
    .w-only-the-workspace-s-moderators-set = Nur die Moderierenden des Arbeitsbereichs setzen das Thema eines Kanals

d-sgvix76a-topic = Thema

d-ciefrn2e = Kategorie umbenennen
    .doc = Benennt eine Kategorie um.
    .w-only-the-workspace-s-moderators-rename = Nur die Moderierenden des Arbeitsbereichs benennen eine Kategorie um
    .w-name-the-category-first = Benenne zuerst die Kategorie

d-ciefrn2e-name = Name

d-z7ux3t5k = Kanal archivieren
    .doc = Legt einen Kanal beiseite, aus der Seitenleiste heraus, und behält alles, was darin gesagt wurde, oder holt ihn zurück.
    .w-only-the-workspace-s-moderators-archive = Nur die Moderierenden des Arbeitsbereichs archivieren einen Kanal
    .w-archive-channel = Kanal archivieren
    .w-restore-channel = Kanal zurückholen

d-z7ux3t5k-archived = Archiviert

d-2z76tvdn = Thread archivieren
    .doc = Legt einen Thread beiseite, wenn das Gespräch vorbei ist, und behält alles darin Gesagte, oder öffnet ihn wieder.
    .w-only-who-started-it-or-a = Nur wer ihn begonnen hat oder ein Moderator archiviert einen Thread
    .w-archive-thread = Thread archivieren
    .w-reopen-thread = Thread wieder öffnen

d-2z76tvdn-archived = Archiviert

d-yjc4fmrk = Bearbeiten
    .doc = Ändert, was eine Nachricht sagt.
    .w-only-its-author-can-edit-a = Nur ihr Autor kann eine Nachricht bearbeiten
    .w-type-a-message-first = Tippe zuerst eine Nachricht

d-yjc4fmrk-body = Text

d-yjc4fmrk-turn = Zug

d-nzltze4g = Anheften
    .doc = Heftet eine Nachricht an ihren Kanal an oder löst sie.
    .w-pin = Anheften
    .w-unpin = Lösen

d-nzltze4g-pinned = Angeheftet

d-ia5g3jsz = Reagieren
    .doc = Fügt deine Reaktion mit einem Emoji zu einer Nachricht hinzu oder nimmt sie zurück, wenn du damit reagiert hattest.

d-ia5g3jsz-emoji = Emoji

d-honuhrmf = Entscheiden
    .doc = Entscheidet einen Bericht: was eine moderierende Person darüber entschied.
    .w-only-the-workspace-s-moderators-settle = Nur die Moderierenden des Arbeitsbereichs entscheiden einen Bericht
    .w-say-what-was-decided = Sag, was entschieden wurde

d-honuhrmf-decision = Entscheidung

d-l6xtwtlq = Überbrücken

d-2yeh74ot = Neue Fähigkeit

d-ujq3oyls = Fähigkeit korrigieren

d-hpamj3eq = Beispiel behalten

d-amkgebye = Problem melden

d-pszgxdax = Person anschreiben

d-vib4vsqe = Senden

d-zmf3ccig = Reaktion weiterleiten

d-mdviqjz2 = Kanal löschen

d-czomdsi6 = Kategorie löschen

d-r47ecljq = Kanal umbenennen

d-cqfpcwyx = Sprachkanal umbenennen

d-maq2lipz = Thema setzen

d-vrzrke7n = Kategorie umbenennen

d-g5hhjpdz = Kanal archivieren

d-3j54b6u3 = Thread archivieren~present

d-7ioawutl = Bearbeiten

d-p5ahaaux = Anheften

d-rcr57dzk = Entscheiden

d-i3f4daaw = Angeheftet
    .w-nothing-pinned-yet = Noch nichts angeheftet
    .w-pin-a-message-from-its-menu = Hefte eine Nachricht über ihr Menü an, um sie hier für alle zu behalten.

d-pyomqal2 = Personen
    .w-people = Personen
    .w-agent = Agent
    .w-message = Nachricht

d-kd5tp6dr = Zeilenlauf
    .w-running = Läuft…

d-zprw5jt4 = Konto
    .w-in = in { $written_in }

d-lzqy37mf = Zugschritte

d-wysicvmq = Zugschritt
    .w-working = Arbeitet…
    .w-failed = Fehlgeschlagen
    .w-allow = Erlauben
    .w-always = Immer
    .w-deny = Verweigern

d-nvzanb5n = Fremde Id
    .doc = Ihre Id dort, wenn eine Brücke sie trägt: der Kanal, die Kategorie oder die Unterhaltung dort.

d-byf6kyop = Archiviert
    .doc = Ob der Kanal beiseitegelegt ist: er behält jede Nachricht, und sein Platz in der Seitenleiste ist fort, bis er zurückgeholt wird.

d-eqr5qd3v = Dienst
    .doc = Der fremde Dienst, zu dem diese Unterhaltung getragen wird, so wie seine Leute ihn lesen („Discord“); leer für eine, die hier bleibt.

d-elvv3ebj = Fremde Id
    .doc = Ihre Id dort, wenn eine Brücke sie trägt: der Kanal, die Kategorie oder die Unterhaltung dort.

d-yycajap4 = Dienst
    .doc = Wie der Ort heißt, von dem sie schrieben, so wie seine Leute ihn lesen („Discord“).

d-a7u4jddr = Dienst
    .doc = Der fremde Dienst, zu dem diese Unterhaltung getragen wird, so wie seine Leute ihn lesen („Discord“); leer für eine, die hier bleibt.

d-f4cfgfpu = Fremde Id
    .doc = Ihre Id dort, wenn eine Brücke sie trägt: der Kanal, die Kategorie oder die Unterhaltung dort.

d-o5vmbr2v = Dateien
    .doc = Die Dateien, die sie mitbringen wird, in der Reihenfolge, in der sie gewählt wurden: die Dateiauswahl des Systems fügt sie hinzu, der Schreibbereich zeigt sie, und das Senden nimmt sie mit der Nachricht an die Nachricht.

d-ldhzcfxm-service = Dienst

d-ldhzcfxm-outside_id = Fremde Id

d-6372fg6z-service = Dienst

d-6372fg6z-outside_emoji = Fremdes Emoji

d-z5btqqxi = Entwurfszeile
    .w-draft = Entwurf: { $line }

d-s6tsrsha = Modellauswahl
    .w-model = Modell
    .w-agents-answer-with = Agenten antworten mit
    .w-answers-so-far = Bisherige Antworten: ${ $cost }
    .w-undo-the-choice = Auswahl rückgängig machen

d-dznrvwyx = Bearbeitungsverlauf
    .w-edited = Bearbeitet

d-7spkrvju = Meldungen
    .doc = Die Meldungen über eine Person statt über eine ihrer Nachrichten, für die Moderierenden des Arbeitsbereichs.

d-ew25p5fb = Mitglied
    .doc = Die Person hier, die jenes Konto mit sich verknüpft hat, falls jemand es tat: die Nachricht trägt ihr Gesicht und ihre Farbe neben dem Namen des Kontos, damit der Arbeitsbereich sieht, wer es ist.

d-3ngvkwox-filter-member = Mitglied

d-t2kepujk = Überlegung
    .doc = Was das Modell des Agenten beim Schreiben dieser Antwort überlegt hat, so wie sein Anbieter es zeigt: neben der Antwort geschrieben, während sie entsteht, und über ihr eingeklappt. Leer bei einer Nachricht, die eine Person schrieb, oder bei einem Modell, das keine Überlegung zeigt.

d-maczz6ef = Aufgehoben
    .doc = Warum eine moderierende Person diese Entscheidung aufgehoben hat, was die Meldung wieder öffnet: leer, solange die Entscheidung steht. Wer entschied, wer aufhob und wann, steht im Verlauf der Meldung.

d-6372fg6z-member = Mitglied

d-yjc4fmrk-reasoning = Überlegung

d-ryvwj3mk = Fortsetzen
    .doc = Bittet den Agenten, der `m` schrieb, dort weiterzumachen, wo seine Antwort aufhörte: eine Antwort darauf von der Person, die den Agenten nennt und die sein nächster Zug mit der Antwort davor liest. Eine Antwort, der der Platz ausging, die angehalten wurde oder abbrach, wird so beendet, nicht von vorn geschrieben.
    .did = hat eine Antwort fortsetzen lassen
    .w-only-an-agent-s-answer-continues = Nur die Antwort eines Agenten lässt sich fortsetzen
    .w-continue-your-answer-where-it-stopped = Setze deine Antwort dort fort, wo sie aufhörte, ohne sie zu wiederholen oder etwas erneut zu tun.

d-ryvwj3mk-to = An

d-ryvwj3mk-m = Nachricht

d-eltshl3a = Fortsetzen

d-c4qrg6s6 = Text kopieren
    .doc = Kopiert, was eine Nachricht sagt, als einfache Worte in die Zwischenablage.

d-cfajw6ne = Person melden
    .doc = Meldet eine Person den Moderierenden des Arbeitsbereichs, für ihr Verhalten hier statt für eine Nachricht: was die meldende Person sagt, ist alles daran, und die Moderierenden entscheiden sie wie jede andere Meldung.
    .did = hat eine Person gemeldet
    .w-you-can-t-report-yourself = Du kannst dich nicht selbst melden
    .w-say-what-happened = Sag, was passiert ist

d-cfajw6ne-who = Wer

d-cfajw6ne-reason = Grund

d-zkys4zdn = Person melden

d-trsgwzny = Entscheidung aufheben
    .doc = Hebt auf, was über eine Meldung entschieden wurde, mit dem Grund: die Meldung ist wieder offen, damit eine moderierende Person neu entscheidet, und die aufgehobene Entscheidung bleibt in ihrem Verlauf.
    .w-only-the-workspace-s-moderators-overturn = Nur die Moderierenden des Arbeitsbereichs heben eine Entscheidung auf
    .w-only-a-decided-report-can-be = Nur eine entschiedene Meldung lässt sich aufheben
    .w-say-why-the-decision-is-overturned = Sag, warum die Entscheidung aufgehoben wird

d-trsgwzny-why = Warum

d-i2nfhat3 = Entscheidung aufheben

d-hji3chrf = Sperren
    .doc = Sperrt eine Person aus dem Arbeitsbereich: Sie verlässt ihn und bleibt draußen, bis eine moderierende Person die Sperre aufhebt.
    .did = hat eine Person gesperrt
    .w-you-can-t-ban-yourself = Du kannst dich nicht selbst sperren
    .w-say-why = Sag, warum

d-hji3chrf-who = Wer

d-hji3chrf-reason = Grund

d-xvkxk373 = Sperren

d-xnavgsk3 = Sperren
    .doc = Entscheidet eine Meldung, indem die gemeldete Person aus dem Arbeitsbereich gesperrt wird, wie [`ban`] es tut.
    .did = hat eine Person gesperrt
    .w-they-are-banned-already = Die Person ist schon gesperrt
    .w-the-report-names-nobody-to-ban = Die Meldung nennt niemanden zum Sperren
    .w-you-can-t-ban-yourself = Du kannst dich nicht selbst sperren

d-idioksqp = Gemeldete Person sperren

d-5gbnohgv = Gesperrt
    .doc = Die Entscheidung hat die Person aus dem Arbeitsbereich gesperrt: Wird sie aufgehoben, darf die Person zurück.

d-ejjotbyg = Anrufstatus
    .w-deafened = Hört nichts
    .w-muted = Stummgeschaltet
    .w-camera-on = Kamera an
    .w-sharing-their-screen = Teilt den Bildschirm

d-bacw3hcw = Stand
    .w-working = Arbeitet
    .w-needs-you = Braucht dich
    .w-done = Fertig: { $line }
    .w-done-2 = Fertig

d-kftueou2 = Verbrauchswörter
    .w-with-no-day-limit = { $spent } ohne Tageslimit
    .w-of-a-day = { $spent } von { $money } pro Tag

d-mvp4n4pe = Zug gesagt
    .w-working = Arbeitet…
    .w-waiting-for-your-answer = Wartet auf deine Antwort
    .w-failed = Fehlgeschlagen
    .w-stopped = Angehalten
    .w-done = Fertig

d-tvcqqdxd = Meldungen
    .w-reports = Meldungen
    .w-only-moderators-read-reports = Nur Moderierende lesen Meldungen

d-yg35odmx = Kanalmeldungen

d-zmfzcl37 = Meldungszeile
    .w-in = { $name } in { $place }
    .w-reported = Gemeldet: { $reason }
    .w-overturned = Aufgehoben: { $overturned }
    .w-decided = Entschieden: { $decision }

d-vghhlimb = Überlegung
    .w-reasoning = Überlegung

d-bous5cgd = Aufsicht
    .w-supervision = Aufsicht
    .w-is-on = { $name } ist an
    .w-model = Modell
    .w-chosen-in-settings = In den Einstellungen gewählt
    .w-change-in-settings = In den Einstellungen ändern
    .w-spent = Ausgegeben

d-fm2l5rjj = Unteragenten
    .w-sub-agents-of = Unteragenten von { $name }
    .w-no-sub-agents = Keine Unteragenten
    .w-each-sub-agent-has-its-conversation = Jeder Unteragent hat seine eigene Unterhaltung unter Direktnachrichten; den Hauptchat kann er nicht lesen.

d-qjqtfdyr = Kein Assistent
    .w-you-have-no-assistant-in-this = Du hast keinen Assistenten in diesem Arbeitsbereich
    .w-an-assistant-works-for-the-workspace = Ein Assistent arbeitet für den Arbeitsbereich und antwortet in der direkten Unterhaltung unter Direktnachrichten.
    .w-create-assistant = Assistent erstellen
    .w-the-workspace-s-agents-could-not = Die Agenten des Arbeitsbereichs konnten nicht gelesen werden

d-4cgtoeqz = Unteragenten starten
    .doc = Bittet den Assistenten, einen Unteragenten für `job` zu starten, in der direkten Unterhaltung, in der er antwortet: `start-sub-agent` im Datensatz ist dort seine eigene Aktion, und der Unteragent meldet sich in dieser Unterhaltung, wo die Person ihn verfolgt.
    .w-say-what-the-job-is-first = Sag zuerst, was die Aufgabe ist

d-4cgtoeqz-agent = Agent

d-4cgtoeqz-job = Aufgabe

d-i6unit73 = Unteragent anfragen~present

d-m6qu4xya = Unteragenten starten
    .w-start-a-sub-agent = Unteragenten starten
    .w-starts-it-in-your-conversation-and = { $name } startet ihn in deiner Unterhaltung und meldet sich dort, sodass du ihm folgst, wo er arbeitet.
    .w-what-the-sub-agent-should-do = Was der Unteragent tun soll
    .w-start = Starten

d-eb6soq5v = Unteragent
    .w-it-cannot-read-the-main-chat = Er kann den Hauptchat nicht lesen. Seine Unterhaltung ist seine eigene, unter Direktnachrichten.
d-knw6d6s7 = Live-Freigabe
    .doc = Ein Fenster, das live in eine Unterhaltung geteilt ist, wie eine Nachricht darauf verweist.
    .noun = { $count ->
        [one] Live-Freigabe
       *[other] Live-Freigaben
    }

d-noscllor = Fenster
    .doc = Das geteilte Fenster, mit dem Schlüssel, unter dem die Fenster und Freigaben der Shell es nennen.

d-n4s3rmdn = Arbeitsbereich
    .doc = Der Arbeitsbereich, in dem es geteilt ist, mit seinem Schlüssel.

d-k535m2ok = Titel
    .doc = Wie das Fenster hieß, als es geteilt wurde.

d-ntg5vyds = App-Name
    .doc = Die App, deren Fenster es ist, wie man sie kennt, und ihr Zeichen.

d-dzpk5b7g = Symbol

d-a4wmd33j = Darf bearbeiten
    .doc = Ob die Mitglieder, die es ansehen, es steuern dürfen, wie die teilende Person die Freigabe eingestellt hat.

d-jfgns7zp = Beendet
    .doc = Ob die teilende Person das Teilen beendet hat.

d-knw6d6s7-filter-can_edit = Darf bearbeiten

d-knw6d6s7-filter-ended = Beendet

d-4upqznm4 = Live-Freigabe
    .doc = Ein Fenster, das die absendende Person live teilt: die Freigabe der Shell selbst, die die Unterhaltung dort zeigt, wo die Nachricht steht.

d-atwyjl6u = Ein Fenster teilen
    .doc = Teilt eines der Fenster der Person live in eine Unterhaltung: Die Shell teilt das Fenster mit dem Arbeitsbereich, wie es die eigene Fensterleiste tut, und die Nachricht verweist auf diese Freigabe, sodass die Unterhaltung das Fenster live zeigt, wo die Nachricht steht.
    .did = hat ein Fenster geteilt
    .w-only-a-window-open-on-this = Von hier lässt sich nur ein Fenster teilen, das auf diesem Gerät offen ist

d-atwyjl6u-to = An

d-atwyjl6u-window = Fenster

d-atwyjl6u-can_edit = Darf bearbeiten

d-7qn2hklc = Ändern, was andere dürfen
    .doc = Lässt die Mitglieder, die ein in eine Unterhaltung geteiltes Fenster ansehen, es steuern oder nur ansehen: Die Freigabe der Shell ändert sich, und die Nachricht sagt es.
    .w-only-whoever-shared-the-window-can = Nur wer das Fenster geteilt hat, kann ändern, was andere dort dürfen
    .w-the-window-is-no-longer-shared = Das Fenster wird nicht mehr geteilt

d-7qn2hklc-can_edit = Darf bearbeiten

d-vg7v2ukx = Teilen beenden
    .doc = Beendet das Teilen eines in eine Unterhaltung geteilten Fensters: Die Freigabe der Shell endet, und die Nachricht sagt an seiner Stelle, dass das Fenster nicht mehr geteilt wird.
    .did = hat das Teilen eines Fensters beendet
    .w-only-whoever-shared-the-window-can = Nur wer das Fenster geteilt hat, kann das Teilen beenden
    .w-the-window-is-no-longer-shared = Das Fenster wird nicht mehr geteilt

d-g36lpm6s = Live-Freigabe
    .w-no-longer-shared = Nicht mehr geteilt
    .w-live = Live
    .w-is-no-longer-shared = { $app_name } wird nicht mehr geteilt
    .w-show-it-live = Live ansehen
    .w-can-edit = Darf bearbeiten
    .w-view-only = Nur ansehen
    .w-what-others-may-do = Was andere dürfen
    .w-stop-sharing = Teilen beenden

d-zl5zivy3 = Teilen-Menü
    .w-share-a-window = Ein Fenster teilen
    .w-open-the-window-you-want-to = Öffne das Fenster, das du teilen möchtest, und komm dann hierher zurück.
    .w-share = { $title } teilen

d-u7b7ovh5 = Fenster teilen~present

d-obu4vl74 = Freigabezugriff festlegen~present

d-6zxrhnyr = Teilen beenden~present
d-rcybqyag = Gehört
    .doc = Wie dieses Gerät eine Person in einem Anruf abspielt.
    .noun = { $count ->
        [one] Gehörte Person
       *[other] Gehörte Personen
    }

d-u4nfj5zx = Prozent
    .doc = In Prozent davon, wie die Person ankommt, bis doppelt so laut.

d-xtnm4w32 = Stummgeschaltet
    .doc = Gar nicht abgespielt, gleich bei welcher Lautstärke.

d-rcybqyag-filter-who = Wer

d-rcybqyag-filter-percent = Prozent

d-rcybqyag-filter-silenced = Stummgeschaltet

d-a2np4gvd = Wer

d-n3jzxecf = Hören
    .doc = Wie dieses Gerät jede Person abspielt, die es in einem Anruf hört: ihre Lautstärke, und ob die Person sie stummgeschaltet hat, was die Lautstärke für später aufhebt.
    .noun = { $count ->
        [one] Hören
       *[other] Hören
    }

d-ua6lm2xh = Personen

d-jnylbhsg = Lautstärkemenü
    .w-muted-for-you = Für dich stumm
    .w-volume = Lautstärke { $percent } %
    .w-mute-for-me = Für mich stummschalten

d-vpyg3qt4 = Festlegen, wie laut jemand klingt
    .doc = Spielt `who` in Anrufen auf diesem Gerät mit `percent` davon ab, wie die Person ankommt, und hört sie wieder, falls sie stummgeschaltet war.
    .did = hat festgelegt, wie laut jemand klingt
    .w-a-level-is-between-0-and = Eine Lautstärke liegt zwischen 0 und 200 Prozent

d-vpyg3qt4-who = Wer

d-vpyg3qt4-percent = Prozent

d-we3jqglq = Festlegen, wie laut jemand klingt

d-z5deq3xd = Jemanden für mich stummschalten
    .doc = Schaltet `who` in Anrufen auf diesem Gerät stumm, oder hört die Person wieder mit der Lautstärke, die sie hatte.
    .did = hat jemanden für sich stummgeschaltet

d-z5deq3xd-who = Wer

d-z5deq3xd-on = An

d-v4cscgro = Über die Person
    .w-about = Über { $name }

d-c2oflalz = Karte der Person
    .w-message = Nachricht

d-msyomsn6 = Unterhaltung exportieren
    .export = Markdown
    .doc = Eine Unterhaltung als Markdown-Datei, die das eine Export… anbietet und dort speichert, wo die Person es sagt: jede Nachricht in ihrer Reihenfolge, unter dem Namen, unter dem sie erscheint, und der Zeit, zu der sie geschrieben wurde.

d-msyomsn6-conversation = Unterhaltung
