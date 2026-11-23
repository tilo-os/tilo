# Agents auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-agent = Agenten

d-zbfroywd = Anfrage
    .doc = Jemand hat einen Agenten etwas über eine Ressource gefragt: indem er ihn in einer Chat-Nachricht, einer Notiz oder einer Aufgabe erwähnt, ihm direkt schreibt oder von der Arbeitsfläche aus.
    .noun = { $count ->
        [one] Anfrage
       *[other] Anfragen
    }

d-mcsi6vp5 = Agent
    .doc = Der gefragte Agent.

d-7j5pok54 = Gesagt
    .doc = Was gefragt wurde, so wie es beim Fragen lautete: eine spätere Änderung des Textes fragt nichts Neues.

d-hw6dxbyx = Erwähnung
    .doc = Die Erwähnung, die gefragt hat: die Referenz des Agenten, wie der Text sie markiert; leer für eine direkt gestellte Anfrage.

d-wtm364ne = Zustand

d-zbfroywd-filter-agent = Agent

d-zbfroywd-filter-state = Zustand

d-bryguujp = Schritt
    .doc = Ein Schritt eines Zuges: eine Aktion, die der Agent aufgerufen hat, mit dem, was sie geantwortet hat.
    .noun = { $count ->
        [one] Schritt
       *[other] Schritte
    }

d-ippal7k2 = Aufgerufen
    .doc = Die Aktion, die er aufgerufen hat, nach ihrer Adresse.

d-cyu3le2j = Geantwortet
    .doc = Was sie geantwortet hat, wie der Agent sie gelesen hat.

d-cma7vgpx = Fehlgeschlagen
    .doc = Ob sie fehlgeschlagen ist.

d-bryguujp-filter-failed = Fehlgeschlagen

d-xhzlueny = Gesendete Ansicht
    .doc = Eine Ansicht, mit der ein Agent geantwortet hat, wie eine Chat-Nachricht eine sendet: wie sie heißt und die Ansicht als tilolang-Quelltext, live für jeden Leser gezeichnet, während er liest.
    .noun = { $count ->
        [one] Gesendete Ansicht
       *[other] Gesendete Ansichten
    }

d-gdhl4pzx = Titel

d-ytw765gn = Definition

d-bdijpmdi = Gezeigtes
    .doc = Ein Ding, das ein Agent in seiner Antwort zeigt: eine Karte davon, live, die es öffnet.
    .noun = { $count ->
        [one] Gezeigtes
       *[other] Gezeigte
    }

d-mx5wcfl2 = Karte
    .doc = Die Karte: ein Absatz, der das Ding projiziert.

d-uwopf5u7 = Antwort
    .doc = Die Antwort eines Agenten auf eine Anfrage, während er sie schreibt.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }

d-gjd462ep = Anfrage

d-oka345cb = Inhalt
    .doc = Die Antwort, die wächst, während der Agent schreibt.

d-czzg7mhh = Gedanken
    .doc = Was das Modell des Agenten unterwegs gedacht hat, wo sein Anbieter es zeigt: wachsend, während es denkt, und nie Teil der Antwort.

d-hzlmnzzw = Zustand

d-tnkzsmes = Versuch
    .doc = Das wievielte Mal gefragt wurde: die erste Antwort ist 1.

d-e36wbuu7 = Schritte
    .doc = Was der Agent unterwegs aufgerufen hat, der Reihe nach.

d-bnlsdtf6 = Ansichten
    .doc = Die Ansichten, mit denen er geantwortet hat, der Reihe nach.

d-mwwrjl52 = Gezeigte
    .doc = Die Dinge, die er gezeigt hat, der Reihe nach.

d-vbzsd5lu = Anbieter
    .doc = Der Anbieter, der die Antwort gegeben hat, wie ihn die Einstellungen des Arbeitsbereichs nennen (`Google`): leer, wenn kein Modell geantwortet hat.

d-5xeeayip = Modell
    .doc = Das Modell, das geantwortet hat, wie es eine Person liest (`Gemini 2.5 Pro`), oder seine Kennung, wenn sein Anbieter ihm keinen Namen gibt: leer, wenn kein Modell geantwortet hat.

d-4xymfj5v = Kosten
    .doc = Was die Modellaufrufe des Durchlaufs gekostet haben, in Millionsteln der Währung des Arbeitsbereichs.

