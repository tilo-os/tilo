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

device-agents-not-accepted = Dieses Gerät nimmt keine Agenten von deinen anderen Geräten an. Schalte in seinen Einstellungen unter Geräte „Agenten von meinen anderen Geräten annehmen“ ein.
device-agent-stopped = Du hast diesen Agenten auf diesem Gerät gestoppt.

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

## Das bereitgestellte Update ließ sich nicht im laufenden Betrieb anwenden und
## wurde zurückgenommen: einmal je Update gesagt, mit dem Neustart, der es
## abschließt.
system-update-live-failed-title = Das Update braucht einen Neustart
system-update-live-failed-body = Es ließ sich nicht im laufenden Betrieb abschließen, deshalb hat sich noch nichts geändert. Starte den Computer neu, um es abzuschließen.

## Die Liste der eigenen Arbeitsbereiche, die dieser Computer aufbewahrt hat,
## ließ sich nicht lesen: er beginnt die Liste neu, statt nichts zu zeigen.
workspaces-app-name = Arbeitsbereiche
workspace-not-founded-title = { $name } ließ sich nicht im Arbeitsbereichsdienst dieses Computers speichern
workspace-not-founded-body = Der Arbeitsbereich ist vorerst nur auf diesem Computer, und diese Sitzung versucht es erneut. { $reason }

## Der gespeicherte Schreibtisch dieses Geräts ließ sich nicht öffnen: der
## eigene Schreibtisch liegt weiterhin auf der Platte, und diese Sitzung startet
## leer, damit nichts darüber geschrieben wird.

## Der eigene Datensatz der Shell, den dieses Gerät führte, konnte nicht
## geöffnet werden: die Einstellungen deines Schreibtischs liegen weiterhin
## auf der Platte, und diese Sitzung startet mit den Vorgaben der Shell, damit
## dein eigener Datensatz nie überschrieben wird.
shell-record-not-opened-app-name = Schreibtisch
shell-record-not-opened-title = Deine Schreibtisch-Einstellungen wurden nicht geöffnet
shell-record-not-opened-detail = Sie konnten diesmal nicht gelesen werden ({ $reason }). Sie wurden nicht verändert: diese Sitzung startet mit den Vorgaben der Shell und speichert nichts davon, kann deine eigenen Einstellungen also nicht überschreiben.

## Der Schreibtisch, den Tilo vor dieser Version führte, der nicht übernommen
## wurde: er liegt unverändert auf der Platte, und diese Sitzung behält ihren
## eigenen.

## Ein Fenster, das dieses Gerät offen ließ und dessen App nicht zurückkam.
shell-window-not-installed = { $app } ist auf diesem Rechner nicht installiert
shell-window-installed-nowhere = Es ist auf diesem Rechner nicht installiert
shell-window-would-not-start = { $app } konnte nicht gestartet werden
shell-window-did-not-open = { $app } wurde gestartet, hat aber kein Fenster geöffnet
shell-window-waits-for-host = { $app } wartet darauf, dass { $host } dieses Gerät aufnimmt

