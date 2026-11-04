# Hinweise, die die Sitzung der Person zeigt: knapper Speicher, ankommende
# Medien-Codecs, eine beendete Sitzung oder ein angehaltener Computer, ein
# Update, das auf einen Neustart wartet, und eine Grafik, die nicht mehr
# reagiert.
battery-app-name = Energie
battery-low-title = Akku fast leer
battery-low-body = Noch { $percent } %. Lade das Gerät an, um weiterzuarbeiten.
battery-critical-title = Akku kritisch leer
battery-critical-suspend-body = Noch { $percent } %. Die Arbeit wird gesichert und das Gerät schläft ein.
battery-critical-plug-body = Noch { $percent } %. Lade das Gerät bitte jetzt an.

idle-inhibit-unnamed-app = Eine App

remote-use-hold = Fernzugriff von einem anderen deiner Geräte

storage-app-name = Speicher
storage-volume-home = deine Dateien
storage-volume-system = das System
storage-low-title = Der Speicher ist fast voll
storage-low-body = Noch { $free } frei für { $volume }. Lösche große Downloads oder öffne Einstellungen, Speicher, um Platz zu schaffen.
storage-full-title = Der Speicher ist voll
storage-full-body = Nur noch { $free } frei für { $volume }. Neue Dateien und Änderungen werden eventuell erst gespeichert, wenn wieder Platz ist. Alles andere funktioniert weiter.

graphics-app-name = Grafik
graphics-stopped-title = Grafik reagiert nicht mehr
graphics-stopped-body = Alles funktioniert weiter, ist aber möglicherweise langsamer, bis du den Computer neu startest.

codec-app-name = Video- und Audioformate
codec-hardware-ready = Hardware-H.264/HEVC ist bereit
codec-installed = Mehr Video- und Audioformate sind eingeschaltet
codec-installed-mesa-waiting = { $applies } Hardware-H.264/HEVC folgt, sobald ein passender Treiber veröffentlicht ist.
codec-failed = Mehr Video- und Audioformate konnten nicht eingeschaltet werden
codec-failed-retry = Öffne Einstellungen, Updates, um es erneut zu versuchen.
codec-failed-with-error = { $error } Öffne Einstellungen, Updates, um es erneut zu versuchen.

crash-notice-app-name = Desktop
crash-notice-title = Der Desktop wurde neu gestartet
crash-notice-froze-body = Er reagierte um { $time } nicht mehr. Ein Bericht darüber bleibt auf diesem Computer.
crash-notice-crashed-body = Er wurde um { $time } unerwartet beendet. Ein Bericht darüber bleibt auf diesem Computer.
crash-notice-stopped-title = Der Computer wurde neu gestartet
crash-notice-stopped-body = Er wurde vor diesem Start unerwartet angehalten. Ein Bericht darüber bleibt auf diesem Computer.

system-update-app-name = Updates
system-update-restart-title = Neu starten, um die Updates abzuschließen
system-update-restart-body = Deine Fenster sind nach dem Neustart wieder da.
power-restart = Neu starten

## Was dieser Computer vom eigenen Update nicht lesen konnte und deshalb nicht
## ausgeführt hat. Der nächste Blick liest es erneut.
update-state-unreadable-title = Der Update-Stand ließ sich nicht lesen
update-state-unreadable-body = Dieser Computer konnte nicht lesen, wie weit das letzte Update gekommen ist, und hat daher nichts unternommen. Er schaut von selbst wieder nach; unter Einstellungen, Updates steht, was er weiß.

## Die Liste der eigenen Arbeitsbereiche, die dieser Computer aufbewahrt hat,
## ließ sich nicht lesen: er beginnt die Liste neu, statt nichts zu zeigen.
workspaces-app-name = Arbeitsbereiche
workspaces-unreadable-title = Deine Arbeitsbereiche ließen sich nicht lesen
workspaces-unreadable-body = Die Liste deiner eigenen Arbeitsbereiche, die dieser Computer aufbewahrt hat, ließ sich nicht lesen, deshalb beginnt er die Liste neu. Nichts wurde entfernt: deine Arbeitsbereiche sind weiterhin auf diesem Computer und auf deinen anderen Geräten.

