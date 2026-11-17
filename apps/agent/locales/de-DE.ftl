# Agent — deutsche Sprache (de-DE).

## Was der Agent der Person sagt, deren Aufgabe er ausführt.
##
## Antwort, Fortschritt und Fehler eines Laufs sind die eigenen Worte des
## Agenten; sie stehen hier, in der Sprache, die die Person für ihren Desktop
## gewählt hat. Was ein Modell antwortet, steht nicht hier: es kommt in der
## Sprache, in der es gefragt wurde.

## Wie die Schritte eines Laufs gewählt wurden

agent-plan-model = { $model } von { $provider } plant diesen Lauf.
agent-plan-words-no-model = Für diesen Lauf ist kein Modell eingerichtet, deshalb werden seine Schritte den Aktionen zugeordnet, die die Apps dieses Arbeitsbereichs veröffentlichen. Wähle ein Modell in den Einstellungen unter „Agentenmodelle“, damit es stattdessen plant.
agent-plan-no-action = Keine veröffentlichte Aktion passt zu den Wörtern der Anfrage.
agent-plan-no-model = So kann ich noch nicht arbeiten: Es ist kein Modell eingerichtet. Wähle eines in den Einstellungen unter „Agentenmodelle“.
agent-plan-models-off = Für diesen Agenten sind Modelle abgeschaltet, deshalb kann nichts diesen Lauf planen. Starte ihn erneut mit einem eingeschalteten Modell.
agent-plan-model-failed = Das Modell hat nicht geantwortet, deshalb ließ sich dieser Lauf nicht planen. Versuche es gleich noch einmal.

## Wie ein Lauf geendet hat

agent-done-words-no-model = { $actions ->
        [one] Fertig: eine Aktion den Wörtern der Anfrage zugeordnet, ohne Modell.
       *[other] Fertig: { $actions } Aktionen den Wörtern der Anfrage zugeordnet, ohne Modell.
    }

## Anfragen, zu denen keine Aktion passte

agent-match-nothing = Das kann ich hier nicht erledigen.
agent-match-several = Das kann mehreres bedeuten. Welches davon?
agent-match-one-of = Das kann { $actions } bedeuten. Welches davon?
agent-match-or = { $a } oder { $b }
agent-match-needs-text = „{ $label }“ braucht die Wörter, mit denen es arbeiten soll, in Anführungszeichen.

## Der Fortschritt eines Laufs

agent-session-reading-actions = Liest die Aktionen, die die Apps dieses Arbeitsbereichs veröffentlichen
agent-session-step = { $index } von { $total }: { $action }
agent-model-turn-completed = Modellzug abgeschlossen
agent-step-searching = Sucht im Web nach „{ $what }“…
agent-step-reading = Liest { $what }…
agent-step-editing = Ändert { $what }…
agent-step-exploring = Sieht nach, was { $what } kann…
agent-step-program = Nachgeschlagen
agent-step-looked-up = { $what } nachgeschlagen
agent-step-on = { $action }: { $subject }
agent-step-edited = { $what } bearbeitet
agent-step-searched = Im Web nach „{ $what }“ gesucht
agent-step-read = { $what } gelesen

## Ein Lauf, der angehalten wurde

agent-session-needs-decision = Braucht deine Entscheidung, um weiterzumachen
agent-session-cancelled-before-planning = Vor der Planung abgebrochen
agent-session-cancelled-before-action = Vor der nächsten Aktion abgebrochen
agent-session-cancelled-before-step = Vor dem nächsten Schritt abgebrochen
agent-session-cancelled-in-action = Während einer laufenden Aktion abgebrochen
agent-cancel-task-center = Im Task-Center abgebrochen
agent-cancel-thinking = Während des Nachdenkens abgebrochen

## Ein Lauf, der fehlgeschlagen ist, und was die Person tun kann

agent-fail-no-terminal-state = Die Aktion der Aufgabe hat ihr Zeitlimit erreicht, ohne zu enden.
agent-fail-local-only = Aufgaben können keine Aktion ausführen, die nur auf diesem Gerät läuft.
agent-fail-arguments = Die Argumente der geplanten Aktion passen nicht: { $error }
agent-fail-turn-not-finished = Der Modellzug war nicht rechtzeitig fertig
agent-fail-native-outcome = Das Ergebnis der Aktion ist weiterhin unbekannt; prüfe den Verlauf, bevor du es erneut versuchst.
agent-fail-spend-limit = { $agent } hat das heutige { $spent } aufgebraucht; bitte die Person, die es verwaltet, das Limit zu erhöhen

# Was ein fehlgeschlagener Modellaufruf in der Antwort hinterlässt, die eine
# Person liest: ein Satz dazu, was geschehen ist, in ihren Worten, und wenn es
# eine Einstellung zu prüfen gibt, welche. Der Fehler des Transports, die
# teilweise Antwort, die offengelegte Begründung des Anbieters und die
# Anweisung, der der Agent folgt, gehören zum Arbeitsprotokoll des Laufs,
# nicht zu dieser Copy.
agent-failure-unavailable = Der Dienst des Modells ist gerade nicht erreichbar.
agent-failure-no-provider = Es ist kein Modell eingerichtet. Wähle eines in den Einstellungen unter Agentenmodelle.
agent-failure-authentication = Der Anbieter des Modells hat seine Zugangsdaten nicht akzeptiert. Prüfe sie in den Einstellungen.
agent-failure-permission-denied = Der Anbieter des Modells hat diese Anfrage abgelehnt.
agent-failure-invalid-request = Das Modell konnte diese Anfrage nicht annehmen. Prüfe das Modell in den Einstellungen.
agent-failure-conflict = Die Einstellungen des Modells haben sich während des Laufs geändert. Prüfe das Modell in den Einstellungen.
agent-failure-invalid-response = Die Antwort des Modells ließ sich nicht verwenden.
agent-failure-unsupported = Das Modell kann das nicht. Wähle ein anderes Modell in den Einstellungen unter Agentenmodelle.
agent-failure-rate-limited = Das Modell ist gerade ausgelastet. Versuche es gleich noch einmal.
agent-failure-credits-exhausted = Das Guthaben beim Anbieter des Modells ist aufgebraucht.
agent-failure-context-limit = Das Gespräch ist länger, als das Modell aufnehmen kann. Wähle ein Modell mit mehr Platz.
agent-failure-provider = Der Anbieter des Modells ist fehlgeschlagen. Versuche es noch einmal.
agent-failure-timed-out = Das Modell hat nicht rechtzeitig geantwortet.
agent-failure-cancelled = Der Aufruf des Modells wurde gestoppt.
agent-failure-truncated = Die Antwort des Modells wurde abgeschnitten.
agent-failure-refused = Das Modell hat diese Anfrage abgelehnt.
agent-failure-transport = Das Modell war nicht erreichbar. Versuche es noch einmal.
agent-failure-no-answer = Der Agent hat nichts getan und nicht gesagt, was er getan hat.
agent-failure-empty-answer = Der Agent hat ohne Antwort aufgehört. Frag noch einmal.
agent-failure-stalled = Der Agent hat aufgehört: Er hat weiter gesucht, ohne voranzukommen, und nichts mehr geändert.
