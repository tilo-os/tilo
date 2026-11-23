# Chat auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-chat = Chat

d-so-tilo-lang-chat-keywords = nachrichten, chat, unterhaltung, gespräch, messenger, sofortnachrichten

d-so-tilo-lang-chat-keywords = nachrichten, chat, unterhaltung, gespräch, messenger, sofortnachrichten

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

d-fp7zx7jn = Fähigkeit
    .doc = Wie eine Art Arbeit in diesem Arbeitsbereich getan wird: was eine Person einmal festhält und was ein Agent liest, bevor er diese Arbeit beginnt.
    .noun = { $count ->
        [one] Fähigkeit
       *[other] Fähigkeiten
    }
    .untitled = Neue Fähigkeit

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

d-tkh5jonr = Personen der Direktnachrichten übernehmen
    .doc = Die Direktnachrichten, die eine frühere Version mit nur einer Person führte (`person`) und die jedes Mitglied lesen konnte: in jeder sind diese Person und wer sie geöffnet hat, wie ihr Verlauf sagt.

d-3cetzzm3 = Nachricht
    .doc = Öffnet die Direktnachricht zwischen dem Aufrufer und `who` in der Seitenleiste, oder die schon offene zwischen den beiden: zwei Personen haben eine Unterhaltung, nicht jedes Mal eine neue leere.

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

d-clq6mq5o = Neue Fähigkeit~present

d-umfqj3us = Fähigkeit korrigieren~present

d-3e27liaz = Beispiel aufbewahren~present

d-comllwls = Bearbeiten~present

d-c2x2a6dh = Anheften~present

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
    .w-direct-messages = Direktnachrichten
    .w-workspace = Arbeitsbereich
    .w-workspace-skills = Fähigkeiten im Arbeitsbereich
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

d-hrbsxhor = Fähigkeitszeile

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

d-dra3nelb = Neue Fähigkeit
    .doc = Beginnt eine Fähigkeit namens `name`.
    .w-the-workspace-already-has-a-skill = Der Arbeitsbereich hat schon eine Fähigkeit namens { $name }

d-dra3nelb-name = Name

d-fvl622e2 = Fähigkeit korrigieren
    .doc = Korrigiert eine Fähigkeit: nur das Angegebene ändert sich, und was fehlt, bleibt, wie es ist.
    .w-the-workspace-already-has-a-skill = Der Arbeitsbereich hat schon eine Fähigkeit namens { $name }

d-fvl622e2-name = Name

d-fvl622e2-work = Wofür sie gilt

d-fvl622e2-how = So wird sie getan

d-fvl622e2-form = Form des Ergebnisses

d-fvl622e2-model = Modell

d-wmxknkkc = Fähigkeit vergessen
    .doc = Vergisst eine Fähigkeit: keine spätere Runde liest sie.

d-4fojczoe = Ein Beispiel aufbewahren
    .doc = Bewahrt ein Beispiel einer Fähigkeit auf: ein Ergebnis, das die Person angenommen (mehr davon) oder abgelehnt (weniger davon) hat, mit dem, was sie dazu gesagt hat.
    .w-an-example-says-what-the-result = Ein Beispiel sagt, was das Ergebnis war

d-4fojczoe-text = Text

d-4fojczoe-accepted = Angenommen

d-4fojczoe-note = Notiz

d-aqrwkeib = Das Beispiel entfernen
    .doc = Nimmt ein Beispiel aus einer Fähigkeit heraus.

d-aqrwkeib-example = Beispiel

d-qnkkcusk = Sprachkanal

d-lkjppscz = Sprachkanalübersicht

d-aod7dups = Früheren Chat übernehmen
    .doc = Die Unterhaltungen, die der frühere Chat vor der Übernahme dieser Kennung hielt, einmalig in diese übernommen: seine Kategorien und Kanäle in der Reihenfolge der Seitenleiste, jeweils mit Name und Thema, und jede Nachricht eines Kanals mit ihrem Wortlaut, ob sie angeheftet war und welche Kennung sie dort hatte.

d-uuhhrrha = Frühere Fähigkeiten übernehmen
    .doc = Die Fähigkeiten des Arbeitsbereichs, die der frühere Chat vor der Übernahme dieser Kennung hielt, einmalig in diese übernommen: wie jede heißt, welche Arbeit sie betrifft, wie die Arbeit hier getan wird, die Form ihres Ergebnisses und das Modell, unter der Kennung, die sie dort hatte, und in der Reihenfolge, in der sie stand, und die Beispiele, die sie aufbewahrte, älteste zuerst, jedes mit dem, was das Ergebnis war, ob die Person es angenommen hat, was sie dazu gesagt hat und wann es aufbewahrt wurde.

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

d-6372fg6z-to = In

d-6372fg6z-body = Text

d-6372fg6z-by = Von

d-6372fg6z-written_in = Geschrieben in

d-6372fg6z-written = Geschrieben

d-6372fg6z-reply_to = Antwort auf

d-bhn35nvx = Reaktion weiterleiten
    .doc = Leitet eine Reaktion weiter, die jemand einer Nachricht dort gab, wo sie geschrieben wurde, so wie jener Ort die Person nennt: das Gegenstück zu „Weiterleiten“ für eine Reaktion.
    .did = hat eine Reaktion weitergeleitet
    .w-say-which-emoji = Sag, welches Emoji
    .w-say-who-gave-the-reaction = Sag, wer die Reaktion gab
    .w-say-what-they-are-called-there = Sag, wie die Person dort heißt

d-bhn35nvx-emoji = Emoji

d-bhn35nvx-persona = Konto

d-bhn35nvx-by = Von

d-bhn35nvx-given = Gegeben

d-jozxp5qy-from = Aus

d-jozxp5qy-ch = Kanal

d-uukhgkjq-c = Kategorie

d-psjj65el = Weiterleiten~present

d-xjtinzwn = Reaktion weiterleiten~present

d-gwrwo3w5 = Kategorie löschen~present

d-5ips5yl3 = Entscheiden~present

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

d-ggtglnmb = Kanal verschieben
    .doc = Verschiebt einen Kanal an eine Stelle der Seitenleiste: unter `category` oder außerhalb jeder, an `at` unter den dort gelisteten Kanälen (am Ende, wenn negativ).

d-ggtglnmb-ch = Kanal

d-ggtglnmb-category = Kategorie

d-ggtglnmb-at = Stelle

d-n4l2mod2 = Kategorie verschieben
    .doc = Verschiebt eine Kategorie an eine Stelle der Seitenleiste, an `at` unter den dort gelisteten Kategorien und Kanälen (am Ende, wenn negativ).

d-n4l2mod2-c = Kategorie

d-n4l2mod2-at = Stelle

d-peb7lz2j-attachments = Anhänge