## Der gespeicherte Schreibtisch dieses Geräts ließ sich nicht öffnen: der
## eigene Schreibtisch liegt weiterhin auf der Platte, und diese Sitzung startet
## leer, damit nichts darüber geschrieben wird.
desk-not-opened-app-name = Desktop
desk-not-opened-newer-title = Dein Schreibtisch wurde nicht geöffnet
desk-not-opened-newer-detail = { $path } wurde von einer neueren Tilo-Version gespeichert (Layout { $found }; diese Version versteht { $understood }). Die Datei wurde nicht verändert: diese Sitzung startet mit einem leeren Schreibtisch und speichert nichts, kann sie also nicht überschreiben.
desk-not-opened-unreadable-title = Dein Schreibtisch konnte nicht gelesen werden
desk-not-opened-unreadable-detail = { $path } ist vorhanden, konnte aber nicht gelesen werden ({ $error }). Die Datei wurde nicht verändert: diese Sitzung startet mit einem leeren Schreibtisch und speichert nichts, kann sie also nicht überschreiben.

## Der Schreibtisch, den Tilo vor dieser Version führte, der nicht übernommen
## wurde: er liegt unverändert auf der Platte, und diese Sitzung behält ihren
## eigenen.
desk-not-opened-old-newer-title = Dein alter Schreibtisch wurde nicht übernommen
desk-not-opened-old-newer-detail = { $path } ist der Schreibtisch, den Tilo vor dieser Version führte, gespeichert von einer neueren (Layout { $found }; diese Version versteht { $understood }). Er wurde nicht verändert und liegt weiterhin auf diesem Rechner: diese Sitzung startet mit einem leeren Schreibtisch.
desk-not-opened-old-unreadable-title = Dein alter Schreibtisch konnte nicht gelesen werden
desk-not-opened-old-unreadable-detail = { $path } ist der Schreibtisch, den Tilo vor dieser Version führte, und er konnte nicht gelesen werden ({ $error }). Er wurde nicht verändert und liegt weiterhin auf diesem Rechner: diese Sitzung startet mit einem leeren Schreibtisch.

## Ein Fenster, das dieses Gerät offen ließ und dessen App nicht zurückkam.
desk-window-not-installed = { $app } ist auf diesem Rechner nicht installiert
desk-window-would-not-start = { $app } konnte nicht gestartet werden: { $why }
desk-window-did-not-open = { $app } wurde gestartet, hat aber kein Fenster geöffnet

## Ein Anzeigewerkzeug wollte eine Anzeigeanordnung erneut anwenden, die die
## Person gerade zurückgesetzt hatte.
display-tool-reapply-refused = { $tool } wollte eine Anzeigeanordnung erneut anwenden, die du zurückgesetzt hast
display-tool-reapply-refused-unknown = Ein Anzeigewerkzeug wollte eine Anzeigeanordnung erneut anwenden, die du zurückgesetzt hast

