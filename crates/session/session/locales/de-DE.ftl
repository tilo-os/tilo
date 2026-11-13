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
update-state-unreadable-body = Dieser Computer konnte nicht lesen, wie weit das letzte Update gekommen ist, und hat daher nichts unternommen. Er versucht es von selbst erneut; unter Einstellungen, Updates steht, was er weiß.

## Eine Version, die dieser Computer versucht hat, startete innerhalb ihrer
## Versuche nicht, und der Bootloader ging auf die Version zurück, die zuvor
## lief: einmal gesagt, mit dem Ort, an dem die fehlgeschlagenen Versuche
## hinterlassen haben, was sie fanden.
system-update-went-back-title = Zurück auf die vorherige Version
system-update-went-back-body = { $what } startete nach { $tries } Versuchen nicht, deshalb ist dieser Computer auf { $running } zurückgegangen. Was die fehlgeschlagenen Versuche hinterlassen haben: { $summary }.
system-update-went-back-unnamed = Die neue Version
system-update-went-back-several-tries = mehreren
system-update-went-back-runs-before = die vorherige Version

## Die Liste der eigenen Arbeitsbereiche, die dieser Computer aufbewahrt hat,
## ließ sich nicht lesen: er beginnt die Liste neu, statt nichts zu zeigen.
workspaces-app-name = Arbeitsbereiche
workspaces-unreadable-title = Deine Arbeitsbereiche ließen sich nicht lesen
workspaces-unreadable-body = Die Liste deiner eigenen Arbeitsbereiche, die dieser Computer aufbewahrt hat, ließ sich nicht lesen, deshalb beginnt er die Liste neu. Nichts wurde entfernt: deine Arbeitsbereiche sind weiterhin auf diesem Computer und auf deinen anderen Geräten.

## Der gespeicherte Schreibtisch dieses Geräts ließ sich nicht öffnen: der
## eigene Schreibtisch liegt weiterhin auf der Platte, und diese Sitzung startet
## leer, damit nichts darüber geschrieben wird.
desk-not-opened-app-name = Desktop
desk-not-opened-nothing-written = Er ist unverändert. Diese Sitzung beginnt mit einem leeren Schreibtisch und speichert nicht darüber.
desk-not-opened-newer-title = Dein Schreibtisch wurde nicht geöffnet
desk-not-opened-newer-detail = { $path } wurde von einer neueren Tilo-Version gespeichert (Layout { $found }; diese Version versteht { $understood }). Die Datei wurde nicht verändert: diese Sitzung startet mit einem leeren Schreibtisch und speichert nichts, kann sie also nicht überschreiben.
desk-not-opened-unreadable-title = Dein Schreibtisch konnte nicht gelesen werden
desk-not-opened-unreadable-detail = { $path } ist vorhanden, konnte aber nicht gelesen werden ({ $error }). Die Datei wurde nicht verändert: diese Sitzung startet mit einem leeren Schreibtisch und speichert nichts, kann sie also nicht überschreiben.

## Der Schreibtisch, den Tilo vor dieser Version führte, der nicht übernommen
## wurde: er liegt unverändert auf der Platte, und diese Sitzung behält ihren
## eigenen.

## Ein Fenster, das dieses Gerät offen ließ und dessen App nicht zurückkam.
desk-window-not-installed = { $app } ist auf diesem Rechner nicht installiert
desk-window-would-not-start = { $app } konnte nicht gestartet werden
desk-window-did-not-open = { $app } wurde gestartet, hat aber kein Fenster geöffnet
desk-window-waits-for-host = { $app } wartet darauf, dass { $host } dieses Gerät aufnimmt

