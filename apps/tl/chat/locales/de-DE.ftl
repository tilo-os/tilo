# Chat auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-chat = Chat

d-lmdoxbnn = Unterhaltungen
    .doc = Die Unterhaltungen des Arbeitsbereichs in der Reihenfolge der Seitenleiste: Kanäle außerhalb jeder Kategorie, die Kategorien mit ihren eigenen, und die Direktnachrichten, in der Liste als Personen.
    .noun = { $count ->
        [one] Unterhaltung
       *[other] Unterhaltungen
    }

d-5kzdptyg = Kanäle

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
    .doc = Eine private Unterhaltung: eine andere Person, oder eine benannte Gruppe von ihnen, im selben Nachrichtenmodell wie ein Kanal.
    .noun = { $count ->
        [one] Direktnachricht
       *[other] Direktnachrichten
    }

d-5stv6ro7 = Name

d-ycadyqh5 = Person
    .doc = Die eine Person, mit der sie ist, für eine Direktnachricht.

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

d-msf7yzn7 = Neuer Kanal
    .doc = Fügt der Seitenleiste einen Textkanal hinzu, über `topic`, wenn es angegeben ist.
    .w-new-channel = new-channel

d-msf7yzn7-topic = Thema

d-26f7sh5k = Neue Kategorie
    .doc = Fügt der Seitenleiste eine Kategorie hinzu.
    .w-new-category = Neue Kategorie

d-abono4ra = Neuer Kanal
    .doc = Fügt einer Kategorie einen Textkanal hinzu, über `topic`, wenn es angegeben ist.
    .w-new-channel = new-channel

d-uhenzjex = Problem melden
    .doc = Meldet ein Problem, auf das jemand beim Benutzen von Tilo gestoßen ist, im Kanal #feedback des Arbeitsbereichs, den die erste Meldung eröffnet.
    .did = hat ein Problem gemeldet
    .w-say-what-went-wrong-first = Sag zuerst, was schiefgegangen ist

d-uhenzjex-body = Text

d-v6wnfixl = Problem melden~present

d-abono4ra-topic = Thema

d-3cetzzm3 = Nachricht
    .doc = Öffnet die Direktnachricht mit `who` in der Seitenleiste, oder die schon offene mit ihm: eine Person hat eine Unterhaltung, nicht jedes Mal eine neue leere.

d-peb7lz2j = Senden
    .doc = Veröffentlicht eine Nachricht in einem Kanal, einem Thread oder einer Direktunterhaltung als der, der es aufträgt, und antwortet auf eine andere, wenn `reply_to` eine nennt.
    .did = { $count ->
        [one] schrieb eine Nachricht
       *[other] schrieb { $count } Nachrichten
    }

d-634q3q33 = Schreiben an
    .doc = Schreibt `body` an `who`, eine Person oder einen Agenten, in der einen Direktunterhaltung mit ihnen: der schon offenen, sonst einer neuen in der Seitenleiste.

d-qaa5kdra = Eine Ansicht senden
    .doc = Sendet eine Ansicht als Nachricht: ihren Titel als das, was die Nachricht sagt, ihre Definition, damit jeder, der sie liest, sie den eigenen Ansichten hinzufügen kann.
    .w-the-view-is-empty = Die Ansicht ist leer

d-255qjx7d = Bearbeiten
    .doc = Ändert, was eine Nachricht sagt.
    .w-only-its-author-can-edit-a = Nur ihr Autor kann eine Nachricht bearbeiten
    .w-type-a-message-first = Schreib zuerst eine Nachricht

d-vhmhrq2t = Löschen
    .doc = Löscht eine Nachricht aus dem Kanal oder Thread, in dem sie steht.

d-2ojqeubz = Anheften
    .doc = Heftet eine Nachricht an ihren Kanal an, oder löst sie wieder.
    .w-pin = Anheften
    .w-unpin = Lösen

d-np3mn65k = Reagieren
    .doc = Fügt einer Nachricht deine Reaktion mit einem Emoji hinzu, oder nimmt sie zurück, wenn du schon damit reagiert hattest.

d-6uysx57s = Thread beginnen
    .doc = Beginnt einen Thread aus einer Nachricht eines Kanals oder einer Direktunterhaltung, benannt nach dem, was die Nachricht sagt.

d-dgrvpxre = Ansicht senden~present

d-comllwls = Bearbeiten~present

d-c2x2a6dh = Anheften~present

d-dzzuovny = Ungelesen
    .w-unread = Ungelesen
    .w-show-all = Alle { $n } zeigen

d-5rkfnkg4 = Seitenleiste
    .w-channels = Kanäle
    .w-no-channels-yet = Noch keine Kanäle
    .w-channels-are-where-your-workspace-talks = Kanäle sind, wo dein Arbeitsbereich spricht, jeder ein Thema.
    .w-direct-messages = Direktnachrichten
    .w-no-channel-open = Kein Kanal offen

d-ymtess4x = Kanal
    .w-message = Nachricht an #{ $name }
    .w-send = Senden

d-jbezqvlc = Direkt
    .w-message = Nachricht an { $name }
    .w-send = Senden

d-q45utnmh = Kanalübersicht

d-enkldjck = Direktübersicht

d-oqg63mfd = Kategorieübersicht

d-enkgjyn7 = Thread
    .w-message = Nachricht an { $name }
    .w-send = Senden

d-rydgecpc = Hier wird geschrieben
    .w-is-typing = { $who } schreibt …
    .w-and-others-are-typing = { $who } und andere schreiben …

d-tx3zjawu = Hier wird direkt geschrieben
    .w-is-typing = { $who } schreibt …
    .w-and-others-are-typing = { $who } und andere schreiben …

