# Console auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-console = Konsole

d-so-tilo-console-keywords = agenten, läufe, mcp, agent verbinden, agent stoppen, agent rückgängig, freigaben, widerrufen

d-avlndwe5 = Sitzungsverweis
    .doc = Eine Agentensitzung als eigene Ressource: Öffnen zeigt ihren Verlauf in einem eigenen Fenster, aus einer Liste, der Suche, einem Link oder einer anderen App.
    .noun = { $count ->
        [one] Sitzungsverweis
       *[other] Sitzungsverweise
    }
    .untitled = Agentensitzung

d-acxv3wxd = Lauf
    .doc = Der eigene Name des Laufs im Verlauf.

d-2pulq7rs = Auftrag
    .doc = Worum die Sitzung gebeten wurde, wie der Verlauf sie zuletzt listete: so heißt ihr Fenster.

d-ggqygzx3 = Agentenkarte
    .doc = Ein Client als eigene Ressource: Öffnen zeigt seine Karte in einem eigenen Fenster.
    .noun = { $count ->
        [one] Agentenkarte
       *[other] Agentenkarten
    }

d-mpwn6fmy = Client
    .doc = Der Name des Clients, wie seine Läufe und seine Freigabe ihn nennen.

d-cbpvmmnd = Fenster
    .doc = Das Fenster: in welchem Ort es sich öffnet, und die Sitzungen und Clients, von denen es gehört hat, jede Sitzung und jeder Client eine eigene Ressource.
    .noun = { $count ->
        [one] Fenster
       *[other] Fenster
    }

d-e6xn3nds = Ort
    .doc = Der Ort, in dem sich das Fenster öffnet: `agents`, `workspaces`, `connect` oder `usage`.

d-ujzhryea = Sitzungen
    .doc = Die Sitzungen, denen das Fenster eine eigene Ressource gegeben hat: die neuesten, die der Verlauf beim Lesen listete, und jede ältere, deren Link seither gefolgt wurde.

d-hadfiy76 = Karten
    .doc = Die Clients, die hier tätig waren, jeder als eigene Ressource: seine Karte.

d-co6xee7i = Ortsname
    .w-agents = Agenten
    .w-workspaces = Arbeitsbereiche
    .w-connect = Verbinden
    .w-usage = Nutzung

d-y4vctfc6 = Erstellt von
    .w-made-by-you = Von dir erstellt
    .w-made-by = Von { $agent } erstellt

d-5c36n5a2 = Client-Konfiguration
    .w-text = {"{"}"mcpServers": {"{"}"tilo": {"{"}"type": "http", "url": "{ $endpoint }"{"}"}{"}"}{"}"}{"}"}

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

d-7f5as25r = Veröffentlichen
    .doc = Gibt jeder der neuesten Sitzungen, die der Verlauf listet, und jedem Client, der hier tätig war, eine eigene Ressource, damit jede und jeder außerhalb dieses Fensters verlinkt, gefunden und geöffnet werden kann.

d-r2aarqjt = Sitzungsverweis
    .doc = Die Ressource der Sitzung `run`, jetzt angelegt, wenn das Fenster sie noch nicht veröffentlicht hat: Jeder Weg, eine Sitzung zu öffnen, geht über sie, damit Liste, Link, Suche und eine andere App dasselbe Fenster öffnen.

d-r2aarqjt-run = Lauf

d-h5zbne7z = Agentenkarte
    .doc = Die Karte von `client`, jetzt angelegt, wenn das Fenster sie noch nicht veröffentlicht hat: Jeder Weg, die Karte eines Clients zu öffnen, geht über sie.

d-h5zbne7z-client = Client

d-wtec67nk = Agent stoppen
    .doc = Hält die arbeitende Sitzung des Clients an und behält ihre Freigabe.

d-gxzhtzhy = Agent übernehmen
    .doc = Hält die Sitzung des Clients an und öffnet, was sie zuletzt geändert hat, oder den Arbeitsbereich, in dem sie arbeitete.

d-3gwnor5z = Sitzung des Agenten rückgängig machen
    .doc = Nimmt jede Änderung der neuesten Sitzung des Clients zurück, wenn der Verlauf sagt, dass es geht.

d-bildylqf = Agent widerrufen
    .doc = Nimmt die Freigabe zurück, durch die der Client hereinkam.

d-i7fkliog = Agent weiterarbeiten lassen
    .doc = Lässt die Aufrufe eines angehaltenen Clients wieder durch.

d-3xt6mvxc = Client stoppen
    .doc = Stoppt die Arbeitssitzung des Clients und behält seine Freigabe: das eigene Stoppen der Karte, über den Client, den eine Karte, eine Palette oder ein Agent nennt.

d-3xt6mvxc-client = Client

d-xie7cmha = Client übernehmen
    .doc = Stoppt die Sitzung des Clients und öffnet, was er zuletzt geändert hat: das eigene Übernehmen der Karte, über den Client, den eine Karte, eine Palette oder ein Agent nennt.

d-xie7cmha-client = Client

d-xxhzqb7e = Client-Sitzung rückgängig machen
    .doc = Nimmt jede Änderung der neuesten Sitzung des Clients zurück: das eigene Sitzung-rückgängig der Karte, über den Client, den eine Karte, eine Palette oder ein Agent nennt.

d-xxhzqb7e-client = Client

d-tf6gn3yw = Client widerrufen
    .doc = Nimmt die Freigabe zurück, mit der der Client hereinkam: das eigene Widerrufen der Karte, über den Client, den eine Karte, eine Palette oder ein Agent nennt.

d-tf6gn3yw-client = Client

d-4r4wjpt6 = Client weiterarbeiten lassen
    .doc = Lässt die Aufrufe eines gestoppten Clients wieder durch: das eigene Weiterarbeiten lassen der Karte, über den Client, den eine Karte, eine Palette oder ein Agent nennt.

d-4r4wjpt6-client = Client

d-mgw7foer = Verbinden öffnen
    .doc = Zeigt, wie ein Agent oder ein anderer Computer sich als diese Person verbindet: das Fenster öffnet sich bei Verbinden.

d-4aroerqe = Sitzung des Agenten rückgängig machen~present

d-3tum3xzo = Sitzungszeile

d-ie4ae4zl = Rückgängig gesperrt
    .w-it-has-no-session-to-undo = Es hat keine Sitzung zum Rückgängigmachen

d-uj5omrsf = Sitzungsfenster
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

d-6spmk7tc = Client-Karte
    .w-last-worked = Zuletzt gearbeitet { $when }
    .w-no-sessions-yet = Noch keine Sitzungen
    .w-access = Zugriff
    .w-let-it-go-on = Weiterarbeiten lassen
    .w-revoke = Widerrufen
    .w-sessions = Sitzungen
    .w-copy-link = Link kopieren

d-a4focscl = Kartenbefehle
    .w-stop = Stoppen
    .w-take-over = Übernehmen
    .w-undo-session = Sitzung rückgängig machen

d-xzdc6hq3 = Client-Liste
    .w-clients = Clients
    .w-last-worked = Zuletzt gearbeitet { $when }
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