## Ein Fenster, das die Aufzeichnung der Person aufbewahrt hat und dessen
## gespeicherter Zustand sich in dieser Version nicht lesen lässt: sein Tab
## sagt, warum das Fenster nicht hier ist, statt dass der Platz verschwindet.
## Die „Immer erlauben“-Antworten, die dieser Rechner für die Agenten an seiner
## Tür aufbewahrt hat und nicht lesen konnte: jeder Agent, dessen Antwort es
## war, wird erneut gefragt.
agent-bindings-app-name = Agenten
send-to-agent-unnamed = Agent
send-to-resource = Senden an
agent-bindings-lost-title = { $agent } wird erneut gefragt
agent-bindings-lost-body = Dieser Rechner konnte nicht lesen, dass du { $agent } hier arbeiten lässt, deshalb fragt er dich erneut, wenn sich { $agent } das nächste Mal verbindet. Sonst wurde nichts entfernt.
agent-bindings-unreadable-title = Deine Antworten zu Agenten ließen sich nicht lesen
agent-bindings-unreadable-body = Dieser Rechner konnte nicht lesen, welchen Agenten du hier das Arbeiten erlaubt hast, deshalb fragt er dich erneut, wenn sich der nächste Agent verbindet. Sonst wurde nichts entfernt.

## Die Registratur der Programme, die die Person heruntergeladen hat, lässt sich
## nicht lesen: keine ihrer Apps wird aufgelistet, und keine ist fort.
refused-write-app = Änderungen
refused-write-title = { $device } nimmt deine Änderung nicht an
refused-write-body = Deine Änderung bleibt auf den Geräten, die sie angenommen haben: { $reason }.

## Warum ein Gerät oder ein Arbeitsbereich eine Änderung nicht angenommen hat,
## als Fortsetzung des Satzes davor.
refusal-reason-revoked = dem Gerät wurde der Zugang entzogen
refusal-reason-expired = die Bescheinigung des Geräts ist abgelaufen
refusal-reason-not-certified = das Gerät gehört nicht dazu
refusal-reason-forbidden = das darf es nicht ändern
refusal-reason-malformed = die Änderung war nicht lesbar
refusal-reason-waiting = sie wartet auf frühere Änderungen
refusal-reason-conflict = eine andere Änderung hält ihren Platz
refusal-reason-full = dort ist kein Platz mehr
refusal-reason-other = der Grund ist nicht bekannt

# Was eine Person am geteilten Schreibtisch eines Arbeitsbereichs geändert hat,
# ist dort nicht angekommen: die Änderung bleibt auf diesem Gerät.
desk-not-shared-app = Geteilter Schreibtisch
desk-not-shared-title = { $workspace } hat deine Fensteranordnung nicht übernommen
desk-not-shared-body = Sie bleibt auf diesem Gerät: { $reason }.

# Das Gerät einer mitarbeitenden Person spricht eine andere Tilo-Protokollversion:
# die beiden arbeiten wieder zusammen, sobald eines von ihnen aktualisiert.
version-mismatch-updating-title = { $device } hat ein neueres Tilo
version-mismatch-updating-body = Dieser Rechner aktualisiert und startet neu, sobald der Bildschirm eine Weile gesperrt ist.
version-mismatch-needs-update-title = { $device } muss aktualisieren
version-mismatch-needs-update-body = Aktualisiere Tilo dort, dann arbeiten beide wieder zusammen.
version-mismatch-unknown-updating-title = Ein anderes Gerät hat ein neueres Tilo
version-mismatch-unknown-updating-body = Dieser Rechner aktualisiert und startet neu, sobald der Bildschirm eine Weile gesperrt ist.
version-mismatch-unknown-needs-update-title = Ein anderes Gerät muss aktualisieren
version-mismatch-unknown-needs-update-body = Aktualisiere Tilo dort, dann arbeiten beide wieder zusammen.

