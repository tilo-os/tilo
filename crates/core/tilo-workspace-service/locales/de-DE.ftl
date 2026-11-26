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
app-records-taken-back = { $app } kann seine Einträge in diesem Arbeitsbereich nicht mehr führen und hat angehalten. Ein Mitglied hat ihm die Berechtigung entzogen; installiere { $app } erneut oder gib sie unter „Berechtigungen“ des Arbeitsbereichs zurück, damit es weitermacht.
app-records-not-installed = { $app } ist in diesem Arbeitsbereich nicht installiert und kann hier keine Einträge führen. Installiere { $app } im Arbeitsbereich, um es zu nutzen.

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
tilo-devices-woke = Sagen, dass ein Gerät wieder wach ist
tilo-devices-move-accessory = Ein Bluetooth-Zubehör auf ein anderes Gerät verschieben
tilo-devices-accessory-ready = Sagen, ob ein Gerät die Kopplung eines Zubehörs hat
tilo-devices-accessory-released = Sagen, dass ein Gerät ein Zubehör freigegeben hat
tilo-devices-accessory-taken = Sagen, dass ein Gerät ein Zubehör verbunden hat
tilo-devices-give-up-move = Das Verschieben eines Zubehörs aufgeben
tilo-devices-name-device = Ein Gerät umbenennen
tilo-devices-keep-agent = Den Agenten behalten, den ein Gerät ausführt
tilo-devices-revoke-device = Ein Gerät als zurückgenommen markieren
tilo-devices-mark-root-holder = Das Gerät markieren, das den Stammschlüssel hält
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

# A workspace's model providers, kept by the host that keeps the workspace.
tilo-models-list = Modellanbieter auflisten
tilo-models-add-provider = Einen Modellanbieter hinzufügen
tilo-models-set-key = Den Schlüssel eines Anbieters aufbewahren
tilo-models-set-key-from-item = Den Schlüssel eines Anbieters aus dem Tresor übernehmen
tilo-models-remove-key = Den Schlüssel eines Anbieters entfernen
tilo-models-remove-provider = Einen Modellanbieter entfernen
tilo-models-add-preset = Einen vorbereiteten Anbieter hinzufügen
tilo-models-set-enabled = Einen Anbieter Agenten anbieten oder nicht mehr
tilo-models-refresh-models = Einen Anbieter nach seinen Modellen fragen
tilo-models-add-model = Ein Modell hinzufügen, das ein Anbieter nirgends auflistet
tilo-models-set-default = Das Modell für die Arbeit wählen
tilo-models-set-assignment = Das Modell für eine Art von Arbeit wählen
tilo-models-set-assignments = Ein Modell für mehrere Arten von Arbeit wählen
tilo-models-set-reasoning = Wählen, wie gründlich die Modelle eines Anbieters nachdenken
tilo-models-begin-sign-in = Die Anmeldung bei einem Anbieter beginnen
tilo-models-wait-sign-in = Auf die Anmeldung bei einem Anbieter warten
tilo-models-cancel-sign-in = Nicht mehr auf die Anmeldung bei einem Anbieter warten
tilo-models-search-hub-models = Hugging Face nach Modellen für diesen Computer durchsuchen
tilo-models-show-hub-model = Ein Hugging-Face-Modell für diesen Computer beschreiben
tilo-models-install-hub-model = Ein Hugging-Face-Modell auf diesen Computer laden
tilo-models-link-this-computer = Den Arbeitsbereich mit dem Modell dieses Computers betreiben
tilo-models-ask = Das Modell des Arbeitsbereichs fragen

