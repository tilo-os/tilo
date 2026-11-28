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
tilo-servers-set-kept-bytes = Festlegen, wie viele Bytes gehaltenen Inhalts es je Arbeitsbereich behält
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

# Said in an app's window when the workspace refuses what the app wrote by itself
# (a trigger, an update of its records) because a member took back the
# permission installing it gave: it keeps nothing until that is given back.
app-records-taken-back = { $app } kann seine Einträge in diesem Arbeitsbereich nicht mehr führen und hat angehalten: Wer es installiert hat, erreicht sie nicht mehr. Installiere { $app } erneut, damit es weitermacht.
app-records-not-installed = { $app } ist in diesem Arbeitsbereich nicht installiert und kann hier keine Einträge führen. Installiere { $app } im Arbeitsbereich, um es zu nutzen.

# Said where an app opens when the workspace runs no version of it and opening it could not
# add it there: adding apps to the workspace is up to its owners.
app-open-not-added = { $app } ist noch nicht in diesem Arbeitsbereich, und nur seine Eigentümer können hier Apps hinzufügen. Bitte einen Eigentümer, { $app } hinzuzufügen.

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
tilo-devices-say-session = Sagen, wie deine Sitzung auf einem Gerät steht
tilo-devices-allow-unlock = Ein Gerät seine Festplatte entsperren lassen
tilo-devices-refuse-unlock = Einem Gerät das Entsperren seiner Festplatte verweigern
tilo-devices-wake-device = Ein Gerät aufwecken
tilo-devices-wake-sent = Sagen, dass ein Gerät das Signal zum Aufwecken eines anderen gesendet hat
tilo-devices-ask-input = Darum bitten, ein anderes Gerät mit dieser Tastatur und Maus zu bedienen
tilo-devices-grant-input = Ein anderes Gerät dieses hier bedienen lassen
tilo-devices-end-input = Aufhören, ein Gerät mit einer anderen Tastatur und Maus zu bedienen
tilo-devices-show-notice = Eine Benachrichtigung auf deinen anderen Geräten zeigen
tilo-devices-answer-notice = Eine Benachrichtigung von einem anderen Gerät beantworten
tilo-devices-retire-notice = Eine Benachrichtigung von deinen anderen Geräten entfernen
tilo-devices-offer-continuity = Die App anbieten, die dieses Gerät gerade zeigt
tilo-devices-answer-continuity = Eine von einem anderen Gerät angebotene App übernehmen oder wegwischen
tilo-devices-show-window-on-device = Ein Fenster auf einem anderen deiner Geräte zeigen
tilo-devices-woke = Sagen, dass ein Gerät wieder wach ist
tilo-devices-ask-tether = Ein Telefon bitten, seine Verbindung zu teilen
tilo-devices-answer-tether = Sagen, ob ein Telefon seine Verbindung teilt
tilo-devices-tether-joined = Sagen, ob ein Gerät dem Hotspot eines Telefons beigetreten ist
tilo-devices-end-tether = Das Teilen der Verbindung eines Telefons beenden
tilo-devices-move-accessory = Ein Bluetooth-Zubehör auf ein anderes Gerät verschieben
tilo-devices-accessory-ready = Sagen, ob ein Gerät die Kopplung eines Zubehörs hat
tilo-devices-accessory-released = Sagen, dass ein Gerät ein Zubehör freigegeben hat
tilo-devices-accessory-taken = Sagen, dass ein Gerät ein Zubehör verbunden hat
tilo-devices-give-up-move = Das Verschieben eines Zubehörs aufgeben
tilo-devices-name-device = Ein Gerät umbenennen
tilo-devices-keep-agent = Den Agenten behalten, den ein Gerät ausführt
tilo-devices-revoke-device = Ein Gerät als zurückgenommen markieren
tilo-devices-say-speaker-endpoints = Sagen, welche Ausgänge und Mikrofone ein Gerät für Lautsprechergruppen anbietet
tilo-devices-lead-speaker-group = Die Gruppe behalten, die ein Gerät leitet, mit ihren Lautsprechern und deren Rollen
tilo-devices-drop-speaker-group = Eine Gruppe vergessen, die ein Gerät geleitet hat
tilo-devices-consent-speaker-group = Sagen, was ein Gerät von der Gruppe eines anderen Geräts angenommen hat
tilo-devices-withdraw-speaker-group = Zurücknehmen, was ein Gerät von der Gruppe eines anderen Geräts angenommen hat
tilo-devices-offer-speaker-playback = Die Wiedergabe einer Gruppe einem ihrer Mitgliedsgeräte anbieten
tilo-devices-admit-speaker-playback = Die Wiedergabe einer Gruppe auf einem Mitgliedsgerät zulassen
tilo-devices-refuse-speaker-playback = Die Wiedergabe einer Gruppe auf einem Mitgliedsgerät ablehnen
tilo-devices-stop-speaker-playback = Die Wiedergabe einer Gruppe beenden
tilo-device-answers = Deine Antworten zu deinen Geräten
tilo-device-answers-answer = Behalten, was eine App auf einem Gerät nutzen darf
tilo-device-answers-take-back = Zurücknehmen, was eine App auf einem Gerät nutzen darf

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
tilo-accounts-connected = Verbundene Konten
tilo-accounts-connected-keep = Ein verbundenes Konto behalten
tilo-accounts-connected-forget = Ein verbundenes Konto vergessen

