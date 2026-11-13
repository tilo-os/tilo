# Chat auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-lang-chat = Chat

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
    .doc = Fügt der Seitenleiste einen Textkanal hinzu.
    .w-new-channel = new-channel

d-26f7sh5k = Neue Kategorie
    .doc = Fügt der Seitenleiste eine Kategorie hinzu.
    .w-new-category = Neue Kategorie

d-abono4ra = Neuer Kanal
    .doc = Fügt einer Kategorie einen Textkanal hinzu.
    .w-new-channel = new-channel

d-3cetzzm3 = Nachricht
    .doc = Öffnet die Direktnachricht mit `who` in der Seitenleiste, oder die schon offene mit ihm: eine Person hat eine Unterhaltung, nicht jedes Mal eine neue leere.

d-peb7lz2j = Senden
    .doc = Veröffentlicht eine Nachricht in einem Kanal, einem Thread oder einer Direktunterhaltung als der, der es aufträgt, und antwortet auf eine andere, wenn `reply_to` eine nennt.

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