d-eyh6zmbf = Überholt
    .doc = Ob die Antwort eines anderen Geräts auf dieselbe Anfrage stattdessen steht: zwei Geräte, gleichzeitig offline, können beide geantwortet haben.

d-uwopf5u7-filter-ask = Anfrage

d-uwopf5u7-filter-state = Zustand

d-uwopf5u7-filter-attempt = Versuch

d-uwopf5u7-filter-cost = Kosten

d-uwopf5u7-filter-superseded = Überholt

d-kprk52dn = Zusammenfassung
    .doc = Was ein Agent von dem behält, was eine Unterhaltung vor dem Teil gesagt hat, den ein Zug sieht, damit eine Unterhaltung länger als ein Zug behalten kann, was sie früher gesagt hat.
    .noun = { $count ->
        [one] Zusammenfassung
       *[other] Zusammenfassungen
    }

d-fbk2rxka = Agent
    .doc = Der Agent, der sie behält.

d-wl4g23ke = Text
    .doc = Was die Unterhaltung vorher gesagt hat, in den Worten des Agenten.

d-hefqltbc = Bis
    .doc = Das Element, das sie erreicht: die Nachricht der Unterhaltung, die der Agent zuletzt aufgenommen hat.

d-cs27n4h5 = Bis wann
    .doc = Wann dieses Element geschrieben wurde, ISO-8601 in UTC.

d-kprk52dn-filter-agent = Agent

d-7nochn55 = Agenten
    .doc = Jede Anfrage und Antwort des Arbeitsbereichs und was seine Agenten von seinen langen Unterhaltungen behalten.
    .noun = { $count ->
        [one] Agent
       *[other] Agenten
    }

d-mzzoxssv = Anfragen

d-353i7jsi = Antworten

d-srbtof5j = Zusammenfassungen

d-zofobn2n = Anfragezustand
    .doc = Wo eine Anfrage steht.

d-lkb24n4s = Gefragt, und noch hat kein Gerät den Zug übernommen
    .doc = Gefragt, und noch hat kein Gerät den Zug übernommen.

d-a2isbfl6 = Ein Gerät hat den Zug übernommen und der Agent arbeitet daran
    .doc = Ein Gerät hat den Zug übernommen und der Agent arbeitet daran.

d-h6ysttlf = Der Agent hat geantwortet
    .doc = Der Agent hat geantwortet.

d-jjhgercj = Der Zug endete ohne Antwort; seine Antwort sagt, warum
    .doc = Der Zug endete ohne Antwort; seine Antwort sagt, warum.

d-4rnpzqhx = Eine Person hat den Zug angehalten, während er lief
    .doc = Eine Person hat den Zug angehalten, während er lief.

d-rcdvp2yi = Die fragende Person hat sie zurückgenommen oder die Erwähnung entfernt, bevor ein Gerät den Zug übernahm
    .doc = Die fragende Person hat sie zurückgenommen oder die Erwähnung entfernt, bevor ein Gerät den Zug übernahm.

d-ovntytjy = Der Agent darf nicht lesen, wonach gefragt wurde; deshalb wurde nichts davon hierher kopiert
    .doc = Der Agent darf nicht lesen, wonach gefragt wurde; deshalb wurde nichts davon hierher kopiert.

d-szvp4mak = Fragen
    .doc = Fragt `agent` nach der Ressource, von der die Anfrage ausgeht (`.made_from(r)`).
    .did = fragte

d-szvp4mak-agent = Agent

d-szvp4mak-said = Gesagt

d-szvp4mak-mention = Erwähnung

d-q24an2fk = Zurücknehmen
    .doc = Nimmt eine Anfrage zurück, bevor ein Gerät ihren Zug übernommen hat.
    .w-only-who-asked-can-withdraw-it = Nur wer gefragt hat, kann sie zurücknehmen
    .w-the-agent-is-already-on-it = Der Agent ist schon daran

d-so4an2tr = Erneut fragen
    .doc = Fragt erneut, was zurückgenommen wurde: die Erwähnung, die sie gemacht hat, ist wieder da, wie Wiederherstellen sie zurückbringt.
    .w-only-who-asked-can-ask-it = Nur wer gefragt hat, kann erneut fragen
    .w-it-was-not-withdrawn = Sie wurde nicht zurückgenommen