# Was jemand in der Suche, auf einer Karte oder in einer Mitteilung gewählt
# hat, ist nicht geschehen, und die Zeile ist fort: die Mitteilung sagt, was
# gefragt war und warum es nicht geschah.
action-failed-app = Tilo
action-failed-title = { $what } hat nicht funktioniert
action-failed-body = { $reason }
action-failed-that-row = Das Öffnen dieses Suchergebnisses
action-failed-that-notification = Das Ausführen dieser Mitteilung
action-failed-that-app = Das Öffnen dieser App
action-failed-no-workspace = Es ist kein Arbeitsbereich offen.
action-failed-that-settings = Das Öffnen der Einstellungen
action-failed-that-change = Das Öffnen dieser Änderung
action-failed-that-agent = Den Zugriff dieses Agenten zurückzunehmen
downloaded-apps-app-name = Heruntergeladene Apps
downloaded-apps-unreadable-title = Deine heruntergeladenen Apps ließen sich nicht lesen
downloaded-apps-unreadable-body = Dieser Rechner konnte die Liste der Programme, die du zu deinen Apps hinzugefügt hast, nicht lesen, deshalb werden sie jetzt nicht aufgelistet. Nichts wurde entfernt: sie sind weiterhin auf diesem Computer.

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
wireless-unavailable-detail = Versuch es gleich noch einmal.
wireless-search-failed = Die Suche nach kabellosen Bildschirmen ist fehlgeschlagen
wireless-search-failed-detail = Prüfe, ob WLAN an ist, und versuch es dann erneut.
wireless-connect-failed = Die Verbindung ist fehlgeschlagen
wireless-connect-failed-detail = Prüfe, ob der Bildschirm an und in der Nähe ist, und versuch es dann erneut.
wireless-lost = Die Verbindung zum Bildschirm ist abgebrochen
wireless-lost-detail = Verbinde dich erneut, wenn er wieder da ist.
wireless-stop-failed = Das Teilen ließ sich nicht beenden
wireless-stop-failed-detail = Versuch es erneut.
session-displays-next = Nächste Bildschirmanordnung
session-displays-set = Bildschirme anordnen
session-displays-open-sheet = Wählen, wo der Schreibtisch erscheint
session-displays-close-sheet = Das Bildschirmfenster schließen
session-display-builtin = Eingebauter Bildschirm
session-display-external = Externer Bildschirm
session-clipboard = Zwischenablage
session-quick-settings = Schnelleinstellungen
session-desk-styles = Schreibtischstile
session-devices = Geräte
session-devices-press = Eines deiner Geräte drücken
session-bluetooth-app = Bluetooth
session-bluetooth-moved = { $accessory } ist umgezogen
session-bluetooth-moved-body = Es funktioniert jetzt auf { $device }.
session-bluetooth-moving = { $accessory } wird verschoben
session-bluetooth-moving-body = Es wird danach auf { $device } funktionieren.
session-bluetooth-no-identity = Dieser Rechner ist keines deiner Geräte.
session-bluetooth-not-connected = Es ist hier nicht verbunden.
session-bluetooth-not-connected-there = Es konnte auf { $device } nicht verbinden.
session-bluetooth-not-paired-there = { $device } hat es nicht gekoppelt.
session-bluetooth-not-released = Dieses Gerät konnte es nicht loslassen.
session-bluetooth-cancelled = { $device } hat den Umzug aufgegeben.
session-bluetooth-stayed = { $accessory } bleibt hier
session-bluetooth-taken = { $accessory } ist jetzt hier
session-bluetooth-taken-body = { $device } hat es verschoben.
session-bluetooth-target-away = Das andere Gerät ist nicht online.
session-bluetooth-target-unknown = Das andere Gerät ist keines deiner Geräte.
session-bluetooth-timed-out = Der Umzug hat zu lange gedauert.
session-bluetooth-unknown = Das ist keine Bluetooth-Adresse.
session-notifications = Benachrichtigungszentrum
session-notifications-dismiss = Die Benachrichtigung verwerfen
session-notifications-act = Die Aktion der Benachrichtigung ausführen
session-notifications-clear = Das Benachrichtigungszentrum leeren
session-notifications-undo-clear = Die verworfenen Benachrichtigungen zurückholen
session-notifications-open = Die Benachrichtigung öffnen
## A notification another of the person's devices projected here: the app it
## came from and the device that showed it first.
notification-from-device = { $app } von { $device }
notification-from-device-only = Von { $device }
## A notification a program sent from a terminal, with no app this device
## knows behind it: the person reads where it came from, never a bare
## command name.
notification-sender-terminal = Terminal
## The person asked to report a problem and this computer has nowhere to
## send one: the report box says so where they asked, rather than opening
## on an address that would never carry it.
session-feedback-nowhere = Ein Problem melden
session-feedback-nowhere-body = Dieser Rechner ist nicht dafür eingerichtet, Berichte an Tilo zu schicken.
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
lock-screen-failed-body = Dasselbe Passwort entsperrt diesen Rechner.
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
identity-clock-wait-body = Deine Tilo-Identität ist sicher. Der Schreibtisch öffnet, sobald Datum und Uhrzeit dieses Computers stimmen.
identity-clock-wait-not-yet-valid = Deine Tilo-Identität ist sicher. Dein Zertifikat beginnt am { $valid }, die Uhr dieses Computers zeigt { $now }. Der Schreibtisch öffnet von selbst, sobald die Uhr diesen Zeitpunkt erreicht oder du Datum und Uhrzeit einstellst.
identity-clock-wait-expired = Deine Tilo-Identität ist sicher. Dein Zertifikat galt bis { $until }, die Uhr dieses Computers zeigt { $now }. Der Schreibtisch öffnet von selbst, sobald Datum und Uhrzeit richtig eingestellt sind.