## Ein Fenster, das die Aufzeichnung der Person aufbewahrt hat und dessen
## gespeicherter Zustand sich in dieser Version nicht lesen lässt: sein Tab
## sagt, warum das Fenster nicht hier ist, statt dass der Platz verschwindet.
## Die „Immer erlauben“-Antworten, die dieser Rechner für die Agenten an seiner
## Tür aufbewahrt hat und nicht lesen konnte: jeder Agent, dessen Antwort es
## war, wird erneut gefragt.
agent-bindings-app-name = Agenten
send-to-agent-unnamed = Agent
send-to-resource = Senden an
send-to-refused-title = Nicht an { $to } gesendet
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
shell-not-shared-app = Geteiltes Layout
shell-not-shared-title = { $workspace } hat dein Layout nicht übernommen
# Dasselbe für einen Arbeitsbereich, den niemand benannt hat, und für einen, den dieses Gerät vor
# dem Hinweis verlassen hat: Einen Namen gibt es dann nicht.
shell-not-shared-title-unnamed = Ein Arbeitsbereich ohne Namen hat dein Layout nicht übernommen
shell-not-shared-title-gone = Ein Arbeitsbereich, in dem dieses Gerät nicht mehr ist, hat dein Layout nicht übernommen
shell-not-shared-body = Es bleibt auf diesem Gerät: { $reason }.

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
session-input-devices = Eingabegeräte
session-privacy = Privatsphäre
session-workspace-access = Zugriff im Arbeitsbereich
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
session-shell-styles = Schreibtischstile
session-style-make = Zu meinem machen
session-style-check = Ein Stilpaket prüfen
session-style-install = Ein Stilpaket installieren
session-turn-conversation = Ganze Unterhaltung
session-application-open = Die App öffnen
session-devices = Geräte
session-speakers = Lautsprecher
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
session-bluetooth-moving-already = Es wird schon verschoben.
session-bluetooth-stayed = { $accessory } bleibt hier
session-bluetooth-taken = { $accessory } ist jetzt hier
session-bluetooth-taken-body = { $device } hat es verschoben.
session-bluetooth-target-away = Das andere Gerät ist nicht online.
session-bluetooth-target-unknown = Das andere Gerät ist keines deiner Geräte.
session-bluetooth-timed-out = Der Umzug hat zu lange gedauert.
session-bluetooth-unknown = Das ist keine Bluetooth-Adresse.
session-notifications = Benachrichtigungszentrum
session-time = Uhrzeit
session-keyboard = Tastatur
session-clock = Uhr

session-nearby-send = In der Nähe senden
session-windows = Fenster
session-windows-new-strip = In einen neuen Streifen
session-windows-delete-strip = Den Streifen löschen
session-tray = Statusleiste
session-invite = Einladen
session-agents = Agenten
session-styles = Stile
session-wallpaper = Hintergrundbild
session-live = Gerade aktiv
session-folders = Ordner
session-search = Suche
session-launchpad = Willkommen
session-notice = Hinweis
session-notice-press = Die Schaltfläche des Hinweises drücken
session-notice-hide = Den Hinweis verwerfen
session-notifications-dismiss = Die Benachrichtigung verwerfen
session-notifications-act = Die Aktion der Benachrichtigung ausführen
session-notifications-clear = Das Benachrichtigungszentrum leeren
session-notifications-undo-clear = Die verworfenen Benachrichtigungen zurückholen
session-notifications-reply = In der Benachrichtigung antworten
session-notifications-open = Die Benachrichtigung öffnen
session-notifications-hand-to-agent = Die Benachrichtigung an deinen Agenten übergeben
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
session-answer-ask = Antworten
session-ask-later = Später fragen
session-tray-open = Öffnen
session-live-press = Drücken
session-live-close = Beenden
session-live-collapse = Nicht in der Leiste zeigen
session-live-expand = Als Karte zeigen
session-live-fold = Zurück in die Leiste
session-live-seek = Springen zu
session-live-move = Karte verschieben
session-agents-stop = Stoppen
session-people = Personen
## Wer hier ist: die eigenen Aktionen der Personenliste, die die Gesichter der
## Leiste und die Karte einer Person wie die jeder anderen App aufrufen.
people-show = Öffnen, wer hier ist
people-close = Schließen, wer hier ist
people-show-person = Eine Person zeigen
people-show-person-person = Die Person, wie die Liste sie nennt
people-follow = Folgen
people-follow-person = Die Person, wie die Liste sie nennt
people-unfollow = Nicht mehr folgen
people-wave = Winken
people-wave-person = Die Person, wie die Liste sie nennt
people-message = Nachricht
people-message-person = Die Person, wie die Liste sie nennt
people-go-to = Dorthin gehen, wo sie arbeiten
people-go-to-person = Die Person, wie die Liste sie nennt
people-name = Dein Name
people-name-name = Der Name, den alle hier sehen
people-doing = Was du machst
people-doing-text = Was du machst, in deinen eigenen Worten
people-availability = Wie verfügbar du bist
people-availability-availability = Wie verfügbar du bist: verfügbar, vertieft, abwesend oder inaktiv
people-pair-device = Ein Gerät hinzufügen
people-settings = Deine Einstellungen öffnen
people-settings-link = Die Einstellungsseite als `settings://devices_access`; deine eigene, wenn leer