d-xamg74g2 = Erneut versuchen
    .doc = Fragt erneut nach einer Antwort, die fehlgeschlagen ist: dieselbe Anfrage, ihr nächster Versuch, sodass die jetzt geschriebene Antwort neben der fehlgeschlagenen steht.
    .w-only-who-asked-can-ask-it = Nur wer gefragt hat, kann erneut fragen
    .w-it-did-not-fail = Sie ist nicht fehlgeschlagen

d-5zilxes5 = Anhalten
    .doc = Hält einen laufenden Zug an; was der Agent bis dahin getan hat, bleibt.
    .w-nothing-is-working-on-it = Nichts arbeitet daran

d-zl7upzy7 = Antworten
    .doc = Der Agent beginnt seine Antwort auf `a`, den nächsten Versuch der Anfrage: den nach seiner neuesten Antwort, sodass eine fehlgeschlagene, erneut gefragte Antwort neben ihrer steht.
    .did = antwortete
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-nobody-waits-for-an-answer = Niemand wartet auf eine Antwort

d-s6gerkjs = Sagen
    .doc = Der Agent schreibt `more` an das Ende seiner Antwort.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist fertig

d-s6gerkjs-more = Mehr

d-jjuslny3 = Denken
    .doc = Das Modell des Agenten hat unterwegs `more` gedacht, wie sein Anbieter es zeigt: am Ende der Gedanken der Antwort, neben seiner Antwort.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist fertig

d-jjuslny3-more = Mehr

d-6s74tahg = Schritt
    .doc = Der Agent hat unterwegs `called` aufgerufen, und es hat `answered` geantwortet.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist fertig

d-6s74tahg-called = Aufgerufen

d-6s74tahg-answered = Geantwortet

d-6s74tahg-failed = Fehlgeschlagen

d-jriyp5ei = Ansicht zeigen
    .doc = Der Agent antwortet mit einer Ansicht: `definition`, genannt `title`.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist fertig
    .w-the-view-is-empty = Die Ansicht ist leer

d-jriyp5ei-title = Titel

d-jriyp5ei-definition = Definition

d-lym5tesj = Zeigen
    .doc = Der Agent zeigt das Ding, das `card` projiziert, live in seiner Antwort.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist fertig

d-lym5tesj-card = Karte

d-6koj42w5 = Beenden
    .doc = Der Zug des Agenten endete als `state`: erledigt, fehlgeschlagen oder angehalten, wenn eine Person ihn angehalten hat.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-a-turn-ends-done-failed-or = Ein Zug endet als erledigt, fehlgeschlagen oder angehalten

d-6koj42w5-state = Zustand

d-6koj42w5-provider = Anbieter

d-6koj42w5-model = Modell

d-6koj42w5-cost = Kosten

d-5n4bbzji = Überholen
    .doc = Die Antwort eines anderen Geräts auf dieselbe Anfrage steht statt `r`.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten

d-q7wtfy6u = Zusammenfassung behalten
    .doc = Der Agent behält, was die Unterhaltung, von der dies ausgeht, vor dem Teil gesagt hat, den sein Zug gesehen hat: `text` sagt es in den Worten des Agenten und reicht bis `through`, dem Element, das er zuletzt aufgenommen hat.
    .w-only-the-agent-may-keep-its = Nur der Agent darf seine Zusammenfassung behalten

d-q7wtfy6u-agent = Agent

d-q7wtfy6u-text = Text

d-q7wtfy6u-through = Bis

d-q7wtfy6u-through_at = Bis wann

d-koyxjtxv = Zurücknehmen~present

d-rxmz2d33 = Erneut fragen~present

d-za2dxf3d = Erneut versuchen~present

d-mzbpkahz = Anhalten~present

d-o6oeh6vp = Antworten~present

d-hdzxkm76 = Sagen~present

d-645ej5zc = Denken~present

d-k36fktxl = Schritt~present

d-zasvncsi = Ansicht zeigen~present

d-64ypkzcb = Zeigen~present

d-3ebfgkt2 = Beenden~present

d-wsxdear2 = Überholen~present

d-hcwspess = Zusammenfassung behalten~present

d-zfzqq5xy = Gefragt
    .w-nobody-asked-an-agent-yet = Noch hat niemand einen Agenten gefragt

d-bigklctq = Anfragezeile

d-7fu4ujho = Antwort
    .w-thinking = Denkt nach
    .w-thought = Nachgedacht

d-sglhetw2 = Antworten

d-rwsixj7g = Anfrageseite
    .w-the-agent-may-not-read-what = Der Agent darf nicht lesen, wonach gefragt wurde.
