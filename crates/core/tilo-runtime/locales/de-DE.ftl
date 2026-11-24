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

# Die eigenen Aktionen des Layouts, wie das Fensterverwaltung eines Workspace sie liest.
so-tilo-layout-read = Lies das Layout: die Workspaces, Streifen und Fenster darauf, die App und den Record, die jedes Fenster zeigt, und die anderen Workspaces, die der Aufrufer hier erreicht.
so-tilo-layout-place = Öffne ein Fenster einer App im Layout, als das des Aufrufers: neben dem vordersten Fenster, auf dem Record, das es nennt, oder dem der App eigenen. Ein Record, den ein Fenster bereits zeigt, bleibt, wo er ist.
so-tilo-layout-close = Schließe ein Fenster im Layout, überall wo es gezeigt wird. Sein Record bleibt im Workspace.

agent-tools-resource = Agentenwerkzeuge
agent-tools-list = Werkzeuge auflisten
agent-tools-list-detail = Listet die Werkzeuge, mit denen ein Agent in seiner Sitzung arbeitet.
agent-tools-call = Ein Werkzeug aufrufen
agent-tools-call-detail = Führt eines der Werkzeuge des Agenten als Schritt seiner Sitzung aus und antwortet, was es gesagt hat.

window-follow-resource = Fenster
window-follow-follow = Der Ansicht von jemandem folgen
window-follow-follow-detail = Sagt den Apps eines Arbeitsbereichs auf diesem Gerät, wessen Ansicht du folgst.
window-follow-handoff = Ein Fenster verschieben
window-follow-handoff-detail = Bittet eine App auf diesem Gerät um den nächsten Schritt, ihr Fenster auf ein anderes Gerät zu verschieben.