## Wie der Sperrbildschirm heißt, solange er oben ist.
session-lock-screen = Sperrbildschirm

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
window-move-reason-record = sein Eintrag lässt sich nicht in diesen Arbeitsbereich verschieben
window-move-reason-storage = dieser Rechner konnte ihn dort nicht ablegen
window-move-reason-no-window = die App hat dort kein Fenster geöffnet
window-move-reason-no-start = die App ist dort nicht gestartet

## The person's own card in the top bar: what it offers about who they are.
profile-action-pair-device = Gerät hinzufügen
profile-action-settings = Einstellungen
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
capture-shot-mark = Markieren
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

style-failed-app-name = Shell
style-failed-title = Dein Stil konnte nicht angezeigt werden
style-failed-body = { $style } konnte nicht angezeigt werden, daher zeigt die Shell Streifen. Deine Wahl bleibt erhalten.

press-refused-app-name = Shell
press-refused-title = Diese Aktion wurde nicht ausgeführt
press-refused-body = { $reason }

## Someone mentioned the person in a record of the workspace.
mention-notice-title = { $name } hat dich in ‘{ $place }’ erwähnt
mention-notice = { $name } hat dich erwähnt
mention-notice-someone = Jemand
mention-notice-open = Öffnen

## What a call says it wants a device for, when the call asks the person
## to allow its camera, microphone or screen.
call-microphone-reason = um im Anruf gehört zu werden
call-camera-reason = um im Anruf gesehen zu werden
call-camera-test-picture = Testbild
call-screen-reason = um diesen Bildschirm im Anruf zu teilen

## What the window names
session-resource-keyboard = Bildschirmtastatur
session-resource-diagnostics = Sitzungsdiagnose
session-diagnostics-profile-start = Die Bilder der Sitzung profilieren
session-diagnostics-profile-stop = Das Profilieren der Sitzung beenden
session-diagnostics-profile-status = Was Profiler und Heap der Sitzung tun
session-resource-window-menu = Fenstermenü
## Die Zeilen des Ein/Aus-Menüs, wie Palette, Skript und Agent sie lesen.
session-power-lock = Bildschirm sperren
session-power-log-out = Abmelden
session-power-switch-user = Benutzer wechseln
session-power-suspend = Bereitschaft
session-power-restart = Neu starten
session-power-shut-down = Ausschalten
session-resource-surfaces = Oberflächen
session-resource-power-menu = Ein/Aus-Menü
session-resource-keyboard-focus = Tastaturfokus
session-resource-layout = Layout
session-resource-layout-history = Layoutverlauf
session-strip-width = Leiste, { $width } % breit
session-place = Fenster

## Was die Aktionen des Layouts sagen, wo die Palette, ein Skript und ein Agent sie lesen.

