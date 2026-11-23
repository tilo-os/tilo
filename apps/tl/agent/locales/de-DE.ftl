# Agenten auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-agent = Agenten

d-zbfroywd = Frage
    .doc = Jemand hat einen Agenten etwas über eine Ressource gefragt: indem er ihn in einer Chat-Nachricht, einer Notiz oder einer Aufgabe erwähnt hat, indem er direkt an ihn geschrieben hat, oder von der Arbeitsfläche aus.
    .noun = { $count ->
        [one] Frage
       *[other] Fragen
    }

d-mcsi6vp5 = Agent
    .doc = Der gefragte Agent.

d-7j5pok54 = Gesagt
    .doc = Was gefragt wurde, wie es beim Fragen lautete: eine spätere Änderung des Textes fragt nichts Neues.

d-hw6dxbyx = Erwähnung
    .doc = Die Erwähnung, die gefragt hat: die Referenz auf den Agenten, wie der Text sie markiert; leer für eine direkt gestellte Frage.

d-wtm364ne = Status

d-zbfroywd-filter-agent = Agent

d-zbfroywd-filter-state = Status

d-bryguujp = Schritt
    .doc = Ein Schritt eines Durchlaufs: eine Aktion, die der Agent aufgerufen hat, mit ihrer Antwort.
    .noun = { $count ->
        [one] Schritt
       *[other] Schritte
    }

d-ippal7k2 = Aufgerufen
    .doc = Die Aktion, die er aufgerufen hat, nach ihrer Adresse.

d-cyu3le2j = Geantwortet
    .doc = Was sie geantwortet hat, wie der Agent sie gelesen hat.

d-cma7vgpx = Fehlgeschlagen
    .doc = Ob es fehlgeschlagen ist.

d-bryguujp-filter-failed = Fehlgeschlagen

d-xhzlueny = Gesendete Ansicht
    .doc = Eine Ansicht, mit der ein Agent geantwortet hat, wie sie eine Chat-Nachricht sendet: ihr Name und die Ansicht als tilolang-Quelltext, live für jeden Leser gezeichnet, während er liest.
    .noun = { $count ->
        [one] gesendete Ansicht
       *[other] gesendete Ansichten
    }

d-gdhl4pzx = Titel

d-ytw765gn = Definition

d-bdijpmdi = Gezeigt
    .doc = Ein Ding, das ein Agent in seiner Antwort zeigt: eine Karte davon, live, die es öffnet.
    .noun = { $count ->
        [one] Gezeigtes
       *[other] Gezeigte
    }

d-mx5wcfl2 = Karte
    .doc = Die Karte: ein Absatz, der das Ding abbildet.

d-uwopf5u7 = Antwort
    .doc = Die Antwort eines Agenten auf eine Frage, wie er sie schreibt.
    .noun = { $count ->
        [one] Antwort
       *[other] Antworten
    }

d-gjd462ep = Frage

d-oka345cb = Inhalt
    .doc = Die Antwort, die wächst, während der Agent schreibt.

d-czzg7mhh = Denken
    .doc = Was das Modell des Agenten unterwegs gedacht hat, wo sein Anbieter es zeigt: es wächst, während es denkt, und ist nie Teil der Antwort.

d-hzlmnzzw = Status

d-tnkzsmes = Versuch
    .doc = Das wievielte Mal gefragt wurde: die erste Antwort ist 1.

d-e36wbuu7 = Schritte
    .doc = Was der Agent unterwegs aufgerufen hat, der Reihe nach.

d-bnlsdtf6 = Ansichten
    .doc = Die Ansichten, mit denen er geantwortet hat, der Reihe nach.

d-mwwrjl52 = Gezeigt
    .doc = Die Dinge, die er gezeigt hat, der Reihe nach.

d-vbzsd5lu = Anbieter
    .doc = Der Anbieter, der die Antwort gegeben hat, wie ihn die Einstellungen des Arbeitsbereichs nennen (`Google`): leer, wenn kein Modell geantwortet hat.

d-5xeeayip = Modell
    .doc = Das Modell, das geantwortet hat, wie es eine Person liest (`Gemini 2.5 Pro`), oder seine Kennung, wenn sein Anbieter ihm keinen Namen gibt: leer, wenn kein Modell geantwortet hat.

d-4xymfj5v = Kosten
    .doc = Was die Modellaufrufe des Durchlaufs gekostet haben, in Millionsteln der Währung des Arbeitsbereichs.

d-eyh6zmbf = Überholt
    .doc = Ob die Antwort eines anderen Geräts auf dieselbe Frage an seiner Stelle steht: zwei Geräte, gleichzeitig offline, können beide geantwortet haben.

d-uwopf5u7-filter-ask = Frage

d-uwopf5u7-filter-state = Status

d-uwopf5u7-filter-attempt = Versuch

d-uwopf5u7-filter-cost = Kosten

d-uwopf5u7-filter-superseded = Überholt

d-7nochn55 = Agenten
    .doc = Jede Frage und Antwort des Arbeitsbereichs.
    .noun = { $count ->
        [one] Agent
       *[other] Agenten
    }

d-mzzoxssv = Fragen

d-353i7jsi = Antworten

d-zofobn2n = Fragestatus
    .doc = Wo eine Frage steht.

d-lkb24n4s = Gefragt, und noch hat kein Gerät den Durchlauf übernommen
    .doc = Gefragt, und noch hat kein Gerät den Durchlauf übernommen.

d-a2isbfl6 = Ein Gerät hat den Durchlauf übernommen und der Agent arbeitet daran
    .doc = Ein Gerät hat den Durchlauf übernommen und der Agent arbeitet daran.

