## Verlauf der gespeicherten Verbindungen

remote-connection-name = { $item } umbenannt
remote-connection-host = { $item } auf { $new } gerichtet
remote-connection-port = Den Port von { $item } auf { $new } gesetzt
remote-connection-user = Den Benutzer von { $item } auf { $new } gesetzt
remote-connection-kind = { $item } verbindet über { $new ->
    [ssh] SSH
    [vnc] VNC
    [rdp] RDP
   *[tilo] das Gerätenetz von Tilo
}
remote-connection-auth = { $item } meldet sich an { $new ->
    [password] mit einem Passwort
    [public_key] mit einem öffentlichen Schlüssel
   *[device_identity] mit der Identität dieses Geräts
}
remote-connection-group = { $item } unter { $new } einsortiert
remote-connection-favorite = { $new ->
    [true] { $item } angeheftet
   *[false] { $item } abgeheftet
}
remote-remote-connections = { $op ->
    [insert] Eine Verbindung gespeichert
    [remove] Eine Verbindung gelöscht
   *[other] Die gespeicherten Verbindungen geändert
}
remote-add-connection = { $name } hinzugefügt
remote-delete-connection = { $item } gelöscht
remote-mark-used = Mit { $item } verbunden

## Was diese App neben dem Bearbeiten gespeicherter Verbindungen tut
remote-describe-put-file = Eine lokale Datei an diese SSH-Maschine senden: nenne den lokalen Pfad und wohin sie dort geschrieben wird.
remote-describe-fetch-file = Eine Datei von dieser SSH-Maschine holen: nenne den entfernten Pfad und wohin sie hier geschrieben wird.
remote-describe-forget-host-key = Die veraltete SSH-Hostschlüssel-Bindung dieses Geräts entfernen; neu verbinden, um einen Ersatzschlüssel zu prüfen.

## Fenster- und Barrierefreiheits-Texte
remote-a11y-window = Remote
remote-waiting-first-frame = Warte auf das erste Bild

## Fakten, Eigenschaftsnamen und Bereichstitel
remote-action-trust-host-key = { $algorithm } { $fingerprint } nur nach Prüfung über einen anderen Weg ausdrücklich für dieses Gerät vertrauen.
remote-action-open-session = Eine laufende Sitzung zu dieser gespeicherten Verbindung öffnen. { $detail }
remote-nearby-devices = Geräte in der Nähe
remote-other-connections = Andere Verbindungen
remote-pinned = Angeheftet
remote-a11y-terminal = Remote-Terminal
remote-a11y-desktop = Remote-Desktop

## What the window shows
remote-placeholder-name = Heimserver
remote-placeholder-endpoint = Kennung des Geräte-Endpunkts
remote-placeholder-host = Hostname oder IP-Adresse
remote-placeholder-port = Port
remote-placeholder-optional = Optional
remote-placeholder-user = Benutzername
remote-key-passphrase = Passphrase des Schlüssels
remote-password = Passwort
remote-delete-connection-button = Verbindung löschen
remote-disconnect-title = Sitzung zu { $connection } schließen?
remote-disconnect-detail = Die laufende Sitzung endet, und ihr Verlauf geht mit. Die gespeicherte Verbindung bleibt.
remote-disconnect-confirm = Sitzung schließen
remote-delete-title = { $connection } löschen?
remote-delete-detail = Die gespeicherte Verbindung wird für alle entfernt, mit denen du sie teilst. Der Rechner selbst bleibt unverändert.
remote-delete-confirm = Verbindung löschen
remote-connect-title = Mit { $connection } verbinden?
remote-connect-detail = Damit öffnet sich eine laufende Sitzung. Remote prüft die gespeicherte Adresse und den Zugang erneut, bevor etwas gesendet wird.
remote-connect-confirm = Verbinden
remote-trust-key-title = Dem SSH-Schlüssel von { $connection } vertrauen?
remote-trust-key-detail = Prüfe diesen Schlüssel mit der Person, der der Rechner gehört, bevor du ihm vertraust. Nur dieses Gerät merkt ihn sich.
remote-trust-key-confirm = Schlüssel vertrauen
remote-forget-key-title = Den SSH-Schlüssel von { $connection } vergessen?
remote-forget-key-detail = Nur der auf diesem Gerät gespeicherte Schlüssel wird entfernt. Verbinde dich erneut, um den neuen Schlüssel zu prüfen und ihm zu vertrauen.
remote-forget-key-confirm = Schlüssel vergessen