session-action-undo = Die letzte Änderung am Layout rückgängig machen
session-action-redo = Die letzte Änderung am Layout wiederholen
session-action-clear-recents = Den Verlauf der Person leeren
session-action-clear-recents-app = Die App, deren Verlauf geleert wird, nach ihrer ID
session-action-clear-recents-kind = Der Typ, dessen Verlauf geleert wird, wie seine App ihn veröffentlicht
session-action-pin = Einen Platz anheften, damit die Änderungen eines Agenten ihn in Ruhe lassen
session-action-pin-pinned = Ob er angeheftet ist
session-action-position = Einen Platz an einen Punkt der Fläche stellen, wie ein Ziehen, das ihn dort losließ
session-action-position-x = Sein x auf der Fläche
session-action-position-y = Sein y auf der Fläche
session-action-size = Einem Platz eine Größe geben, wie das Ziehen an seiner Kante
session-action-size-width = Seine Breite in Pixeln der Fläche
session-action-size-height = Seine Höhe in Pixeln der Fläche
session-action-drawn = Melden, wie hoch ein mitwachsender Platz gerade gezeichnet wird, wie seine Fläche es sieht
session-action-drawn-height = Seine gezeichnete Höhe in Pixeln der Fläche
session-action-close = Einen Platz schließen
session-action-activate = Einen Platz vor seine Leiste holen
session-action-move = Einen Platz in eine Zeile bewegen, oder als Tab in eine Leiste
session-action-move-row = Die Zeile, in die er kommt
session-action-move-strip = Die Leiste, deren Tab er wird, in der Zeile dieser Leiste
session-action-move-before = Der Platz, vor den er in dieser Leiste kommt
session-action-float = Einen Platz über die Fenster seines Arbeitsbereichs schweben lassen, wo ihn jedes Mitglied sieht
session-action-float-corner = Die Ecke der Arbeitsfläche, von der er seinen Abstand hält: oben links, oben rechts, unten rechts (die Vorgabe) oder unten links
session-action-float-dx = Sein Abstand von der Seitenkante dieser Ecke, in Pixeln der Fläche
session-action-float-dy = Sein Abstand von der oberen oder unteren Kante dieser Ecke, in Pixeln der Fläche
session-action-float-width = Seine Breite in Pixeln der Fläche
session-action-float-height = Seine Höhe in Pixeln der Fläche
session-action-snap-back = Einen schwebenden Platz zurück unter die Fenster seines Arbeitsbereichs holen
session-action-snap-back-row = Die Zeile, in die er kommt; seine eigene, wenn ausgelassen
session-action-snap-back-before = Der Platz, vor den er in dieser Zeile kommt
session-action-resize = Eine Leiste in der Breite ändern
session-action-resize-permille = Ihre Breite in Promille der Zeile
session-action-maximize-strip = Eine Leiste maximieren, sodass sie allein steht und die Shell füllt
session-action-maximize-strip-on = Ob sie die Shell füllt
session-action-show = Eine andere Ressource in einem Platz zeigen, und den Platz lassen, wo er ist
session-action-show-place = Der Platz, nach seiner Ressource
session-action-show-resource = Die Ressource, die er von nun an zeigt
session-action-show-view = Die Ansicht, durch die er sie zeigt
session-action-retry-query = Erneut suchen, was die Suche eines Platzes nennt
session-action-retry-query-place = Der Platz, nach seiner Ressource
session-action-place = Eine Ressource als eine ihrer Ansichten auf die Fläche stellen
session-action-place-workspace = Der Arbeitsbereich, auf dessen Fläche er steht, nach seiner Kennung; der vorn, wenn ausgelassen
session-action-place-row = Die Zeile, in der er steht; die vordere Zeile, wenn ausgelassen
session-action-place-strip = Die Leiste, in der er steht; eine neue, wenn ausgelassen
session-action-place-place = Die Kennung, die der neue Platz nimmt; eine neue, wenn ausgelassen
session-action-place-resource = Die Ressource, die er zeigt
session-action-place-view = Die Ansicht, durch die er sie zeigt
session-action-place-x = Sein x auf der Fläche
session-action-place-y = Sein y auf der Fläche
session-action-place-width = Seine Breite in Pixeln der Fläche
session-action-place-height = Seine Höhe in Pixeln der Fläche
session-action-place-float = Lässt ihn über den Fenstern seines Arbeitsbereichs an dieser Ecke der Arbeitsfläche schweben: oben links, oben rechts, unten rechts (die Vorgabe) oder unten links
session-action-place-dx = Der Abstand eines schwebenden Platzes von der Seitenkante dieser Ecke, in Pixeln der Fläche
session-action-place-dy = Der Abstand eines schwebenden Platzes von der oberen oder unteren Kante dieser Ecke, in Pixeln der Fläche
session-action-place-found = Was ein Text findet, auf die Fläche stellen oder als Tab in eine Leiste
session-action-place-found-text = Was zu finden und zu platzieren ist
session-action-place-found-view = Die Ansicht, durch die er es zeigt
session-action-place-found-strip = Die Leiste, der er als Tab beitritt, nach ihrer Kennung; eine eigene Leiste, wenn ausgelassen
session-action-place-found-x = Sein x auf der Fläche
session-action-place-found-y = Sein y auf der Fläche
session-action-open = Eine Ressource mit ihrer eigenen Ansicht öffnen, neben dem, worin die Anfrage steht
session-action-open-resource = Die Ressource, die er zeigt
session-action-open-view = Die Ansicht, durch die er sie zeigt
session-action-open-beside = Die Ressource, neben deren Platz er steht, nach ihrer Adresse; der Platz vorn, wenn ausgelassen
session-action-show-match = Einen Treffer der Suche in diesem Platz zeigen
session-action-show-match-resource = Die Ressource, die die Zeile nennt
session-action-retry = Erneut danach suchen
session-action-remove-row = Eine leere Leiste aus ihrem Arbeitsbereich entfernen
session-action-remove-row-row = Die Leiste, über ihre Ressource