## wie die Einblendung die Tastatur nennt, deren Beleuchtung sie gerade ändert.
osd-keyboard-name = Tastatur

## Was ein Einfügen kopierter Dateien in einen Ordner ergeben hat.
paste-app-name = Dateien
paste-refused-title = Nichts wurde eingefügt
paste-nothing-to-paste = Jeder Name war schon belegt, oder die Dateien liegen bereits in diesem Ordner.
paste-not-a-folder = { $path } ist kein Ordner.
paste-stopped = { $reason }
paste-failed-title = Einige Dateien wurden nicht eingefügt
paste-failed-body = { $failed } von { $total } wurden nicht eingefügt. { $name }: { $reason }
paste-undone-title = Das letzte Einfügen wurde nicht fertig
paste-undone-body = { $landed } von { $total } waren schon eingefügt. Die bleiben; der Rest liegt wieder, wo er war.
paste-undone-trouble = { $landed } von { $total } waren schon eingefügt. Die bleiben; der Rest konnte noch nicht zurückgelegt werden: { $reason }

## Was die eigenen Zeilen des Schreibtischs sagen: die Frage auf dem Avatar der
## Person, der Tray-Eintrag einer laufenden Anwendung und wie die Personenliste heißt.
session-desk-answer-ask = Antworten
session-desk-ask-later = Später fragen
session-desk-open = Öffnen
session-desk-stop-agent = Stoppen
session-people = Personen

## Wie der Sperrbildschirm heißt, solange er oben ist.
session-lock-screen = Sperrbildschirm

## Was die eigenen Geräte der Person antworteten, als eines von ihnen nach
## einem Neustart entsperrt werden wollte, wie es ihre Aktivität auflistet.
unlock-history-app = Datenträgerentsperrung
unlock-approved = Entsperrung für { $item } freigegeben
unlock-denied = Entsperrung für { $item } abgelehnt
unlock-history-unknown-device = ein Gerät, das dieser Rechner nicht benennen kann

## Was ein in einen anderen Arbeitsbereich verschobenes Fenster fragt, was es
## sagt, wenn die App dort nicht gestartet ist, und was es sagt, wenn der Eintrag,
## den das Fenster zeigt, nicht mitkommen konnte.
window-move-app-name = Arbeitsbereiche
window-move-ask-title = { $app } nach { $workspace } verschieben?
window-move-ask-what = Die App startet dort neu, mit den Berechtigungen, die dieser Arbeitsbereich ihr gibt, und öffnet, was sie hier gezeigt hat.
window-move-ask-carries = Die App startet dort neu, mit den Berechtigungen, die dieser Arbeitsbereich ihr gibt, und nimmt mit, was das Fenster zeigt, samt Änderungen. Was zurückbleibt, sagt, wohin es ging.
window-move-ask-they-see = Alle in { $workspace } sehen das Fenster und was es öffnet.
window-move-ask-you-leave = In { $workspace } läuft sie dann nicht mehr, und was dieser Arbeitsbereich führt, bleibt dort.
window-move-ask-carry = Verschieben
window-move-ask-stay = Lassen
window-move-failed-title = { $app } ist in { $workspace } nicht gestartet
window-move-failed-body = Das Fenster blieb in { $workspace }.
window-move-record-failed-title = { $app } konnte nicht nach { $workspace } mitnehmen, was es zeigt
window-move-record-failed-body = Das Fenster blieb in { $workspace }: { $reason }.
window-move-reason-busy = die App des Fensters hat es noch geöffnet
window-move-reason-record = sein Eintrag lässt sich nicht in diesen Arbeitsbereich verschieben
window-move-reason-storage = dieser Rechner konnte ihn dort nicht ablegen
window-move-reason-no-window = die App hat dort kein Fenster geöffnet
window-move-reason-no-start = die App ist dort nicht gestartet
window-move-bringing-edits = Das Fenster kommt mit dem zurück, was nach dem Verschieben geschrieben wurde.