## Die eigenen Ressourcen der Sitzung: die Eingabegeräte, der Bildschirm, das
## Nachtlicht, die Lupe, die Bildschirme, die Zwischenablage und die Fragen zu
## den Apps der Person.
session-input = Eingabegeräte
session-toggle-touchpad = Touchpad ein- oder ausschalten
session-param-touchpad-on = Ob das Touchpad an ist; weglassen zum Umschalten
session-next-input-source = In der nächsten Tastatureingabequelle tippen
session-screen = Bildschirm
session-lock = Bildschirm sperren
session-log-out = Abmelden
session-power-menu = Das Energiemenü anzeigen
session-night-light = Nachtlicht
session-night-light-toggle = Nachtlicht ein- oder ausschalten
session-param-night-light-on = Ob das Nachtlicht an ist; weglassen zum Umschalten
session-magnifier = Lupe
session-magnifier-toggle = Lupe ein- oder ausschalten
session-magnifier-zoom-in = Stärker vergrößern
session-magnifier-zoom-out = Weniger vergrößern
session-displays = Bildschirme
wireless-unavailable = Kabellose Bildschirme sind gerade nicht verfügbar
wireless-unavailable-detail = Versuchen Sie es gleich noch einmal.
wireless-search-failed = Die Suche nach kabellosen Bildschirmen ist fehlgeschlagen
wireless-search-failed-detail = Prüfen Sie, ob WLAN an ist, und versuchen Sie es dann erneut.
wireless-connect-failed = Die Verbindung ist fehlgeschlagen
wireless-connect-failed-detail = Prüfen Sie, ob der Bildschirm an und in der Nähe ist, und versuchen Sie es dann erneut.
wireless-lost = Die Verbindung zum Bildschirm ist abgebrochen
wireless-lost-detail = Verbinden Sie erneut, wenn er wieder da ist.
wireless-stop-failed = Das Teilen ließ sich nicht beenden
wireless-stop-failed-detail = Versuchen Sie es erneut.
session-displays-next = Nächste Bildschirmanordnung
session-displays-set = Bildschirme anordnen
session-displays-open-sheet = Wählen, wo der Schreibtisch erscheint
session-displays-close-sheet = Das Bildschirmfenster schließen
session-display-builtin = Eingebauter Bildschirm
session-clipboard = Zwischenablage
session-devices = Geräte
session-devices-press = Eines der Geräte der Person drücken
session-bluetooth-app = Bluetooth
session-bluetooth-moved = { $accessory } ist umgezogen
session-bluetooth-moved-body = Es funktioniert jetzt auf { $device }.
session-bluetooth-moving = { $accessory } wird verschoben
session-bluetooth-moving-body = Es wird danach auf { $device } funktionieren.
session-bluetooth-no-identity = Diese Sitzung ist keines der Geräte der Person.
session-bluetooth-not-connected = Es ist hier nicht verbunden.
session-bluetooth-not-connected-there = Es konnte auf { $device } nicht verbinden.
session-bluetooth-not-paired-there = { $device } hat seine Kopplung nicht.
session-bluetooth-not-released = Dieses Gerät konnte es nicht loslassen.
session-bluetooth-cancelled = { $device } hat den Umzug aufgegeben.
session-bluetooth-stayed = { $accessory } bleibt hier
session-bluetooth-taken = { $accessory } ist jetzt hier
session-bluetooth-taken-body = { $device } hat es verschoben.
session-bluetooth-target-away = Das andere Gerät ist nicht online.
session-bluetooth-target-unknown = Das andere Gerät gehört nicht zu den Geräten der Person.
session-bluetooth-timed-out = Der Umzug hat zu lange gedauert.
session-bluetooth-unknown = Das ist keine Bluetooth-Adresse.
session-notifications = Benachrichtigungszentrum
session-notifications-dismiss = Die Benachrichtigung verwerfen
session-notifications-act = Die Aktion der Benachrichtigung ausführen
session-notifications-clear = Das Benachrichtigungszentrum leeren
session-notifications-undo-clear = Die verworfenen Benachrichtigungen zurückholen
session-notifications-open = Die Benachrichtigung öffnen
session-notifications-hand-to-agent = Die Benachrichtigung an einen Agenten übergeben
session-apps = Apps
session-apps-uninstall = Eine App deinstallieren
session-param-app = Der Name der App

## A timer Clocks ran in the background has come due.
clock-due-title = Uhren
clock-due-body = Die Zeit ist abgelaufen

## Ein Laufwerk wurde eingesteckt und lässt sich öffnen oder auswerfen.
drives-app-name = Laufwerke
drives-medium-title = { $name } ({ $size })
drives-unnamed = Wechseldatenträger
drives-body = Eingesteckt. Öffne es, um zu sehen, was darauf ist, oder wirf es aus, bevor du es abziehst.
drives-open = Öffnen
drives-eject = Auswerfen

## Was beim Deinstallieren einer App oder beim Entfernen aus den Apps herauskam.
app-list-app-name = Apps
app-list-gone-title = App nicht gefunden
app-list-gone-body = Sie ist nicht mehr unter deinen Apps.

lock-screen-failed-title = Sperrbildschirm startete nicht
lock-screen-failed-body = Tilo zeichnet die Sperre selbst: dasselbe Passwort entsperrt diesen Rechner.
app-list-uninstalled = { $name } deinstalliert
app-list-uninstalled-file = Ihre Datei liegt im Papierkorb.
app-list-uninstalled-files = Alle ihre Kopien liegen im Papierkorb.
app-list-removed = { $name } aus deinen Apps entfernt
app-list-add-failed = { $name } konnte nicht zu deinen Apps hinzugefügt werden
app-list-locate-failed = { $name } wurde nicht gefunden
app-list-remove-failed = { $name } konnte nicht aus deinen Apps entfernt werden
app-list-uninstall-failed = { $name } konnte nicht deinstalliert werden

