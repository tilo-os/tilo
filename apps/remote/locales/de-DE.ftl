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