## The person's own card in the top bar: what it offers about who they are.
profile-action-identities = Deine Identitäten
profile-action-backup = Deine Identität sichern
profile-action-pair-device = Ein weiteres Gerät koppeln
profile-action-connect-agent = Agent oder Rechner verbinden
profile-action-devices-access = Geräte und Zugriff
profile-name-not-saved = Dein Name wurde nicht gespeichert

## A capture, while the capture studio's bar is down: the card the floating
## layer and the bar's pill show, and what its controls do.
capture-recording-title = Bildschirmaufnahme
capture-recording-stop = Beenden
capture-recording-pause = Pausieren
capture-recording-resume = Fortsetzen
capture-replay-title = Wiederholungspuffer
capture-replay-save = Speichern
capture-replay-stop = Beenden
capture-shot-title = Bildschirmfoto
capture-shot-copy = Kopieren
capture-shot-open = Öffnen
capture-shot-reveal = Im Ordner zeigen
capture-shot-dismiss = Fertig
capture-failed-not-saved = Das Bildschirmfoto konnte nicht gespeichert werden
capture-failed-not-saved-permission = Tilo darf nicht in { $folder } schreiben
capture-failed-not-saved-no-room = in { $folder } ist kein Platz mehr frei
capture-failed-not-saved-read-only = { $folder } ist schreibgeschützt
capture-failed-not-saved-folder = es gibt keinen Ordner { $folder }
capture-failed-not-saved-other = das System ließ Tilo nicht in { $folder } schreiben
capture-failed-copy-instead = Stattdessen kopieren
capture-failed-dismiss = Fertig
capture-target-display = Der ganze Schreibtisch
capture-target-window = Ein Fenster
capture-target-region = Ein Bereich des Schreibtischs

# The agent the session makes in a workspace that keeps none, when a person
# asks from the canvas (shell.md §8.1).
session-agent-name = Assistent

desk-style-failed-app-name = Schreibtisch
desk-style-failed-title = Dein Schreibtischstil konnte nicht angezeigt werden
desk-style-failed-body = { $style } konnte nicht angezeigt werden, daher zeigt der Schreibtisch Streifen. Deine Wahl bleibt erhalten.

## A step an agent takes inside a conversation, asking the person before it
## runs. The sheet shows the step's draft; confirming allows this one step.
turn-question-title = Den Agenten { $action } lassen
turn-question-detail = { $action } auf { $resource }, wie gezeigt. Es läuft einmal, nur für diesen Schritt.
turn-question-detail-here = { $action }, wie gezeigt. Es läuft einmal, nur für diesen Schritt.

## Someone mentioned the person in a record of the workspace.
mention-notice-title = { $name } hat dich in ‘{ $place }’ erwähnt
mention-notice-channel = { $name } hat dich in { $channel } erwähnt
mention-notice = { $name } hat dich erwähnt
mention-notice-someone = Jemand
mention-notice-open = Öffnen

## What a call says it wants a device for, when the call asks the person
## to allow its camera, microphone or screen.
call-microphone-reason = um im Anruf gehört zu werden
call-camera-reason = um im Anruf gesehen zu werden
call-screen-reason = um diesen Bildschirm im Anruf zu teilen