## Actions

so-tilo-session-switch-workspace = Zum Arbeitsbereich wechseln
so-tilo-session-invite-invitations = Einladungen auflisten
so-tilo-session-invite-issue-invitation = Einladungslink erstellen
so-tilo-session-invite-show-invite = Personen einladen
so-tilo-session-invite-show-connect = Agent oder Computer verbinden
so-tilo-session-invite-show-pair-device = Gerät hinzufügen
so-tilo-session-invite-open-link = Einen Link öffnen
so-tilo-session-invite-cancel-invitation = Einladung zurückziehen
so-tilo-desktop-overlays-show-surface = Bereich ein- oder ausblenden
so-tilo-desktop-overlays-walk-switcher = Fenster wechseln
so-tilo-desktop-overlays-select-switcher = Fenster im Umschalter wählen
so-tilo-desktop-overlays-commit-switcher = Zum gewählten Fenster wechseln
so-tilo-desktop-overlays-pick-emoji = Das gewählte Emoji in das Feld schreiben, in das die Person tippt, und die Auswahl schließen.
so-tilo-desktop-overlays-close-emoji = Die Emoji-Auswahl schließen, ohne etwas zu schreiben

## Actions

so-tilo-session-clipboard-clear = Den Verlauf leeren
so-tilo-session-clipboard-copy = Text kopieren
so-tilo-session-clipboard-paste = Eine Kopie einfügen
so-tilo-session-clipboard-paste-files = Dateien einfügen
so-tilo-session-clipboard-pin = Die Kopie anheften
so-tilo-session-clipboard-remove = Eine Kopie aus dem Verlauf entfernen
so-tilo-session-devices-found = Auf diesem Gerät neu beginnen
so-tilo-session-devices-hand-off = Zubehör weitergeben
so-tilo-session-devices-log-in = Mit einem Ticket anmelden
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
so-tilo-session-notifications-notify = Benachrichtigung senden
so-tilo-session-notifications-hand-to-agent = An den Agenten geben
so-tilo-session-notifications-open-notice = Öffnen
so-tilo-session-notifications-set-quiet = Meldungen zurückhalten
so-tilo-session-notifications-undo-clear = Die gelöschten Benachrichtigungen zurückholen
so-tilo-session-quick-settings-hide-control = Ein Element entfernen
so-tilo-session-quick-settings-move-control = Ein Element verschieben
so-tilo-session-quick-settings-reset-controls = Das Panel zurücksetzen
so-tilo-session-quick-settings-show-control = Ein Element hinzufügen
so-tilo-session-send-to-send = Senden an
so-tilo-desktop-overlays-close-surface = Eine Oberfläche schließen
so-tilo-desktop-overlays-open-surface = Eine Oberfläche öffnen
so-tilo-desktop-window-menu-choose = Eine Zeile wählen
so-tilo-desktop-window-menu-close = Das Fenstermenü schließen
so-tilo-desktop-window-menu-open = Das Fenstermenü öffnen
so-tilo-desktop-power-menu-close = Das Ein/Aus-Menü schließen
so-tilo-desktop-power-menu-open = Das Ein/Aus-Menü öffnen

## Die Beschreibung eines Parameters wird als `<Label>-<Argument>` gelesen.

session-toggle-touchpad-on = Ob das Touchpad an ist; weglassen zum Umschalten
session-night-light-toggle-on = Ob das Nachtlicht an ist; weglassen zum Umschalten
session-apps-uninstall-app = Der Name der App

## Eine Datei an einen Empfänger geben, der nicht eines der eigenen Geräte ist.

