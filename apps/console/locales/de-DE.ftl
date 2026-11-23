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

console-sessions-working = Jetzt bei der Arbeit
console-sessions-earlier = Früher
console-sessions-pending = Der Verlauf deiner Arbeitsbereiche wird gelesen
console-sessions-empty = Noch kein Agent hat hier gearbeitet
console-sessions-empty-message = Verbinde einen Agenten und bitte ihn um etwas: Du kannst seine Sitzungen verfolgen, während er arbeitet.
console-sessions-none-here = Keine Sitzungen passen
console-sessions-none-here-message = Keine Sitzung dieses Agenten in diesem Arbeitsbereich. Zeige alles, um den Rest zu sehen.
console-session-actions = { $count ->
    [one] 1 Aktion
   *[other] { $count } Aktionen
}
console-session-changes = { $count ->
    [one] 1 Änderung
   *[other] { $count } Änderungen
}
console-session-started-by-you = Von dir gestartet
console-session-started-by = Von { $name } gestartet

console-session = Agentensitzung
console-session-copy-link = Link kopieren
console-session-pending = Diese Sitzung wird gelesen
console-session-missing = Diese Sitzung ist nicht hier
console-session-missing-message = Der Verlauf kennt keine solche Sitzung, oder sie war in einem Arbeitsbereich, in dem du nicht bist.

console-session-stop = Stoppen
console-session-take-over = Übernehmen
console-session-undo = Sitzung rückgängig machen
console-session-why = Warum
console-session-timeline = { $count ->
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

console-connect-lead = Füge diesen Server der MCP-Konfiguration deines Agenten hinzu. Er bittet dich einmal um Anmeldung und arbeitet dann als er selbst in deinen Arbeitsbereichen. Alles, was er tut, erscheint unter Agenten, wo du es stoppen oder rückgängig machen kannst.
console-connect-heading = Einen Agenten verbinden
console-connect-setup-lead = Dein Agent arbeitet hier als du, sobald Tilo auf seinem Computer eingerichtet ist. Eine Zeile, die du ihm gibst, richtet es ein und meldet ihn als dich an.
console-connect-setup = Agenten einrichten
console-client-url = Serveradresse
console-client-other = Jeder MCP-Client
console-client-every-session = Jede Sitzung von { $client }
console-grants-heading = Verbundene Agenten
console-grants-empty = Noch kein Agent verbunden
console-reach-account = Jeder Arbeitsbereich, und darf neue anlegen
console-reach-workspace = Nur { $workspace }
console-grant-browser-pages = Browserseiten
console-grant-running = { $count ->
    [one] Jetzt bei der Arbeit
   *[other] { $count } Sitzungen jetzt bei der Arbeit
}
console-grant-last-active = Zuletzt aktiv { $when }
console-grant-granted = Hereingelassen { $when }
console-grant-revoke = Entziehen
console-grant-go-on = Weiterlaufen lassen

console-usage-empty = Noch keine Linux-Rechenzeit
console-usage-empty-message = Ein Agent, der Linux braucht, führt es auf einem eigenen Rechner aus, und diese Zeit wird gezählt.

console-agent-sessions = Sitzungen
console-agent-access = Zugriff
console-agent-no-sessions = Noch keine Sitzungen
console-agent-last-worked = Zuletzt gearbeitet { $when }

## What the app is called in a window and in the launcher
console-app-title = Konsole