d-h6ysttlf = Der Agent hat geantwortet
    .doc = Der Agent hat geantwortet.

d-jjhgercj = Der Durchlauf endete ohne Antwort; die Antwort sagt, warum
    .doc = Der Durchlauf endete ohne Antwort; die Antwort sagt, warum.

d-4rnpzqhx = Eine Person hat den Durchlauf während der Arbeit gestoppt
    .doc = Eine Person hat den Durchlauf während der Arbeit gestoppt.

d-rcdvp2yi = Der Fragende hat sie zurückgezogen oder die Erwähnung, die gefragt hat, entfernt, bevor ein Gerät sie übernahm
    .doc = Der Fragende hat sie zurückgezogen oder die Erwähnung, die gefragt hat, entfernt, bevor ein Gerät sie übernahm.

d-ovntytjy = Der Agent darf nicht lesen, wonach gefragt wurde, daher wurde nichts davon hierher kopiert
    .doc = Der Agent darf nicht lesen, wonach gefragt wurde, daher wurde nichts davon hierher kopiert.

d-szvp4mak = Frage
    .doc = Fragt `agent` über die Ressource, aus der die Frage entsteht (`.made_from(r)`).
    .did = fragte

d-szvp4mak-agent = Agent

d-szvp4mak-said = Gesagt

d-szvp4mak-mention = Erwähnung

d-q24an2fk = Zurückziehen
    .doc = Zieht eine Frage zurück, bevor ein Gerät ihren Durchlauf übernommen hat.
    .w-only-who-asked-can-withdraw-it = Nur wer gefragt hat, kann sie zurückziehen
    .w-the-agent-is-already-on-it = Der Agent ist schon dabei

d-so4an2tr = Erneut fragen
    .doc = Fragt erneut, was zurückgezogen wurde: die Erwähnung, die sie gestellt hat, ist wieder da, wie Rückgängig sie zurückbringt.
    .w-only-who-asked-can-ask-it = Nur wer gefragt hat, kann erneut fragen
    .w-it-was-not-withdrawn = Sie wurde nicht zurückgezogen

d-5zilxes5 = Stoppen
    .doc = Stoppt einen laufenden Durchlauf; was der Agent bis dahin getan hat, bleibt.
    .w-nothing-is-working-on-it = Es läuft nichts dazu

d-zl7upzy7 = Antworten
    .doc = Der Agent beginnt seine Antwort auf `a`, den nächsten Versuch der Frage.
    .did = antwortete
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-nobody-waits-for-an-answer = Niemand wartet auf eine Antwort

d-s6gerkjs = Sagen
    .doc = Der Agent schreibt `more` ans Ende seiner Antwort.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist beendet

d-s6gerkjs-more = Mehr

d-jjuslny3 = Denken
    .doc = Das Modell des Agenten dachte unterwegs `more`, wie sein Anbieter es zeigt: am Ende des Denkens der Antwort, neben seiner Antwort.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist beendet

d-jjuslny3-more = Mehr

d-6s74tahg = Schritt
    .doc = Der Agent rief unterwegs `called` auf, und es antwortete `answered`.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist beendet

d-6s74tahg-called = Aufgerufen

d-6s74tahg-answered = Geantwortet

d-6s74tahg-failed = Fehlgeschlagen

d-jriyp5ei = Ansicht zeigen
    .doc = Der Agent antwortet mit einer Ansicht: `definition`, genannt `title`.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist beendet
    .w-the-view-is-empty = Die Ansicht ist leer

d-jriyp5ei-title = Titel

d-jriyp5ei-definition = Definition

d-lym5tesj = Zeigen
    .doc = Der Agent zeigt das Ding, das `card` abbildet, in seiner Antwort, live.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-the-answer-is-finished = Die Antwort ist beendet

d-lym5tesj-card = Karte

d-6koj42w5 = Beenden
    .doc = Der Durchlauf des Agenten endete als `state`: erledigt, fehlgeschlagen, oder gestoppt, wenn eine Person ihn stoppte.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten
    .w-a-turn-ends-done-failed-or = Ein Durchlauf endet erledigt, fehlgeschlagen oder gestoppt

d-6koj42w5-state = Status

d-6koj42w5-provider = Anbieter

d-6koj42w5-model = Modell

d-6koj42w5-cost = Kosten

d-5n4bbzji = Überholen
    .doc = Die Antwort eines anderen Geräts auf dieselbe Frage steht anstelle von `r`.
    .w-only-the-agent-asked-may-answer = Nur der gefragte Agent darf antworten

d-koyxjtxv = Zurückziehen~present

d-rxmz2d33 = Erneut fragen~present

d-mzbpkahz = Stoppen~present

d-o6oeh6vp = Antworten~present

d-hdzxkm76 = Sagen~present

d-645ej5zc = Denken~present

d-k36fktxl = Schritt~present

d-zasvncsi = Ansicht zeigen~present

d-64ypkzcb = Zeigen~present

d-3ebfgkt2 = Beenden~present

d-wsxdear2 = Überholen~present

d-zfzqq5xy = Fragen
    .w-nobody-asked-an-agent-yet = Niemand hat bisher einen Agenten gefragt

d-bigklctq = Fragezeile

d-7fu4ujho = Antwort
    .w-thinking = Denkt
    .w-thought = Dachte

d-sglhetw2 = Antworten

d-rwsixj7g = Frageseite
    .w-the-agent-may-not-read-what = Der Agent darf nicht lesen, wonach gefragt wurde.