airdrop-verify-title = AirDrop-Empfänger überprüfen
airdrop-verify-detail = Datei: { $name }
    Größe: { $bytes } Bytes
    Empfänger: { $endpoint }
    Den SHA-256-Zertifikatsfingerabdruck des Empfängers als 64 Hexadezimalziffern von dessen vertrauenswürdiger Konsole oder einem anderen unabhängig geprüften Weg eingeben. Dies sendet die bereitgestellte Datei einmal, nur wenn der Fingerabdruck übereinstimmt. Abbrechen, wenn der Empfänger nicht geprüft werden kann.
airdrop-verify-send-once = Einmal senden
airdrop-verify-fingerprint = Fingerabdruck des Empfängers
program-asks-gone = Das Programm, das gefragt hat, ist beendet; frag dort noch einmal
airdrop-verify-cancel = Abbrechen
airdrop-error-verification = Fingerabdruck stimmt nicht überein. Keine Datei gesendet.

so-tilo-session-notifications-reply = In einer Benachrichtigung mit Antwortfeld antworten
session-osd = Anzeige
## Banners

session-banners = Hinweise
so-tilo-session-banners-open-banner = Die Benachrichtigung öffnen
so-tilo-session-banners-dismiss = Die Benachrichtigung schließen
so-tilo-session-banners-act = Auf der Benachrichtigung wählen
so-tilo-session-banners-reply = Auf die Benachrichtigung antworten
so-tilo-session-banners-snooze = Die Benachrichtigung später erneut zeigen
so-tilo-session-banners-focus-newest = Den neuesten Hinweis fokussieren
so-tilo-session-banners-read = Ganz lesen
so-tilo-session-banners-answer-call = Den Anruf annehmen
so-tilo-session-banners-decline-call = Den Anruf ablehnen

## The Devices section's rows: what a device says of itself, and what a press on it does.
devices-row-offline = Offline
devices-row-online = Online
devices-row-nearby = In der Nähe
devices-row-play-here = { $media } hier abspielen
devices-row-wake = Aufwecken
devices-row-action-dismiss = Verwerfen
devices-row-action-send = Senden
devices-row-action-stop = Beenden
devices-row-allow-audio = Diese Tonübertragung einmal erlauben
devices-row-audio-app = Ton der fokussierten App
devices-row-audio-choose = Wähle unten ein Gerät
devices-row-decline-audio = Diese Tonübertragung ablehnen
devices-row-requested-allow-once = { $source } fragt danach
devices-row-send-here = Nimmt den Ton dieses Geräts
so-tilo-desktop-confirm-approve = Die Aktion ausführen, nach der die Frage fragt
so-tilo-desktop-confirm-cancel = Nichts ausführen und die Frage schließen
## Die Aktivität des Arbeitsbereichs, einer App als `so.tilo.activity` gereicht.


so-tilo-activity = Aktivität
so-tilo-activity-mark-viewed = Die gezeigten Änderungen auf diesem Gerät als gesehen markieren
so-tilo-activity-focused = Melden, dass die angeforderte Ansicht gezeigt wurde
so-tilo-activity-name = Einen Bereich von Änderungen benennen, umbenennen oder den Namen entfernen; ein leerer Name entfernt ihn
so-tilo-activity-split = Die benannte Arbeitssitzung, in der ein Bereich liegt, an ihrem Anfang teilen
so-tilo-activity-merge-groups = Die benannten Arbeitssitzungen, die ein Bereich umfasst, zusammenführen
so-tilo-activity-move-boundary = Die Grenze zwischen zwei Arbeitssitzungen verschieben
so-tilo-activity-undo-change = Einen Bereich von Änderungen rückgängig machen
so-tilo-activity-restore = Den Arbeitsbereich auf den Stand vor einem Bereich von Änderungen zurücksetzen
so-tilo-activity-undo-session = Die Sitzung eines Agenten, zu der ein Bereich gehört, rückgängig machen
so-tilo-activity-undo-record = Den Anteil eines Eintrags an der Sitzung eines Agenten rückgängig machen
so-tilo-activity-open-change = Öffnen, was eine Änderung berührt hat, in seiner App
so-tilo-activity-choose-source = Die Ressource, die ein Bereich geändert hat, als Morgenquelle wählen oder abwählen
so-tilo-activity-source = Einen Befehl auf einer gewählten Morgenquelle ausführen
session-this-computer = Dieser Computer

session-host-account = Hosting-Konto