## Workspace vault
passwords-workspace-vault = Arbeitsbereichstresor
passwords-workspace-vault-keep-sealed = Verschlüsselten Tresoreintrag speichern
passwords-workspace-vault-import-sealed = Verschlüsselten Tresoreintrag importieren
passwords-workspace-vault-remove-sealed = Verschlüsselten Tresoreintrag löschen
passwords-workspace-vault-keep-uses = Festhalten, wo ein Eintrag verwendet wurde
browser-environment = Einstellungen des Arbeitsbereichsrechners


tilo-servers-floor = Rollback-Grenze
tilo-servers-floor-set-floor-key = Grenzschlüssel festlegen
tilo-servers-floor-request-floor-advance = Anheben der Grenze erbitten
tilo-servers-floor-approve-floor-advance = Anheben der Grenze erlauben
tilo-servers-floor-refuse-floor-advance = Anheben der Grenze ablehnen

# What a person's assistant tells them.
tilo-telling-tell = Der Person etwas mitteilen
tilo-telling-list = Was mitgeteilt wurde
tilo-telling-acknowledge = Ein Ding als gesehen vermerken
tilo-telling-insist = Als nicht zu verpassen markieren
tilo-telling-since-you-left = Was passiert ist, während sie weg waren
founding-create-question = Den Arbeitsbereich „{ $name }“ anlegen, um den { $device } gebeten hat?
# A workspace's governance, and the host keeping it, as they are listed.
tilo-workspace-invite = Einladen
tilo-workspace-invitation-list = Einladungen auflisten
tilo-workspace-invitation-cancel = Einladung zurückziehen
tilo-workspace-member-add = Eine Person aufnehmen
tilo-workspace-remove-member = Mitglied entfernen
tilo-workspace-role = Rolle ändern
tilo-workspace-remove = Mitglied entfernen
tilo-workspace-ban = Sperren
tilo-workspace-wait-list-list = Die Warteliste auflisten
tilo-workspace-wait-list-remove = Eine Adresse von der Warteliste nehmen
tilo-workspace-wait-list-mail = Eine Einladung an eine Adresse auf der Warteliste mailen
tilo-workspace-server-list = Die Server auflisten, die den Arbeitsbereich behalten
tilo-workspace-server-add = Einen weiteren Server den Arbeitsbereich behalten lassen
tilo-workspace-server-remove = Einen Server den Arbeitsbereich nicht mehr behalten lassen
tilo-workspace-server-move = Den Arbeitsbereich auf einen anderen Server verschieben
tilo-workspace-copies-set = Festlegen, wie viele Kopien behalten werden
tilo-workspace-content-held = Zählen, welcher Inhalt nur auf den Geräten der Mitglieder liegt
tilo-workspace-content-policy = Was Mitglieder hier veröffentlichen dürfen
tilo-workspace-content-set-policy = Festlegen, was Mitglieder hier veröffentlichen dürfen
tilo-workspace-content-refused = Der Inhalt, den dieser Server nicht mehr behält, und warum
tilo-workspace-copies-read = Die Geräte auflisten, die den Arbeitsbereich behalten
tilo-workspace-rename = Den Arbeitsbereich umbenennen
tilo-workspace-member-name = Deinen Namen festlegen
tilo-workspace-unban = Entsperren
tilo-workspace-call = Einen Anruf moderieren
tilo-workspace-publish = Öffentlich machen
tilo-workspace-unpublish = Privat machen
tilo-workspace-publication = Öffentliche Auflistung
tilo-workspace-install-app = Eine Version einer App laufen lassen
tilo-workspace-upload-app-file = Dem Arbeitsbereich das Paket einer App-Version übergeben
tilo-workspace-app-offers = Die Apps, die die Registries anbieten
tilo-workspace-trust-list = Die Attestierer auflisten, denen dieser Arbeitsbereich vertraut
tilo-workspace-trust-revoked = Die Herausgeberschlüssel auflisten, die dieser Arbeitsbereich ablehnt
tilo-workspace-trust-add = Einem Attestierer vertrauen
tilo-workspace-trust-remove = Einem Attestierer nicht mehr vertrauen
tilo-workspace-trust-revoke = Einen Herausgeber ablehnen
tilo-workspace-registry-list = Die Registries auflisten, von denen dieser Arbeitsbereich Apps bezieht
tilo-workspace-registry-add = Eine Registry hinzufügen
tilo-workspace-registry-remove = Eine Registry entfernen
tilo-workspace-registry-enable = Eine Registry lesen
tilo-workspace-registry-disable = Eine Registry nicht mehr lesen
tilo-workspace-registry-channel = Den Release-Kanal einer Registry wählen
tilo-workspace-uninstall-app = Alle ihre eigene Version einer App laufen lassen
tilo-workspace-app-pin = Diese Version behalten
tilo-workspace-app-unpin = Die App aktualisierbar lassen
tilo-workspace-app-earlier = Die Versionen auflisten, die dieser Arbeitsbereich zuvor laufen ließ
tilo-workspace-key-revoke = Einen Schlüssel zurücknehmen
tilo-workspace-webhook-list = Die Webhooks auflisten, die dieser Schlüssel registriert hat
tilo-workspace-webhook-create = Einen Webhook registrieren
tilo-workspace-webhook-delete = Einen Webhook beenden, den dieser Schlüssel registriert hat
tilo-workspace-workspace-fetch-grant = Deiner Seite das Abruf-Proxy-Token des Hosts geben
tilo-workspace-grant-revoke = Eine Berechtigung zurücknehmen
tilo-workspace-reading-grant = Lesen erlauben
tilo-workspace-reading-revoke = Nicht mehr lesen
tilo-workspace-server-rename = Einen Server umbenennen
tilo-workspace-server-keep = Wählen, ob ein Server einen Arbeitsbereich behält
tilo-workspace-server-retire = Einen Server außer Betrieb nehmen
tilo-host-workspace-create = Einen Arbeitsbereich erstellen
tilo-host-workspace-create-own = Deinen eigenen Arbeitsbereich erstellen
tilo-host-workspace-delete = Deinen Arbeitsbereich löschen
tilo-host-workspace-branch-own = Deinen Arbeitsbereich abzweigen
tilo-host-workspace-list = Arbeitsbereiche auflisten
tilo-host-identity-revocations = Zurückgenommene Geräte auflisten
tilo-host-host-block = Ein Gerät oder eine Person auf diesem Host sperren
tilo-host-host-unblock = Ein Gerät oder eine Person auf diesem Host nicht mehr sperren
tilo-host-workspace-enter = Einem Arbeitsbereich beitreten
tilo-host-workspace-host = Einen Arbeitsbereich behalten, den ein anderer Host übergibt
tilo-host-workspace-unhost = Einen Arbeitsbereich nicht mehr behalten, den ein anderer Host übergab
tilo-host-workspace-host-frames = Die Frames eines Arbeitsbereichs von einem Gerät annehmen, das ihn behält
tilo-host-machine-plan = Einen Rechner planen
tilo-host-machine-list = Die Rechner auflisten, die du geplant hast
tilo-host-machine-show = Einen Rechner zeigen, den du geplant hast
tilo-host-machine-servers = Deine Server auflisten
tilo-host-machine-admit = Einen geplanten Rechner aufnehmen
tilo-host-server-admit = Einen Server in deine Server aufnehmen
tilo-host-machine-keep-recovery-key = Den Wiederherstellungsschlüssel der Festplatte dieses Rechners behalten
tilo-host-machine-recovery-keys = Die Wiederherstellungsschlüssel der Festplatten deiner Rechner lesen
tilo-host-web-serve = Die Web-Oberfläche dieses Servers anbieten
# What a workspace's agents learned about its people, as a turn's tools name them.
tilo-learned-resource = Was gelernt wurde
tilo-learned-list = Was gelernt wurde
tilo-learned-learn = Einen Fakt behalten
tilo-learned-correct = Einen Fakt berichtigen
tilo-learned-forget = Einen Fakt vergessen