# A workspace's connected accounts, kept by the host that keeps the workspace.
tilo-accounts-list = E-Mail-Konten auflisten
tilo-accounts-watch = Den E-Mail-Konten folgen
tilo-accounts-add-mail = Ein E-Mail-Konto hinzufügen
tilo-accounts-add-mail-with-item = Ein E-Mail-Konto aus dem Tresor hinzufügen
tilo-accounts-add-calendar = Einen Kalenderserver verbinden
tilo-accounts-reconnect = Ein Konto erneut anmelden
tilo-accounts-check = Den Server eines Kontos prüfen
tilo-accounts-replace-password = Die Anmeldung eines Kontos ersetzen
tilo-accounts-remove = Ein Konto entfernen
tilo-accounts-discover-calendars = Die Kalender eines Kontos finden
tilo-accounts-set-calendars = Die Kalender eines Kontos auswählen
tilo-accounts-find-servers = Herausfinden, wo die E-Mails einer Adresse liegen
tilo-accounts-begin-sign-in = Auf der Seite des Anbieters bei einem Konto anmelden
tilo-accounts-cancel-sign-in = Nicht mehr auf eine Anmeldung warten
tilo-accounts-set-services = Die Dienste eines Kontos auswählen
tilo-accounts-sign-ins = Die Anmeldungen im Tresor lesen
tilo-accounts-set-files-mode = Festlegen, wie die Dateien eines Kontos behalten werden
tilo-accounts-cloud-accounts = Die Konten einer Cloud-Einbindung lesen
tilo-accounts-cloud-credential = Eine Cloud-Einbindung anmelden
tilo-accounts-report-sync = Melden, wie die Synchronisierung einer Cloud-Einbindung lief
tilo-accounts-sync-now = Ein Konto jetzt synchronisieren
tilo-accounts-pause-sync = Die Synchronisierung eines Kontos pausieren
tilo-accounts-resume-sync = Die Synchronisierung eines Kontos fortsetzen
tilo-accounts-begin-cloud-sign-in = Cloud-Speicher verbinden
tilo-accounts-submit-sign-in-code = Die Anmeldung mit einem Code abschließen
tilo-accounts-connect-account = Ein Konto beim Aussteller einer App verbinden
tilo-accounts-finish-connect-account = Auf ein verbundenes Konto warten
tilo-accounts-forget-connected-account = Ein verbundenes Konto vergessen

# A person's connected services, kept by the host that keeps their workspace.
tilo-connections-list = Verbindungen auflisten
tilo-connections-look = Einen Dienst ansehen
tilo-connections-add = Den Dienst unter einer Adresse verbinden
tilo-connections-remove = Eine Verbindung entfernen
tilo-connections-refresh = Die Befehle einer Verbindung erneut lesen
tilo-connections-call = Einen Befehl aufrufen
tilo-connections-uses = Wofür eine Verbindung genutzt wurde
tilo-connections-begin-sign-in = Die Anmeldung bei einer Verbindung beginnen
tilo-connections-finish-sign-in = Die Anmeldung bei einer Verbindung abschließen
tilo-connections-propose = Eine Verbindung vorschlagen

# The workspace's vault, kept by the host that keeps the workspace.
tilo-vault-list = Tresoreinträge auflisten
tilo-vault-add = Einen Tresoreintrag sichern
tilo-vault-remove = Einen Tresoreintrag vergessen
tilo-vault-uses = Die Verwendungen eines Tresoreintrags lesen
tilo-vault-take-back = Zurücknehmen, was einen Tresoreintrag immer verwenden darf
tilo-vault-copy = Das Geheimnis eines Tresoreintrags kopieren
tilo-vault-reveal = Das Geheimnis eines Tresoreintrags zeigen
tilo-vault-ask = Deine Person nach einem Geheimnis fragen
tilo-vault-fill-page = Ein Seitenfeld aus dem Tresor ausfüllen
tilo-vault-web-call = Einen Dienst mit einem Schlüssel aus dem Tresor aufrufen
tilo-vault-code = Einen Code aus dem Tresor lesen
tilo-vault-sign = Mit einem SSH-Schlüssel aus dem Tresor signieren
tilo-vault-release = Das Geheimnis eines Tresoreintrags an den Prozess einer App geben
tilo-vault-find = Den Tresoreintrag einer App finden
tilo-vault-keep = Das eigene Geheimnis einer App sichern
tilo-vault-device-key = Den Schlüssel dieses Geräts für eine App lesen
tilo-vault-page-logins = Die Anmeldungen einer Website auflisten
tilo-vault-keep-page-login = Die Anmeldung einer Seite sichern
tilo-vault-forget-page-login = Die Anmeldung einer Seite vergessen
tilo-vault-page-fill = Die Anmeldung einer Seite ausfüllen