## Why a connection cannot open
remote-state-ready = Remote kann sich mit diesem Rechner verbinden.
remote-state-no-ssh = Remote kann auf diesem Gerät keine SSH-Sitzungen öffnen.
remote-state-no-vnc = Remote kann auf diesem Gerät keine VNC-Sitzungen öffnen.
remote-state-no-session = Remote kann diese Art von Sitzung auf diesem Gerät nicht öffnen.
remote-state-no-rdp = Remote öffnet SSH-, VNC- und Tilo-Geräte, aber kein RDP.
remote-state-invalid-profile = Die gespeicherte Adresse oder Anmeldeart passt nicht zu diesem Protokoll.
remote-connection-deleted-message = Jemand, mit dem du deine Verbindungen teilst, hat sie entfernt.
remote-empty-pinned-hint = Hefte die Maschinen an, die du am meisten nutzt, damit sie hier bleiben.
remote-empty-saved-hint = Speichere eine Maschine, um sie über SSH, VNC oder als Tilo-Gerät zu erreichen.
remote-rdp-unsupported-message = Wähle SSH oder VNC, um diese Maschine weiter zu nutzen.

## Die Plätze der Fensterleiste
remote-pinned-connections = Angeheftete Verbindungen
remote-connections = Verbindungen
remote-new-connection = Neue Verbindung
remote-edit-connection = Verbindung bearbeiten
remote-save = Speichern
remote-actual-size = Originalgröße
remote-fit-to-window = An Fenster anpassen
remote-full-screen = Vollbild
remote-disconnect = Trennen

## Actions

remote-effect-show-connections = Verbindungen anzeigen
remote-effect-edit-connection = Verbindungseinstellungen
remote-effect-send-text = Text senden
remote-effect-put-file = Datei senden
remote-effect-fetch-file = Datei holen
remote-effect-trust-host-key = Hostschlüssel vertrauen
remote-effect-forget-host-key = Hostschlüssel vergessen
remote-connection-list = Fernverbindungen
remote-connection-list-add-connection = Verbindung hinzufügen
remote-connection-list-mark-used = Verwendung vermerken

## What a connection is called
remote-kind-ssh = SSH
remote-kind-vnc = VNC
remote-kind-rdp = RDP
remote-kind-tilo = Tilo-Gerät
remote-auth-password = Passwort
remote-auth-public-key = Öffentlicher Schlüssel
remote-auth-device-identity = Geräteidentität
remote-readiness-ready = Bereit
remote-readiness-elsewhere = Hier nicht verfügbar
remote-readiness-no-rdp = RDP wird nicht unterstützt
remote-readiness-editing = Muss angepasst werden
remote-issue-ssh-auth = SSH-Profile unterstützen Authentifizierung per Passwort oder öffentlichem Schlüssel.
remote-issue-vnc-auth = VNC-Profile unterstützen nur Authentifizierung per Passwort.
remote-issue-rdp-auth = RDP-Profile unterstützen nur Authentifizierung per Passwort.
remote-issue-tilo-auth = Tilo-Geräteprofile müssen die Geräteidentität verwenden.
remote-issue-tilo-endpoint = Der Knoten eines Tilo-Geräts muss die exakte Endpunkt-ID des Geräts sein.
remote-rdp-unsupported-title = Remote kann keine RDP-Verbindungen öffnen

## What the app is called in a window and in the launcher
remote-app-title = Remotedesktop
