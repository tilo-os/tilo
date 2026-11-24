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

# Wo ein Agent dieser Anmeldung auf einem anderen Gerät der Person arbeitet: $device, wie es
# genannt wurde, $devices die hier bekannten.
other-devices-none = Hier ist keines deiner anderen Geräte bekannt.
other-devices-unknown = Keines deiner Geräte hier heißt { $device }. Deine Geräte hier: { $devices }.
other-devices-name-workspace = { $device } ist hier in mehr als einem deiner Arbeitsbereiche. Nenne einen mit --workspace.

# Die eigenen Aktionen des Layouts, wie das Fensterverwaltung eines Workspace sie liest.
so-tilo-layout-read = Lies das Layout: die Workspaces, Streifen und Fenster darauf, die App und den Record, die jedes Fenster zeigt, und die anderen Workspaces, die der Aufrufer hier erreicht.
so-tilo-layout-place = Öffne ein Fenster einer App im Layout, als das des Aufrufers: neben dem vordersten Fenster, auf dem Record, das es nennt, oder dem der App eigenen. Ein Record, den ein Fenster bereits zeigt, bleibt, wo er ist.
so-tilo-layout-close = Schließe ein Fenster im Layout, überall wo es gezeigt wird. Sein Record bleibt im Workspace.

# Was der Paket-Cache eines Geräts anbietet, wie sein Daemon es veröffentlicht.
so-tilo-packages-collect = Zurückgewinnen, was nichts mehr braucht
so-tilo-packages-verify = Die installierten Apps gegen ihre Pakete prüfen
so-tilo-packages-runtime-install = Eine Laufzeit bauen und installieren
so-tilo-packages-runtime-fetch = Eine Laufzeit holen und installieren, die ein Repository anbietet
so-tilo-packages-runtime-remove = Eine Laufzeit entfernen
so-tilo-packages-runtime-pin = Eine Laufzeit behalten
so-tilo-packages-runtime-unpin = Eine Laufzeit freigeben, wenn nichts mehr auf ihr läuft
so-tilo-packages-image-add = Ein Umgebungsabbild hinzufügen
so-tilo-packages-image-remove = Ein Umgebungsabbild entfernen
so-tilo-packages-image-fetch = Ein Umgebungsabbild holen
so-tilo-packages-flatpak-remote-add = Ein Flatpak-Repository hinzufügen
so-tilo-packages-flatpak-remote-remove = Ein Flatpak-Repository entfernen
so-tilo-packages-flatpak-remote-enable = Ein Flatpak-Repository aktivieren
so-tilo-packages-flatpak-remote-disable = Ein Flatpak-Repository deaktivieren
so-tilo-packages-flatpak-flathub = Flathub hinzufügen
so-tilo-packages-flatpak = Was die Flatpak-Installation dieses Geräts enthält
so-tilo-packages-flatpak-catalogue = Die Apps, die ein Flatpak-Repository anbietet
so-tilo-packages-catalogue = Die Apps, die ein Tilo-Repository anbietet
so-tilo-packages-runtime-offer = Der Build einer Laufzeit, den ein Tilo-Repository anbietet

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

# Nearby Send between the person's devices, as the daemon carries it and their shell shows it.
nearby-send-resource = In der Nähe senden
nearby-send-offer = Eine Datei an ein Gerät senden
nearby-send-offer-detail = Bietet eine Datei, die dieses Gerät schon hält, einem anderen deiner Geräte an, das wählt, wo sie landet.
nearby-send-keep = Eine gesendete Datei behalten
nearby-send-keep-detail = Behält eine Datei, die ein anderes deiner Geräte angeboten hat, in dem Ordner, den du wählst.
nearby-send-refuse = Eine Datei ablehnen
nearby-send-refuse-detail = Lehnt eine Datei ab, die ein anderes deiner Geräte angeboten hat.
nearby-send-stop = Eine Übertragung anhalten
nearby-send-stop-detail = Hält eine Übertragung an, die hinaus- oder hereingeht.
nearby-send-again = Eine Datei erneut anbieten
nearby-send-again-detail = Bietet eine fehlgeschlagene Übertragung erneut an.
nearby-send-keep-question = „{ $name }“ von { $device } behalten?

## The actions of this device's server settings

so-tilo-device-server-set-service = Einen Dienst anbieten oder zurückziehen
so-tilo-device-server-set-work = Wessen Arbeit dieses Gerät annimmt
so-tilo-device-server-set-work-room = Einen Teil des Rechners freihalten
so-tilo-device-server-set-work-while-charging = Arbeit nur beim Laden annehmen
