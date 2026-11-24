tilo-servers-ask-operation = Einen Server um etwas bitten
tilo-servers-set-floor-key = Rollback-Schlüssel festlegen
tilo-servers-answer-operation = Festhalten, was ein Server geantwortet hat

## Actions

tilo-servers = Server
tilo-servers-enroll-server = Server hinzufügen
tilo-servers-plan-machine = Einen Rechner planen, der noch nicht installiert ist
tilo-servers-begin-install = Der Rechner ist beigetreten und installiert
tilo-servers-answer-machine-choice = Eine Frage seines Rechners beantworten
tilo-servers-finish-install = Die Installation des Rechners ist fertig
tilo-servers-rename-server = Server umbenennen
tilo-servers-hand-planned-workspace = Einem Server einen Arbeitsbereich aus seinem Plan übergeben
tilo-servers-set-keeping = Arbeitsbereich behalten oder nicht
tilo-servers-set-appetite = Angebot festlegen
tilo-servers-set-updates = Die Updates festlegen, denen ein Server folgt
tilo-servers-set-relay-url = Sagen, unter welcher Adresse sein Paket-Relay erreichbar ist
tilo-servers-set-web = Festlegen, ob er seine Web-Oberfläche anbietet
tilo-servers-set-maintenance-window = Wartungsfenster festlegen
tilo-servers-set-unlock-policy = Entsperrung festlegen
tilo-servers-set-hostname = Hostnamen festlegen
tilo-servers-use-disk = Eine Festplatte verwenden
tilo-servers-release-disk = Eine Festplatte freigeben
tilo-servers-set-interface = Eine Netzwerkschnittstelle einrichten
tilo-servers-release-interface = Eine Netzwerkschnittstelle nicht mehr einrichten
tilo-servers-report-device-config = Festhalten, was sein Rechner angewendet hat
tilo-servers-retire-server = Server stilllegen
tilo-region = Region und Formate
tilo-region-set-formats = Datums-, Zeit- und Zahlenformate festlegen
tilo-region-choose-zone = Zeitzone wählen
tilo-region-browser-zone = Zeitzone des Browsers übernehmen
tilo-servers-ask-retirement = Einen Server bitten, sich stillzulegen
# The name a person's own workspace gets when they do not name it, read in
# their language when they found it.
workspace-default-name = Mein Arbeitsbereich

# Why a person's own model gave no answer to work done for them, said where
# they read it, with what they can do about it.
model-none-linked = Für diese Art von Arbeit ist kein Modell verknüpft. Wähle eines in den Einstellungen unter Agentenmodelle.
model-choice-unusable = { $model } von { $provider } kann nicht mehr verwendet werden. Wähle in den Einstellungen ein anderes Modell.
model-choice-gone = { $model } kann nicht mehr verwendet werden. Wähle in den Einstellungen ein anderes Modell.
model-no-key = Für { $provider } ist hier noch kein API-Schlüssel hinterlegt. Füge ihn in den Einstellungen hinzu.
model-signed-out = { $provider } braucht eine neue Anmeldung. Melde dich in den Einstellungen unter Modellanbieter erneut an.
model-key-refused = { $provider } hat deinen API-Schlüssel nicht angenommen. Prüfe den Schlüssel oder verwende einen anderen Schlüssel für { $provider }.
model-key-not-allowed = { $provider } lässt deinen API-Schlüssel das nicht tun. Prüfe bei { $provider }, was der Schlüssel darf, oder verwende einen anderen Schlüssel.
model-key-no-credit = Das Konto deines Schlüssels für { $provider } hat kein Guthaben mehr. Lade bei { $provider } Guthaben auf oder verwende einen anderen Schlüssel.
model-address-refused = { $provider } hat eine Adresse, die von hier aus nicht erreichbar ist.
model-settings-unreadable = Deine Modelleinstellungen konnten nicht gelesen werden. Versuche es gleich noch einmal.
model-cannot-take-picture = { $model } liest keine Bilder. Wähle in den Einstellungen ein Modell, das es kann.
model-workspace-refuses = Dieser Arbeitsbereich lässt seine Inhalte nicht an { $provider } gehen.
model-day-limit = { $agent } hat das heutige Limit von { $limit } aufgebraucht. Erhöhe das Limit, damit es weitergeht.
model-timed-out = Das Modell hat nicht rechtzeitig geantwortet.
model-cancelled = Der Aufruf des Modells wurde gestoppt.
model-truncated = Die Antwort des Modells wurde abgeschnitten.
model-refused = Das Modell hat diese Anfrage abgelehnt.
model-context-limit = Das Gespräch ist länger, als das Modell aufnehmen kann. Wähle in den Einstellungen unter Agentenmodelle ein Modell mit mehr Platz.
model-rate-limited = Das Modell ist gerade ausgelastet. Versuche es gleich noch einmal.
model-unsupported = Das kann das Modell nicht. Wähle in den Einstellungen unter Agentenmodelle ein anderes Modell.
model-nothing-to-work-with = Für die Agenten dieses Arbeitsbereichs ist kein Modell verknüpft. Wähle eines in den Einstellungen unter Agentenmodelle.
tilo-devices = Deine Geräte
tilo-devices-keep-device = Ein Gerät der Person behalten
tilo-devices-keep-fact = Behalten, was ein Gerät über sich sagt
tilo-devices-keep-wifi = Ein WLAN-Passwort behalten
tilo-devices-name-device = Ein Gerät umbenennen
tilo-devices-keep-agent = Den Agenten behalten, den ein Gerät ausführt
tilo-devices-revoke-device = Ein Gerät als zurückgenommen markieren
tilo-devices-mark-root-holder = Das Gerät markieren, das den Stammschlüssel hält

