# Hinweise, die die Sitzung der Person zeigt: knapper Speicher, ankommende
# Medien-Codecs, eine beendete Sitzung oder ein angehaltener Computer, ein
# Update, das auf einen Neustart wartet, und eine Grafik, die nicht mehr
# reagiert.
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

## Der gespeicherte Schreibtisch dieses Geräts ließ sich nicht öffnen: der
## eigene Schreibtisch liegt weiterhin auf der Platte, und diese Sitzung startet
## leer, damit nichts darüber geschrieben wird.
desk-not-opened-app-name = Desktop
desk-not-opened-newer-title = Dein Schreibtisch wurde nicht geöffnet
desk-not-opened-newer-detail = { $path } wurde von einer neueren Tilo-Version gespeichert (Layout { $found }; diese Version versteht { $understood }). Die Datei wurde nicht verändert: diese Sitzung startet mit einem leeren Schreibtisch und speichert nichts, kann sie also nicht überschreiben.
desk-not-opened-unreadable-title = Dein Schreibtisch konnte nicht gelesen werden
desk-not-opened-unreadable-detail = { $path } ist vorhanden, konnte aber nicht gelesen werden ({ $error }). Die Datei wurde nicht verändert: diese Sitzung startet mit einem leeren Schreibtisch und speichert nichts, kann sie also nicht überschreiben.

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
session-param-displays-mode = Wie die Bildschirme zusammen genutzt werden: erweitern, spiegeln, eingebaut oder extern
session-clipboard = Zwischenablage
session-clipboard-paste-files = Dateien hier einfügen
session-param-paste-destination = Der Ordner, in den die kopierten Dateien eingefügt werden
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
