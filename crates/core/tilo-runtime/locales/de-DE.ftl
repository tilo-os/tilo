# Die eigenen Texte der Laufzeit: die Einwilligung für Kamera und Mikrofon.
# $device ist `camera` oder `microphone`.
device-consent-question = { $device ->
    [camera] Darf { $app } die Kamera verwenden?
   *[microphone] Darf { $app } das Mikrofon verwenden?
}
device-consent-detail = { $device ->
    [camera] Ohne deine Erlaubnis erhält { $app } nichts von der Kamera. Deine Entscheidung gilt auf diesem Gerät, bis du sie auf einem deiner Geräte zurücknimmst.
   *[microphone] Ohne deine Erlaubnis erhält { $app } nichts vom Mikrofon. Deine Entscheidung gilt auf diesem Gerät, bis du sie auf einem deiner Geräte zurücknimmst.
}
device-consent-allow = Erlauben
device-consent-deny = Nicht erlauben
agent-confirm-question = Darf dein Agent „{ $action }“ in { $app } verwenden?
agent-confirm-detail = Das erlaubt nur diese eine Aktion; dein Agent fragt beim nächsten Mal erneut.
agent-confirm-allow = Erlauben
agent-confirm-deny = Nicht erlauben
app-grant-question = Darf { $asker } „{ $action }“ in { $app } verwenden?
app-grant-detail = Das gilt ab jetzt in diesem Arbeitsbereich. Du kannst es unter Apps in Datenschutz & Sicherheit widerrufen.

# Wo ein Agent dieser Anmeldung auf einem anderen Gerät der Person arbeitet: $device, wie es
# genannt wurde, $devices die hier bekannten.
other-devices-none = Hier ist keines deiner anderen Geräte bekannt.
other-devices-unknown = Keines deiner Geräte hier heißt { $device }. Deine Geräte hier: { $devices }.
other-devices-name-workspace = { $device } ist hier in mehr als einem deiner Arbeitsbereiche. Nenne einen mit --workspace.