## Nearby Send

so-tilo-device-nearby-send-send = Dateien an ein Gerät in der Nähe senden
so-tilo-device-nearby-send-accept = Eine angebotene Datei behalten
so-tilo-device-nearby-send-decline = Eine angebotene Datei ablehnen
so-tilo-device-nearby-send-cancel = Eine Übertragung stoppen
so-tilo-device-nearby-send-retry-transfer = Eine fehlgeschlagene Übertragung erneut anbieten
so-tilo-device-nearby-send-share = Eine Ressource mit Personen teilen, über das Einladungsblatt
# What a turn says it is doing, as the person reads it on its reply: one line per step, what is
# being done and what app it is about.
agent-step-searching = Sucht im Netz nach „{ $what }“…
agent-step-reading = Liest { $what }…
agent-step-editing = Ändert { $what }…
agent-step-exploring = Sieht nach, was { $what } kann…

## Receivers that are not the person's devices

airdrop-error-unavailable = AirDrop benötigt Dateiübertragungsrechte und aktuellen Dateizugriff.
airdrop-error-source = AirDrop unterstützt eine lesbare reguläre Datei bis 16 MiB.
airdrop-error-busy = Eine AirDrop-Übertragung läuft bereits. Vor einer weiteren Übertragung abbrechen.
airdrop-error-failed = AirDrop-Übertragung fehlgeschlagen. Den Empfänger vor einem neuen Versuch erneut prüfen.
airdrop-error-declined = Die AirDrop-Übertragung wurde vor dem Senden abgebrochen.

device-agents-not-accepted = Dieses Gerät nimmt keine Agenten von deinen anderen Geräten an. Schalte „Agenten von meinen anderen Geräten annehmen“ in seinen Einstellungen unter Geräte ein.

admit-not-permitted = Du kannst niemanden in diesen Arbeitsbereich einladen. Seine Eigentümerin oder ein Administrator kann dir erlauben, Mitglieder hinzuzufügen.
admit-above-role = Du kannst Personen mit deiner eigenen Rolle oder darunter einladen, und { $role } liegt darüber.
invitation-issuer-admits-no-more = Die Person, die dich eingeladen hat, kann niemanden mehr mit dieser Rolle aufnehmen. Bitte sie um eine neue Einladung.

task-programs-resource = Aufgabenprogramme
task-programs-run = Ein Programm ausführen
task-programs-run-detail = Führt ein Programm als Schritt der Aufgabe aus und antwortet, was es gelesen oder geändert hat.
tilo-membership = Mitgliedschaft
tilo-membership-keep-role = Die Rolle eines Mitglieds festhalten
tilo-ownership = Eigentum
tilo-ownership-relabel = Den Arbeitsbereich beschriften
tilo-ownership-run-unattended = Schlüssel ungefragt Befehle auf seinem Rechner ausführen lassen oder nicht mehr
tilo-host-blocked = Gesperrte Geräte und Personen
tilo-host-blocked-block-device = Ein Gerät sperren
tilo-host-blocked-unblock-device = Ein Gerät nicht mehr sperren
tilo-host-blocked-block-user = Eine Person und alle ihre Geräte sperren
tilo-host-blocked-unblock-user = Eine Person nicht mehr sperren

## Workspace vault
passwords-workspace-vault = Arbeitsbereichstresor
passwords-workspace-vault-keep-sealed = Verschlüsselten Tresoreintrag speichern
passwords-workspace-vault-import-sealed = Verschlüsselten Tresoreintrag importieren
passwords-workspace-vault-remove-sealed = Verschlüsselten Tresoreintrag löschen
passwords-workspace-vault-keep-uses = Festhalten, wo ein Eintrag verwendet wurde
