# Console auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-console = Konsole

d-so-tilo-console-keywords = agenten, läufe, mcp, agent verbinden, agent stoppen, agent rückgängig, freigaben, widerrufen

d-cbpvmmnd = Fenster
    .doc = Das Fenster: in welchem Ort es sich öffnet.
    .noun = { $count ->
        [one] Fenster
       *[other] Fenster
    }

d-e6xn3nds = Ort
    .doc = Der Ort, in dem sich das Fenster öffnet: `agents`, `workspaces`, `connect` oder `usage`.

d-co6xee7i = Ortsname
    .w-agents = Agenten
    .w-workspaces = Arbeitsbereiche
    .w-connect = Verbinden
    .w-usage = Nutzung

d-y4vctfc6 = Erstellt von
    .w-made-by-you = Von dir erstellt
    .w-made-by = Von { $agent } erstellt

d-a4jk4ymn = Reichweite
    .w-only = Nur { $name }
    .w-only-one-workspace = Nur ein Arbeitsbereich
    .w-every-workspace-and-may-make-new = Jeder Arbeitsbereich, und darf neue anlegen

d-vwss2xgt = Freigabestatus
    .w-working-now = Arbeitet gerade
    .w-sessions-working-now = { $working } Sitzungen arbeiten gerade
    .w-last-active = Zuletzt aktiv { $when }
    .w-let-in = Zugelassen { $when }

d-z3srezcw = Aktuelles Tun
    .w-running = Läuft { $now }…
    .w-editing = Bearbeitet { $now }
    .w-on = Auf { $now }
    .w-ran = Führte { $now } aus

d-rkm2dwcu = Gestartet von
    .w-started-by-you = Von dir gestartet
    .w-started-by = Von { $name } gestartet

d-fkxqnf4v = Aktionstitel
    .w-1-action = 1 Aktion
    .w-actions = { $count } Aktionen

d-i3edslph = Umfang
    .w-1-change = 1 Änderung
    .w-changes = { $changes } Änderungen

d-4h4nigsj = Konsole
    .w-console = Konsole
    .w-agents = Agenten
    .w-workspaces = Arbeitsbereiche
    .w-connect = Verbinden
    .w-usage = Nutzung
    .w-back = Zurück
    .w-last-worked = Zuletzt gearbeitet { $when }
    .w-no-sessions-yet = Noch keine Sitzungen
    .w-access = Zugriff
    .w-let-it-go-on = Weiterarbeiten lassen
    .w-revoke = Widerrufen
    .w-sessions = Sitzungen
    .w-why = Warum
    .w-it-changed-nothing-yet = Es hat noch nichts geändert
    .w-in = in { $cwd }
    .w-its-output-has-not-reached-this = Seine Ausgabe hat dieses Gerät noch nicht erreicht.
    .w-it-printed-more-than-its-shell = Es hat mehr ausgegeben, als seine Shell behalten hat.
    .w-reading-every-line = Jede Zeile wird gelesen…
    .w-show-less = Weniger anzeigen
    .w-show-all-output = Gesamte Ausgabe anzeigen
    .w-open = Öffnen
    .w-reading-this-session = Diese Sitzung wird gelesen
    .w-this-session-is-not-here = Diese Sitzung ist nicht hier
    .w-the-history-lists-no-such-session = Der Verlauf listet keine solche Sitzung, oder sie arbeitete in einem Arbeitsbereich, in dem du nicht bist.
    .w-every-agent = Jeder Agent
    .w-every-workspace = Jeder Arbeitsbereich
    .w-show-everything = Alles anzeigen
    .w-reading-your-workspaces-history = Der Verlauf deiner Arbeitsbereiche wird gelesen
    .w-no-sessions-match = Keine Sitzung passt
    .w-no-session-of-this-agent-in = Keine Sitzung dieses Agenten in diesem Arbeitsbereich. Zeige alles an, um den Rest zu sehen.
    .w-no-agent-has-worked-here-yet = Hier hat noch kein Agent gearbeitet
    .w-connect-an-agent-and-ask-it = Verbinde einen Agenten und bitte ihn um etwas: Du kannst jeder seiner Sitzungen folgen, während er arbeitet.
    .w-working-now = Arbeitet gerade
    .w-earlier = Früher
    .w-clients = Clients
    .w-connected-no-sessions-yet = Verbunden, noch keine Sitzungen

d-d22uzx3p = Befehle
    .w-stop = Stoppen
    .w-take-over = Übernehmen
    .w-undo-session = Sitzung rückgängig machen
    .w-it-can-be-undone-here = Es kann hier rückgängig gemacht werden

d-om4pkkxd = Befehlszeilen

d-quelstem = Befehlsstatus
    .w-running = Läuft…
    .w-exited-with-after = Mit { $code } nach { $took } beendet
    .w-exited-with = Beendet mit { $code }

d-ceombaci = Diff-Zeilen

d-fmguxyaa = Verglichen
    .w-this-change-shows-no-difference-to = Diese Änderung zeigt keinen Unterschied zur Vorschau.
    .w-reading-what-changed = Das Geänderte wird gelesen

d-mgxu3hat = Arbeitsbereiche
    .w-no-workspaces-yet = Noch keine Arbeitsbereiche
    .w-make-one-in-tilo-or-ask = Lege einen in Tilo an, oder bitte einen Agenten, einen für seine Arbeit anzulegen.
    .w-workspaces = Arbeitsbereiche
    .w-open-in-tilo = In Tilo öffnen

d-i24mkgkc = Verbinden
    .w-your-agent-works-here-as-you = Dein Agent arbeitet hier als du, sobald Tilo auf seinem Computer eingerichtet ist. Eine Zeile, die du dort einfügst, richtet das ein und meldet ihn als dich an.
    .w-set-up-your-agent = Richte deinen Agenten ein
    .w-add-this-server-to-the-mcp = Füge diesen Server der MCP-Konfiguration deines Agenten hinzu. Er bittet dich einmal um Anmeldung und arbeitet dann als er selbst in deinen Arbeitsbereichen. Alles, was er tut, erscheint unter Agenten, wo du es stoppen oder rückgängig machen kannst.
    .w-connect-an-agent = Verbinde einen Agenten
    .w-server-address = Serveradresse
    .w-any-mcp-client = Jeder MCP-Client
    .w-connected-agents = Verbundene Agenten
    .w-no-agent-connected-yet = Noch kein Agent verbunden
    .w-and-operates-web-pages = { $reach_line }, und bedient Webseiten
    .w-let-it-go-on = Weiterarbeiten lassen
    .w-revoke = Widerrufen

d-bys5sbff = Nutzung
    .w-no-linux-machine-time-yet = Noch keine Linux-Maschinenzeit
    .w-an-agent-that-needs-linux-runs = Ein Agent, der Linux braucht, führt es auf einer eigenen Maschine aus, und diese Zeit wird gezählt.
