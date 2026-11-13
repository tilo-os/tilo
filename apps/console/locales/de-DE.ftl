console-title = Konsole
console-places = Bereiche der Konsole
console-place-agents = Agenten
console-place-workspaces = Arbeitsbereiche
console-place-connect = Verbinden
console-place-usage = Nutzung

console-filter-agent = Agent
console-filter-workspace = Arbeitsbereich
console-every-agent = Jeder Agent
console-every-workspace = Jeder Arbeitsbereich
console-show-everything = Alles anzeigen

console-runs-working = Jetzt bei der Arbeit
console-runs-earlier = Früher
console-runs-pending = Der Verlauf deiner Arbeitsbereiche wird gelesen
console-runs-empty = Noch kein Agent hat hier gearbeitet
console-runs-empty-message = Verbinde einen Agenten und bitte ihn um etwas: Du kannst seine Läufe verfolgen, während er arbeitet.
console-runs-none-here = Keine Läufe passen
console-runs-none-here-message = Kein Lauf dieses Agenten in diesem Arbeitsbereich. Zeige alles, um den Rest zu sehen.
console-run-actions = { $count ->
    [one] 1 Aktion
   *[other] { $count } Aktionen
}
console-run-changes = { $count ->
    [one] 1 Änderung
   *[other] { $count } Änderungen
}
console-run-started-by-you = Von dir gestartet
console-run-started-by = Von { $name } gestartet

console-run = Agentenlauf
console-run-copy-link = Link kopieren
console-run-pending = Dieser Lauf wird gelesen
console-run-missing = Dieser Lauf ist nicht hier
console-run-missing-message = Der Verlauf kennt keinen solchen Lauf, oder er war in einem Arbeitsbereich, in dem du nicht bist.

console-run-stop = Stoppen
console-run-take-over = Übernehmen
console-run-undo = Lauf rückgängig machen
console-run-why = Warum
console-run-timeline = { $count ->
    [one] 1 Aktion
   *[other] { $count } Aktionen
}
console-action-open = Öffnen
console-action-no-preview = Diese Änderung zeigt keinen Unterschied zur Vorschau.
console-command-exit = Mit { $code } beendet
console-command-exit-after = Mit { $code } nach { $took } beendet
console-command-running = Läuft…
console-command-in = in { $cwd }
console-command-full = Gesamte Ausgabe zeigen
console-command-less = Weniger zeigen
console-command-output-unavailable = Seine Ausgabe hat dieses Gerät noch nicht erreicht.
console-now-running = Führt { $command } aus…
console-now-editing = Bearbeitet { $path }
console-now-browsing = Auf { $page }
console-now-ran = Hat { $command } ausgeführt

console-workspaces-empty = Noch keine Arbeitsbereiche
console-workspaces-empty-message = Lege einen in Tilo an, oder bitte einen Agenten, einen für seine Arbeit anzulegen.
console-workspace-made-by-you = Von dir angelegt
console-workspace-made-by = Von { $agent } angelegt
console-workspace-open = In Tilo öffnen

console-connect-lead = Führe einen dieser Befehle dort aus, wo dein Agent läuft. Er bittet dich einmal um Anmeldung und arbeitet dann als er selbst in deinen Arbeitsbereichen. Alles, was er tut, erscheint unter Agenten, wo du es stoppen oder rückgängig machen kannst.
console-connect-heading = Einen Agenten verbinden
console-client-claude-code = Claude Code
console-client-codex = Codex
console-client-other = Jeder MCP-Client
console-client-every-run = Jeder Lauf von { $client }
console-grants-heading = Verbundene Agenten
console-grants-empty = Noch kein Agent verbunden
console-reach-account = Jeder Arbeitsbereich, und darf neue anlegen
console-reach-workspace = Nur { $workspace }
console-grant-browser-pages = Browserseiten
console-grant-running = { $count ->
    [one] Jetzt bei der Arbeit
   *[other] { $count } Läufe jetzt bei der Arbeit
}
console-grant-last-active = Zuletzt aktiv { $when }
console-grant-granted = Hereingelassen { $when }
console-grant-revoke = Entziehen

console-usage-empty = Noch keine Linux-Rechenzeit
console-usage-empty-message = Ein Agent, der Linux braucht, führt es auf einem eigenen Rechner aus, und diese Zeit wird gezählt.

console-agent-runs = Läufe
console-agent-access = Zugriff
console-agent-no-runs = Noch keine Läufe
console-agent-last-worked = Zuletzt gearbeitet { $when }

## What the app is called in a window and in the launcher
console-app-title = Konsole