## What the window names
session-resource-keyboard = Bildschirmtastatur
session-resource-diagnostics = Sitzungsdiagnose
session-resource-window-menu = Fenstermenü
session-resource-surfaces = Oberflächen
session-resource-keyboard-focus = Tastaturfokus

## Actions

so-tilo-session-desk-switch-workspace = Zum Arbeitsbereich wechseln
so-tilo-session-desk-invitations = Einladungen auflisten
so-tilo-session-desk-issue-invitation = Einladungslink erstellen
so-tilo-session-desk-show-invite = Personen einladen
so-tilo-session-desk-show-connect = Agent oder Computer verbinden
so-tilo-session-desk-show-pair-device = Gerät hinzufügen
so-tilo-session-desk-notify = Benachrichtigung senden
so-tilo-session-desk-cancel-invitation = Einladung zurückziehen
so-tilo-session-desk-show-surface = Bereich ein- oder ausblenden
so-tilo-session-desk-close-surface = Bereich schließen
so-tilo-session-desk-clear-notices = Mitteilungen löschen
so-tilo-session-desk-dismiss-notice = Mitteilung verwerfen
so-tilo-session-desk-act-on-notice = Auf Mitteilung antworten
so-tilo-session-desk-walk-switcher = Fenster wechseln
so-tilo-session-desk-select-switcher = Fenster im Umschalter wählen
so-tilo-session-desk-commit-switcher = Zum gewählten Fenster wechseln
so-tilo-session-desk-show-app = App anzeigen
so-tilo-session-desk-open-settings = Einstellungen öffnen

## Actions

so-tilo-session-clipboard-clear = Den Verlauf leeren
so-tilo-session-clipboard-copy = Text kopieren
so-tilo-session-clipboard-paste = Eine Kopie einfügen
so-tilo-session-clipboard-paste-files = Dateien einfügen
so-tilo-session-clipboard-pin = Die Kopie anheften
so-tilo-session-clipboard-remove = Eine Kopie aus dem Verlauf entfernen
so-tilo-session-devices-hand-off = Zubehör weitergeben
so-tilo-session-devices-press = Eines der Geräte drücken
so-tilo-session-displays-brightness-down = Dunkler
so-tilo-session-displays-brightness-up = Heller
so-tilo-session-displays-close-sheet = Die Anzeigeeinstellungen schließen
so-tilo-session-displays-next-mode = Nächste Anordnung
so-tilo-session-displays-open-sheet = Die Anzeigeeinstellungen öffnen
so-tilo-session-displays-set-brightness = Die Helligkeit setzen
so-tilo-session-displays-set-display-brightness = Die Helligkeit einer Anzeige setzen
so-tilo-session-displays-set-mode = Diese Anordnung verwenden
so-tilo-session-keyboard-set-visible = Die Bildschirmtastatur zeigen
so-tilo-session-keyboard-type = Text tippen
so-tilo-session-notifications-act = Eine Aktion wählen
so-tilo-session-notifications-clear = Alle löschen
so-tilo-session-notifications-dismiss = Verwerfen
so-tilo-session-notifications-open = Öffnen
so-tilo-session-notifications-set-quiet = Meldungen zurückhalten
so-tilo-session-notifications-undo-clear = Die gelöschten Benachrichtigungen zurückholen
so-tilo-session-quick-settings-hide-control = Ein Element entfernen
so-tilo-session-quick-settings-move-control = Ein Element verschieben
so-tilo-session-quick-settings-reset-controls = Das Panel zurücksetzen
so-tilo-session-quick-settings-show-control = Ein Element hinzufügen
so-tilo-session-send-to-send = Senden an
so-tilo-desktop-overlays-close = Eine Oberfläche schließen
so-tilo-desktop-overlays-open = Eine Oberfläche öffnen
so-tilo-desktop-window-menu-choose = Eine Zeile wählen
so-tilo-desktop-window-menu-close = Das Fenstermenü schließen
so-tilo-desktop-window-menu-open = Das Fenstermenü öffnen