## Was aus dem Aufwecken eines schlafenden Geräts wurde.
device-wake-app-name = Geräte
device-wake-rejoined = { $device } ist wach
device-wake-not-rejoined = { $device } ist nicht aufgewacht
device-wake-not-rejoined-body = Es hat ein Aufwecksignal bekommen, ist aber nicht zurück. Vielleicht ist es aus oder hat keinen Strom.
device-wake-failed = { $device } konnte nicht aufgeweckt werden
device-wake-failed-body = In seinem Netzwerk konnte kein Aufwecksignal gesendet werden.

## Was ein Screenreader über einen Zeiger sagt: wem er gehört und über
## welchem Fenster er steht.
people-pointer-where = { $who } ist in { $where }

## Was die Statusanzeige über die Maschine in einer Zeile sagt, für einen
## Reader, der sie hört statt ihre Symbole zu sehen.
status-battery = Akku { $percent } %
status-battery-charging = Akku { $percent } %, wird geladen
status-muted = Stummgeschaltet
status-offline = Offline
status-volume = Lautstärke { $percent } %
status-wifi = WLAN
status-wifi-named = WLAN: { $name }
status-wired = Kabelgebunden
status-wired-named = Kabelgebunden: { $name }

## Der Schreibtisch wartet, während das Zertifikat dieses Geräts nach der Uhr
## außerhalb seiner Gültigkeit liegt: Die Identität auf der Platte ist intakt,
## und der Bildschirm sagt, bis wann die Uhr laufen muss, bevor der
## Schreibtisch von selbst aufgeht.
identity-clock-wait-title = Warten auf die Uhr
identity-clock-wait-body = Ihre Tilo-Identität ist sicher. Der Schreibtisch öffnet, sobald Datum und Uhrzeit dieses Computers stimmen.
identity-clock-wait-not-yet-valid = Ihre Tilo-Identität ist sicher. Ihr Zertifikat beginnt am { $valid }, die Uhr dieses Computers zeigt { $now }. Der Schreibtisch öffnet von selbst, sobald die Uhr diesen Zeitpunkt erreicht oder Sie Datum und Uhrzeit einstellen.
identity-clock-wait-expired = Ihre Tilo-Identität ist sicher. Ihr Zertifikat galt bis { $until }, die Uhr dieses Computers zeigt { $now }. Der Schreibtisch öffnet von selbst, sobald Datum und Uhrzeit richtig eingestellt sind.

## wie die Einblendung die Tastatur nennt, deren Beleuchtung sie gerade ändert.
osd-keyboard-name = Tastatur

## Was ein Einfügen kopierter Dateien in einen Ordner ergeben hat.
paste-app-name = Dateien
paste-refused-title = Nichts wurde eingefügt
paste-nothing-to-paste = Jeder Name war schon belegt, oder die Dateien liegen bereits in diesem Ordner.
paste-not-a-folder = { $path } ist kein Ordner.
paste-stopped = { $reason }
paste-failed-title = Einige Dateien wurden nicht eingefügt
paste-failed-body = { $failed } von { $total } sind nicht angekommen. { $name }: { $reason }
paste-undone-title = Das letzte Einfügen wurde nicht fertig
paste-undone-body = { $landed } von { $total } waren schon angekommen. Die bleiben; der Rest liegt wieder, wo er war.
paste-undone-trouble = { $landed } von { $total } waren schon angekommen. Die bleiben; der Rest konnte noch nicht zurückgelegt werden: { $reason }

## Was die eigenen Zeilen des Schreibtischs sagen: die Frage auf dem Avatar der
## Person, der Tray-Eintrag einer laufenden Anwendung und wie die Personenliste heißt.
session-desk-answer-ask = Antworten
session-desk-ask-later = Später fragen
session-desk-open = Öffnen
session-desk-stop-agent = Stoppen
session-people = Personen

## Wie die privaten Unterhaltungen einer Person heißen und was sie anbieten.
session-messages = Private Nachrichten
session-messages-send = Private Nachricht senden

## Wie der Sperrbildschirm heißt, solange er oben ist.
session-lock-screen = Sperrbildschirm

## Was die Programme eines Agenten sind, wie sie die Aktionspforte anbietet:
## ein Aufgaben-Agent schreibt jeweils ein Programm als Schritt seines Laufs.
task-programs-resource = Aufgabenprogramme
task-programs-run = Ein Programm ausführen
task-programs-run-detail = Führt ein tilolang-Programm als Schritt des Laufs der Aufgabe aus, als der Agent, der an der Aufgabe arbeitet, und antwortet, was es gelesen oder geändert hat.