d-dcprca6g = Hier wird im Thread geschrieben
    .w-is-typing = { $who } schreibt …
    .w-and-others-are-typing = { $who } und andere schreiben …

d-hjw5xd6b = Gesagt
    .w-a-view-to-add-to-your = Eine Ansicht, die du den eigenen hinzufügen kannst
    .w-edited = bearbeitet

# What each argument is called where a person gives it.

d-msf7yzn7-name = Name

d-26f7sh5k-name = Name

d-abono4ra-name = Name

d-3cetzzm3-who = Wer

d-peb7lz2j-to = An

d-peb7lz2j-body = Text

d-peb7lz2j-reply_to = Antwort auf

d-634q3q33-who = Wer

d-634q3q33-body = Text

d-qaa5kdra-to = An

d-qaa5kdra-title = Titel

d-qaa5kdra-definition = Definition

d-255qjx7d-body = Text

d-vhmhrq2t-to = An

d-vhmhrq2t-m = M

d-2ojqeubz-pinned = Angeheftet

d-np3mn65k-emoji = Emoji

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

d-egiu6ag4 = Neuer Sprachkanal
    .doc = Fügt der Seitenleiste einen Sprachkanal hinzu.
    .w-voice = Sprache

d-egiu6ag4-name = Name

d-bjtbjfox = Neuer Sprachkanal
    .doc = Fügt einer Kategorie einen Sprachkanal hinzu.
    .w-voice = Sprache

d-bjtbjfox-name = Name

d-qnkkcusk = Sprachkanal

d-lkjppscz = Sprachkanalübersicht

d-tz5yw5dt = Von
    .doc = Wer ihn im früheren Chat geschrieben hat, so wie jene App die Person nannte, für eine von dort übernommene Nachricht: die eigene Historie nennt, wer sie übernommen hat, und wann.

d-mmlcexsa = Geschrieben
    .doc = Wann er im früheren Chat geschrieben wurde, in Millisekunden seit 1970; 0 für eine hier geschriebene Nachricht.

d-jcgolgdf = Reagiert
    .doc = Die Reaktionen, die er im früheren Chat hatte, jedes Emoji mit der Zahl derer, die damit reagiert haben: wer dort reagiert hat, wird nicht übernommen, da diese App Personen nur als ihre eigenen Mitglieder nennt.

d-aod7dups = Früheren Chat übernehmen
    .doc = Die Unterhaltungen, die der frühere Chat vor der Übernahme dieser Kennung hielt, einmalig in diese übernommen: seine Kategorien und Kanäle in der Reihenfolge der Seitenleiste, jeweils mit Name und Thema, und jede Nachricht eines Kanals mit ihrem Wortlaut, ob sie angeheftet war und welche Kennung sie dort hatte.

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

d-jozxp5qy = Kanal löschen
    .doc = Löscht einen Kanal und was darin gesagt wurde.

d-uukhgkjq = Kategorie löschen
    .doc = Löscht eine Kategorie, die keinen Kanal enthält.
    .w-move-or-delete-its-channels-first = Verschiebe oder lösche zuerst ihre Kanäle

d-7sjai347 = Melden
    .doc = Meldet eine Nachricht einer Unterhaltung an die Moderatoren des Arbeitsbereichs, mit Grund, wenn die meldende Person einen nennt.
    .did = hat eine Nachricht gemeldet

d-7sjai347-to = In

d-7sjai347-m = Nachricht

d-7sjai347-reason = Grund

d-qfzfo5jh = Entscheiden
    .doc = Schließt eine Meldung ab: was eine Moderatorin oder der Moderationsagent entschieden hat.
    .w-say-what-was-decided = Sag, was entschieden wurde

d-qfzfo5jh-decision = Entscheidung

d-tz5yw5dt = Von
    .doc = Wer sie dort geschrieben hat, wo sie geschrieben wurde, so wie dieser Ort die Person nannte, bei einer aus dem nativen Chat übernommenen oder von einem anderen Dienst weitergeleiteten Nachricht: ihr eigener Verlauf nennt, wer sie übernommen oder weitergeleitet hat.

d-mmlcexsa = Geschrieben
    .doc = Wann sie dort geschrieben wurde, in Millisekunden seit 1970; 0 bei einer hier geschriebenen Nachricht.

d-jcgolgdf = Reaktionen dort
    .doc = Die Reaktionen, die sie im nativen Chat hatte, jedes Emoji mit der Zahl derer, die damit reagiert haben: wer dort reagiert hat, wird nicht übernommen, da diese App Personen nur als ihre eigenen Mitglieder nennt.

d-aod7dups = Nativen Chat übernehmen
    .doc = Die Unterhaltungen, die der native Chat führte, bevor diese App seine Kennung übernahm, einmal hierher übernommen: seine Kategorien und Kanäle in der Reihenfolge der Seitenleiste, jeder mit Name und Thema, und jede Nachricht eines Kanals mit dem, was sie sagte, ob sie angeheftet war, und der Kennung, die sie dort hatte.

d-6372fg6z-to = In

d-6372fg6z-body = Text

d-6372fg6z-by = Von

d-6372fg6z-written_in = Geschrieben in

d-6372fg6z-written = Geschrieben

d-6372fg6z-reply_to = Antwort auf

d-jozxp5qy-from = Aus

d-jozxp5qy-ch = Kanal

d-uukhgkjq-c = Kategorie

d-psjj65el = Weiterleiten~present

d-gwrwo3w5 = Kategorie löschen~present

d-5ips5yl3 = Entscheiden~present
