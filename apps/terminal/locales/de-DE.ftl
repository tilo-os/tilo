terminal-find-scrollback = Im Terminalverlauf suchen
terminal-target-host = Deine Shell auf diesem Gerät
terminal-target-default = Standardumgebung
terminal-target-legacy = Virtuelle Maschine { $vm }
terminal-target-manage = Umgebungen verwalten…
terminal-target-choose = Wählen, wo dies läuft
terminal-start-failed-title = Die Shell konnte nicht starten
terminal-start-retry = Erneut versuchen
terminal-prompt = Befehlszeile
terminal-restart = Shell neu starten
terminal-runs-elsewhere = Die Shell dieser Sitzung läuft auf einem anderen Gerät oder für jemand anderen. „Shell neu starten“ startet hier eine.
terminal-reconnecting = Verbindung wird wiederhergestellt…
terminal-held-title = Dafür braucht es eine Linux-Maschine.
terminal-held-add-card-message = Füge eine Karte hinzu, um fortzufahren.
terminal-held-add-card = Karte hinzufügen
terminal-held-cancel = Abbrechen
terminal-reconnect-superseded = Diese Sitzung ist in einem anderen Terminal geöffnet
terminal-host-awaiting-allow = Warte darauf, dass du die eigene Shell dieses Geräts erlaubst
terminal-host-denied = Die eigene Shell dieses Geräts wurde nicht erlaubt
terminal-no-runtime-status = Hier kann keine Umgebung starten — Eingabetaste öffnet die eigene Shell dieses Geräts
terminal-no-runtime-title = Die Umgebung dieses Terminals kann auf diesem Gerät nicht starten.
terminal-no-runtime-host = Drücke die Eingabetaste, um stattdessen die eigene Shell dieses Geräts zu öffnen (Tilo fragt dich vorher),
terminal-no-runtime-picker = oder wähle oben in diesem Tab im Zielmenü einen anderen Ort.
terminal-no-runtime-install = Eine Umgebung braucht ein env-runtime-Image: aktualisiere Tilo oder installiere eines aus dem App Store.
terminal-env-title = Umgebungen
terminal-env-tools = Werkzeuge
terminal-env-no-tools = Noch keine Werkzeuge — pkg install in der Umgebung ausführen
terminal-env-grants = Ordner
terminal-env-no-grants = Noch keine Ordner freigegeben — „Ordner freigeben…“ verwenden oder perm trust-dir in der Umgebung ausführen
terminal-env-grant = Ordner freigeben…
terminal-env-grant-title = Ordner für diese Umgebung wählen
terminal-env-grant-accept = Freigeben
terminal-env-read-only = nur lesen
terminal-env-rename = Umbenennen
terminal-env-open = Hier öffnen
terminal-env-create = Erstellen
terminal-env-name-placeholder = Name einer neuen Umgebung
terminal-env-unavailable = Umgebungen werden über Tilos Shell-Laufzeit verwaltet, mit der dieses Terminal nicht verbunden ist. Öffne das Terminal auf dem Tilo-Desktop, um Umgebungen anzulegen und zu ändern.
terminal-env-unreachable = Tilos Shell-Laufzeit hat nicht geantwortet, daher können Umgebungen gerade weder aufgelistet noch angelegt werden.
terminal-setup-failed = Diese Umgebung konnte nicht eingerichtet werden, daher wurde keine Shell gestartet. Versuche es erneut oder wähle in der Fensterleiste einen anderen Ort.
terminal-editor-busy = Der Editor bestätigt noch die vorherige Eingabe. Prüfe das Feld, bevor du weiteren Text eingibst.
terminal-editor-rejected = Das Feld wurde geändert, bevor diese Eingabe eingefügt werden konnte. Prüfe das Feld, bevor du es erneut versuchst.
terminal-editor-unconfirmed = Der Editor hat diese Eingabe nicht bestätigt. Prüfe das Feld, bevor du es erneut versuchst.

# Verlauf: was die Sitzung festgehalten hat.
terminal-record-target = Sitzung verlegt
terminal-record-relay-session = Relay der Sitzung geändert
terminal-claim-pane = Ein neuer Prozess hat die Sitzung übernommen
terminal-run = Die Sitzung gebeten, { $command } auszuführen
terminal-rerun = Die Sitzung gebeten, einen Befehl erneut auszuführen
terminal-rerun-test = Die Sitzung gebeten, { $test } erneut auszuführen
terminal-stop = Den laufenden Befehl der Sitzung gestoppt
terminal-record-run = { $command } ausgeführt
terminal-remember = { $command } gemerkt
terminal-used = Die zuletzt von diesem Gerät geöffnete Umgebung gemerkt
terminal-recent-environments-devices = Die zuletzt von jedem Gerät geöffnete Umgebung gemerkt
terminal-link-in-output = Link in der sichtbaren Terminalausgabe
terminal-a11y-description = Gemeinsames Terminal. Tippen geht an die Shell auf diesem Gerät.
terminal-a11y-graphics = { $count ->
    [one] Ein Bild wird im Terminal gezeigt.
   *[other] { $count } Bilder werden im Terminal gezeigt.
}
terminal-session-ended = Sitzung beendet. Die Shell wurde beendet, Tippen erreicht sie nicht mehr.
terminal-session-exit-unknown = Sitzung beendet. Der Exit-Status der Shell war nicht lesbar.
terminal-session-input-unavailable = Sitzung beendet. Eingabe ist nicht möglich.
terminal-session-read-failed = Sitzung beendet. Das Terminal kann nichts mehr von der Shell lesen.

## Das Schließen der eigenen Bereiche des Terminals
terminal-a11y-close = Schließen

## Actions

terminal-history = Befehlsverlauf
terminal-history-remember = Befehl merken
terminal-effects-inspect-session = Sitzung prüfen
terminal-effects-read-screen = Ausgabe lesen
terminal-effects-inspect-editor = Feld prüfen
terminal-effects-replace-editor-selection = Auswahl ersetzen
terminal-effects-send = Text senden
terminal-effects-interrupt = Prozess unterbrechen
terminal-effects-clear = Bildschirm leeren
terminal-effects-restart = Shell neu starten
terminal-effects-paste = Einfügen
terminal-effects-paste-primary = Auswahl einfügen
terminal-effects-open-in = Öffnen in…
terminal-effects-create-environment = Neue Umgebung
terminal-effects-grant-folder = Ordner freigeben
terminal-effects-rename-environment = Umgebung umbenennen
terminal-effects-copy-visible = Sichtbare Ausgabe kopieren
terminal-effects-copy-selection = Ausgewählte Ausgabe kopieren
terminal-effects-select-all = Gesamte Ausgabe auswählen
terminal-effects-history-top = Älteste Ausgabe anzeigen
terminal-effects-history-live = Zurück zur laufenden Ausgabe
terminal-effects-open-item = Eintrag anzeigen
terminal-effects-open-link = Link öffnen
terminal-session = Terminal
terminal-session-run = Befehl ausführen
terminal-session-rerun = Erneut ausführen
terminal-session-rerun-test = Test erneut ausführen
terminal-session-stop = Befehl stoppen
terminal-session-record-run = Lauf erfassen
terminal-session-record-target = Ausführungsort erfassen
terminal-session-record-relay-session = Wieder verbundene Sitzung erfassen
terminal-session-claim-pane = Bereich übernehmen
terminal-recent-environments = Zuletzt verwendete Umgebungen
terminal-recent-environments-used = Verwendete Umgebung erfassen
