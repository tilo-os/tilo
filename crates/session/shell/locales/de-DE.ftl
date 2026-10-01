# Shell — deutsche Übersetzung (de-DE).
# Schlüssel und Variablen müssen mit en-US.ftl übereinstimmen.

# Auf diesem Desktop fortsetzen, was du gerade auf einem anderen Gerät getan hast.
continuity-continue = { $app } von { $device } fortsetzen
continuity-continue-subtitle = Den Tab fokussieren, den du gerade auf { $device } verwendet hast
continuity-bringing-record = Sein Dokument wird von einem anderen Gerät geholt …
record-not-kept-here = Dieses Dokument wird auf diesem Gerät nicht aufbewahrt.
record-too-large-to-bring = Dieses Dokument ist zu groß, um es herüberzuholen.
record-offset-past-end = Diesen Teil des Dokuments gibt es nicht.
continuity-dismiss-title = „{ $app } von { $device } fortsetzen“ ausblenden
continuity-dismiss-subtitle = Ausgeblendet, bis du auf { $device } wieder den Tab wechselst
continuity-other-device = deinem anderen Gerät

# Schließen eines Tabs rückgängig machen.
tab-closed-toast = { $title } geschlossen
tab-closed-group-toast = { $count } Tabs geschlossen
tab-closed-undo = Rückgängig
tab-closed-undo-hint = Zum Rückgängigmachen auswählen oder später { $chord } drücken
tab-closed-undo-hint-unbound = Zum Rückgängigmachen auswählen

# Anzeigeanordnung (Super+P)
display-sheet-title = Anzeige
display-sheet-subtitle = Wähle, wo dein Desktop erscheint. Super+P wechselt zur nächsten Option.
display-sheet-a11y-description = Pfeil nach oben und unten wechseln zwischen den Optionen, Eingabe übernimmt die markierte, Super+P wechselt zur nächsten Anordnung, Escape schließt.
display-mode-extend = Erweitern
display-mode-extend-detail = Ein Desktop über alle Bildschirme
display-mode-mirror = Spiegeln
display-mode-mirror-detail = Dasselbe Bild auf jedem Bildschirm
display-mode-builtin-only = Nur dieser Bildschirm
display-mode-builtin-only-detail = Externe Bildschirme werden ausgeschaltet
display-mode-external-only = Nur extern
display-mode-external-only-detail = Der eingebaute Bildschirm wird ausgeschaltet
display-mode-current = Aktuell
display-mode-unavailable-single = Schließe einen weiteren Bildschirm an, um dies zu nutzen
display-mode-unavailable-builtin = Dieses Gerät hat keinen eingebauten Bildschirm
display-mode-unavailable-mirror = Diese Bildschirme haben keine gemeinsame Auflösung
display-confirm-title = Diese Anzeigeeinstellungen behalten?
display-confirm-countdown = Wird in { $seconds } s zurückgesetzt
display-confirm-applying = Wird angewendet …
display-confirm-keep = Behalten
display-confirm-revert = Zurücksetzen
display-rejected = Diese Anordnung konnte nicht angewendet werden: { $reason }

# Eine riskante Änderung eines Anzeigewerkzeugs (kanshi, wlr-randr) behalten oder
# zurücksetzen: eine, die einen Bildschirm schwarz lassen oder den benutzten ausschalten kann
display-confirm-tool-title = Die Anzeigeänderung von { $tool } behalten?
display-confirm-tool-unknown-title = Diese Anzeigeänderung behalten?
display-confirm-always = Behalten und bei { $tool } nicht mehr fragen
display-confirm-a11y-description = Ein Anzeigewerkzeug hat einen Bildschirm geändert, den du gerade benutzt, oder einen Modus gewählt, den er noch nie gezeigt hat – dabei kann er schwarz bleiben. Eingabe behält die Änderung; Escape oder Abwarten stellt die vorherige Anordnung wieder her.
display-tool-reapply-refused = { $tool } wollte eine Anzeigeanordnung erneut anwenden, die du zurückgesetzt hast
display-tool-reapply-refused-unknown = Ein Anzeigewerkzeug wollte eine Anzeigeanordnung erneut anwenden, die du zurückgesetzt hast

# Drahtlose Empfänger, der untere Bereich desselben Blatts
wireless-heading = Drahtlose Bildschirme
wireless-share-audio = Audio teilen
wireless-stream = Stream
wireless-stream-auto = Automatisch
wireless-stream-fixed = Fest
wireless-stream-fps = { $fps } fps
wireless-credential-label = PIN oder Passwort des Empfängers
wireless-credential-placeholder = PIN oder Empfängerpasswort
wireless-credential-hint = Leer lassen, außer der Empfänger fragt danach. Eingabe verbindet.
wireless-scanning = Bildschirme werden gesucht …
wireless-scanning-detail = Halte den Empfänger wach und im selben Netzwerk.
wireless-scan-failed = Suche fehlgeschlagen
wireless-none = Keine Bildschirme gefunden
wireless-none-detail = Versuche „Aktualisieren“ oder prüfe das WLAN und den Übertragungsmodus des Empfängers.
wireless-connected = Verbunden
wireless-connecting = Wird verbunden …
wireless-connect = Verbinden
wireless-stop = Beenden
wireless-wait = Warten
wireless-protocols = Miracast · Google Cast · AirPlay · FCast

# Bildschirme identifizieren: die Karte auf jedem Bildschirm
display-identify-builtin = Eingebauter Bildschirm
display-identify-unnamed = Bildschirm { $number }

# Die Fensterreihe des aktuellen Arbeitsbereichs in der Übersicht.
overview-windows-title = Fenster in diesem Arbeitsbereich
overview-windows-description = Zuletzt verwendete zuerst. Links und Rechts wechseln zwischen ihnen, Enter öffnet eines. Klicke oder tippe auf ein Fenster, um dorthin zu wechseln, klicke mit der mittleren Taste, um es zu schließen, oder ziehe es auf einen anderen Arbeitsbereich, um es dorthin zu verschieben.
overview-window-move-failed = Dieses Fenster kann nicht verschoben werden

# Feste Tastenkombinationen der Shell, wie Tastaturhilfe und Tastaturübersicht sie nennen
shortcut-screenshot = Bildschirmfoto
shortcut-switch-windows = Fenster wechseln
shortcut-switch-windows-backwards = Fenster rückwärts wechseln
shortcut-switch-app-windows = Fenster der App wechseln
shortcut-close-tab = Tab schließen
shortcut-change-workspace = Arbeitsbereich wechseln
shortcut-new-window = Neues Fenster
shortcut-add-tab = Tab hier hinzufügen
shortcut-duplicate-tab = Tab duplizieren
shortcut-open-terminal = Terminal öffnen
shortcut-global-search = Globale Suche
shortcut-lock = Sperren
shortcut-workspace-wheel = Arbeitsbereichsrad
shortcut-display-layout = Anzeigeanordnung
shortcut-capture-screenshot = Bildschirmfoto aufnehmen
shortcut-picture-in-picture = Bild im Bild
shortcut-save-recent-replay = Letzte Aufnahme speichern
shortcut-workspace-history = Arbeitsbereichsverlauf
shortcut-explore-workspace-history = Arbeitsbereichsverlauf erkunden
shortcut-guide = Tastaturhilfe
shortcut-stop-remote-input = Alle Ferneingaben beenden
shortcut-toggle-mouse-keys = Maustasten umschalten
shortcut-vim-navigation = Vim-Navigation
shortcut-dismiss = Schließen

# Wie jedes änderbare Tastenkürzel des Desktops heißt
shortcut-name-search = Globale Suche
shortcut-name-workspace-overview = Übersicht
shortcut-name-lock = Sperren
shortcut-name-quick-settings = Schnelleinstellungen
shortcut-name-shortcut-help = Desktop-Hilfe
shortcut-name-focus-previous-window = Vorheriges Fenster fokussieren
shortcut-name-focus-next-window = Nächstes Fenster fokussieren
shortcut-name-previous-tab = Vorheriger Tab
shortcut-name-next-tab = Nächster Tab
shortcut-name-previous-strip = Vorherige Leiste
shortcut-name-next-strip = Nächste Leiste
shortcut-name-previous-workspace = Vorheriger Arbeitsbereich
shortcut-name-next-workspace = Nächster Arbeitsbereich
shortcut-name-toggle-maximize = Fenster maximieren oder wiederherstellen
shortcut-name-detach-tab = Tab in neue Gruppe verschieben
shortcut-name-attach-tab-left = Tab in die Gruppe links verschieben
shortcut-name-attach-tab-right = Tab in die Gruppe rechts verschieben
shortcut-name-move-group-left = Gruppe nach links verschieben
shortcut-name-move-group-right = Gruppe nach rechts verschieben
shortcut-name-move-group-to-previous-strip = Gruppe in die Leiste darüber verschieben
shortcut-name-move-group-to-next-strip = Gruppe in die Leiste darunter verschieben
shortcut-name-move-group-to-previous-display = Gruppe auf den linken Bildschirm verschieben
shortcut-name-move-group-to-next-display = Gruppe auf den rechten Bildschirm verschieben
shortcut-name-resize-narrower = Fenster schmaler machen
shortcut-name-resize-wider = Fenster breiter machen
shortcut-name-reopen-closed-tab = Geschlossenen Tab wieder öffnen
shortcut-name-focus-group-1 = Fenster 1
shortcut-name-focus-group-2 = Fenster 2
shortcut-name-focus-group-3 = Fenster 3
shortcut-name-focus-group-4 = Fenster 4
shortcut-name-focus-group-5 = Fenster 5
shortcut-name-focus-group-6 = Fenster 6
shortcut-name-focus-group-7 = Fenster 7
shortcut-name-focus-group-8 = Fenster 8
shortcut-name-focus-group-9 = Fenster 9
shortcut-name-open-files = Dateien öffnen
shortcut-name-open-terminal = Terminal öffnen
shortcut-name-clipboard-history = Verlauf der Zwischenablage
shortcut-name-notifications = Benachrichtigungen
shortcut-name-toggle-fullscreen = Vollbild
shortcut-name-toggle-magnifier = Bildschirmlupe
shortcut-name-magnifier-zoom-in = Lupe vergrößern
shortcut-name-magnifier-zoom-out = Lupe verkleinern
shortcut-name-power-menu = Ein/Aus-Menü
shortcut-name-next-input-source = Nächstes Tastaturlayout
shortcut-name-emoji-picker = Emoji

# Die Gruppen, in denen Tastenkürzel aufgeführt werden
shortcut-category-system = System
shortcut-category-navigation = Navigation
shortcut-category-windows = Fenster
shortcut-category-apps = Apps
shortcut-category-capture = Aufnahme
shortcut-category-accessibility = Bedienungshilfen

# Die Tastenkürzel als Ressource an der Aktionstür
keyboard-shortcuts-resource = Tastenkürzel
keyboard-shortcuts-list = Tastenkürzel auflisten
keyboard-shortcuts-list-description = Jedes Tastenkürzel des Desktops mit Bezeichnung, aktueller und voreingestellter Belegung, Änderbarkeit und Kategorie
keyboard-shortcuts-reset-all = Alle Tastenkürzel zurücksetzen
keyboard-shortcuts-reset-all-description = Jedes änderbare Tastenkürzel des Desktops auf seine Voreinstellung zurücksetzen
keyboard-shortcut-rebind = Tastenkürzel für { $shortcut } ändern
keyboard-shortcut-rebind-description = { $shortcut } liegt auf { $chords } (Voreinstellung { $defaults }). Gib eine andere Belegung an; eine bereits vergebene wird abgelehnt und ihr Inhaber genannt.
keyboard-shortcut-reset = Tastenkürzel für { $shortcut } zurücksetzen
keyboard-shortcut-reset-description = { $shortcut } wieder auf { $defaults } legen
keyboard-shortcut-conflict = { $chord } ist bereits für „{ $shortcut }“ belegt.
keyboard-shortcut-invalid = Dieses Tastenkürzel wird nicht unterstützt.

# Tastaturhilfe (F1)
guide-title = Desktop-Hilfe
guide-reopen-hint = Drücke überall F1, um diese Hilfe erneut zu öffnen · Wähle eine Navigationstaste aus, um sie zu ändern
guide-done = Fertig
guide-press-new-shortcut = Neues Tastenkürzel für „{ $action }“ drücken · Esc bricht ab
guide-message-esc-cancels = { $message } · Esc bricht ab
guide-rebind-needs-modifier = Kombiniere druckbare Tasten mit Super, Strg, Alt oder Umschalt.
guide-rebind-in-use = Bereits belegt für „{ $action }“. Drücke ein anderes Tastenkürzel.
guide-rebind-reserved = F1 und Druck sind für den Desktop reserviert.
guide-rebind-unsupported = Dieses Tastenkürzel wird nicht unterstützt. Drücke ein anderes Tastenkürzel.
guide-rebind-taken-by-action = Bereits für eine Aktion belegt. Drücke ein anderes Tastenkürzel.
guide-rebind-save-failed = Das Tastenkürzel konnte nicht gespeichert werden. Drücke ein anderes Tastenkürzel.
guide-reset-in-use = Eine Voreinstellung ist bereits für „{ $action }“ belegt. Ändere zuerst dieses Tastenkürzel.
guide-reset-taken-by-action = Eine Voreinstellung führt jetzt eine Aktion aus. Ändere zuerst dieses Tastenkürzel.
guide-reset-invalid = Die voreingestellten Tastenkürzel konnten nicht wiederhergestellt werden.
guide-reset-save-failed = Die Voreinstellungen konnten nicht wiederhergestellt werden.
guide-controller-intro = Mit dem Stick oder Steuerkreuz bewegen. Halte { $guide }, um Fenster anzuordnen. In einem Vollbildspiel erreichen nur Kombinationen mit { $guide } den Desktop.
guide-row-move-focus = Fokus bewegen
guide-row-select = Auswählen
guide-row-back = Zurück
guide-row-app-actions = Aktionen dieser App
guide-row-search = Suche
guide-row-this-guide = Diese Hilfe
guide-row-windows = Fenster
guide-row-tabs = Tabs
guide-row-move-tab = Tab verschieben
guide-row-strips = Leisten
guide-row-workspaces = Arbeitsbereiche
guide-row-move-group = Gruppe verschieben
guide-row-move-group-strip = Gruppe in Leiste verschieben
guide-row-move-window = Fenster verschieben
guide-row-resize = Größe ändern
guide-row-quick-settings = Schnelleinstellungen
guide-touch-intro = Drei Finger bewegen sich entlang der Fenster und zwischen Streifen; vier Finger wechseln den Arbeitsbereich.
guide-touch-focus-next-window = Nächstes Fenster fokussieren
guide-touch-focus-previous-window = Vorheriges Fenster fokussieren
guide-touch-strip-above = Streifen darüber
guide-touch-strip-below = Streifen darunter
guide-touch-previous-workspace = Vorheriger Arbeitsbereich
guide-touch-next-workspace = Nächster Arbeitsbereich
guide-touch-open-overview = Übersicht öffnen
guide-touch-close-overview = Übersicht schließen
guide-navigation-heading = NAVIGATIONSKÜRZEL BEARBEITEN
guide-essentials-heading = DESKTOP-GRUNDLAGEN
guide-group-windows = FENSTER & ARBEITSBEREICHE
guide-group-search = SUCHE & SYSTEM
guide-group-capture = AUFNAHME & ANZEIGE
guide-overview = Übersicht
guide-unassigned = Nicht belegt
guide-unassigned-inline = nicht belegt
guide-pointer = Zeiger: klicken zum Fokussieren · Tabs oder Gruppen ziehen zum Anordnen · Rand ziehen zum Ändern der Größe
guide-your-shortcuts = Deine Tastenkürzel: { $shortcuts }
guide-vim-mode = Vim-Modus · h j k l
guide-a11y-name = Tastatur- und Mausübersicht des Desktops
guide-a11y-description = Fenster fokussieren mit { $window-back } oder { $window-next }; Tabs mit { $tab-back } oder { $tab-next }; Leisten mit { $strip-back } oder { $strip-next }; Arbeitsbereiche mit { $workspace-back } oder { $workspace-next }; Gruppen verschieben mit { $group-left } oder { $group-right }; Größe ändern mit { $narrower } oder { $wider }. Wichtige Desktop-Aktionen: { $essentials }. Klicke auf ein angezeigtes Navigationskürzel, um es zu ändern; Escape bricht eine laufende Änderung ab. Eine geänderte Zeile erhält eine Schaltfläche mit gebogenem Pfeil, die ihre Voreinstellungen wiederherstellt. Mit der Maus klickst du Fenster und Tabs an, ziehst Tabs oder freie Stellen der Tableiste zum Anordnen und ziehst Fensterzwischenräume, um die Größe zu ändern. Escape schließt diese Hilfe.

# Geräte und Medien, die Detailansicht der Schnelleinstellungen
devices-media-title = Geräte und Medien
devices-media-description = Lokale Audioausgänge und identitätszertifizierte Tilo-Geräte. Übergaben an entfernte Geräte laufen direkt und verschlüsselt, wenn sie erreichbar sind. Mit Pfeil nach oben und unten, Bild auf und Bild ab, Pos1 oder Ende bewegst du dich durch alle Ziele.
devices-media-list = Medienziele

# Schnelleinstellungen
qs-title = Schnelleinstellungen
qs-a11y-description = Häufig verwendete Systemsteuerungen
qs-back = Zurück
qs-on = An
qs-off = Aus
qs-connected = Verbunden
qs-disconnected = Getrennt
qs-connecting = Wird verbunden …
qs-not-connected = Nicht verbunden
qs-tile-settings = Einstellungen
qs-tile-settings-description = Geräteeinstellungen öffnen
qs-tile-privacy = Datenschutz
qs-tile-privacy-description = App-Berechtigungen und erzwungene Zugriffe für diesen Arbeitsbereich
qs-tile-history = Verlauf
qs-tile-history-description = Benachrichtigungsverlauf
qs-tile-power = Ein/Aus
qs-tile-power-description = Sperren, abmelden, neu starten oder herunterfahren
qs-volume = Lautstärke
qs-brightness = Helligkeit
qs-brightness-display = Helligkeit, { $display }
qs-slider-a11y-description = Mit Pfeil links und rechts anpassen
qs-network = Netzwerk
qs-wired-network = Kabelnetzwerk
qs-cable-unplugged = Kabel nicht angeschlossen
qs-bluetooth = Bluetooth
qs-vpn = VPN
qs-vpn-no-profiles = Keine Profile
qs-night-light = Nachtlicht
qs-airplane-mode = Flugmodus
qs-mobile-data = Mobile Daten
qs-do-not-disturb = Bitte nicht stören
qs-do-not-disturb-on = Benachrichtigungen zurückgehalten
qs-rotation-lock = Drehsperre
qs-rotation-locked = Gesperrt
qs-rotation-unlocked = Dreht sich mit dem Gerät
qs-devices = Geräte
qs-devices-audio-and-media = Audio und Medien
qs-devices-count = { $count ->
    [one] 1 Gerät
   *[other] { $count } Geräte
}
qs-devices-a11y-description = Persönliche Geräte, Audioausgänge und Medienziele
qs-clipboard = Zwischenablage
qs-clipboard-empty = Leer
qs-clipboard-count = { $count ->
    [one] 1 Eintrag
   *[other] { $count } Einträge
}
qs-clipboard-a11y-description = Privater Verlauf der Zwischenablage
qs-power-saver = Energiesparen
qs-power-balanced = Ausgewogen
qs-power-performance = Leistung
qs-power-profile-a11y = Energieprofil ändern
qs-power-battery = { $percent } % Akku
qs-power-ac = Netzbetrieb
qs-power-kept-awake-by-app = { $power } · { $app } verhindert den Ruhezustand
qs-power-kept-awake-by-apps = { $power } · { $count } Apps verhindern den Ruhezustand

# Schnelleinstellungen: Steuerungen bearbeiten
qs-edit-controls = Steuerungen bearbeiten
qs-edit-help = Zum Anordnen ziehen · Strg + Auf/Ab
qs-edit-a11y-description = Steuerungen bearbeiten; zum Anordnen ziehen oder Strg plus Pfeiltasten verwenden
qs-edit-active-heading = AKTIVE STEUERUNGEN
qs-edit-available-heading = VERFÜGBAR
qs-edit-reset = Standard wiederherstellen
qs-edit-reset-a11y = Standard der Schnelleinstellungen wiederherstellen
qs-edit-done = Fertig
qs-edit-done-a11y = Bearbeiten der Schnelleinstellungen beenden
qs-edit-required-a11y = Erforderliche Steuerung der Schnelleinstellungen
qs-edit-remove-a11y = Steuerung aus den Schnelleinstellungen entfernen
qs-edit-add-a11y = Steuerung zu den Schnelleinstellungen hinzufügen
qs-edit-required-control-a11y = Erforderlich; mit Strg plus Pfeiltasten anordnen
qs-edit-optional-control-a11y = Zum Hinzufügen oder Entfernen auswählen; mit Strg plus Pfeiltasten anordnen

# Schnelleinstellungen: WLAN
qs-wifi = WLAN
qs-wifi-off = WLAN ist aus
qs-wifi-scanning = Netzwerke werden gesucht …
qs-wifi-scan-failed = Suche fehlgeschlagen · Zum erneuten Versuch aktualisieren
qs-wifi-none = Keine Netzwerke gefunden
qs-wifi-choose-a11y = Wähle ein Netzwerk, aktualisiere die Suche oder verwalte WLAN und Hotspot
qs-wifi-rescan-a11y = Nach WLAN-Netzwerken suchen
qs-wifi-secured = Gesichert
qs-wifi-open = Offen
qs-wifi-turn-on = WLAN einschalten
qs-wifi-turn-off = WLAN ausschalten
qs-wifi-hotspot = Hotspot
qs-wifi-hotspot-a11y = WLAN-Hotspot
qs-wifi-hotspot-sharing = Diese Verbindung wird geteilt
qs-wifi-hotspot-turn-on = WLAN-Hotspot einschalten
qs-wifi-hotspot-turn-off = WLAN-Hotspot ausschalten

# Schnelleinstellungen: Bluetooth
qs-bluetooth-empty = Keine Geräte gefunden
qs-bluetooth-none-a11y = Keine gekoppelten Bluetooth-Geräte gefunden
qs-bluetooth-choose-a11y = Wähle ein gekoppeltes Bluetooth-Gerät
qs-bluetooth-device-connected-a11y = Verbunden; Geräteaktionen öffnen
qs-bluetooth-device-paired-a11y = Gekoppelt; zum Verbinden aktivieren

# Schnelleinstellungen: VPN
qs-vpn-empty = Noch keine VPN-Profile · füge eins in den Einstellungen hinzu
qs-vpn-none-a11y = Keine VPN-Profile eingerichtet
qs-vpn-choose-a11y = Ein eingerichtetes VPN-Profil verbinden oder trennen
qs-vpn-connected-a11y = VPN verbunden
qs-vpn-disconnected-a11y = VPN getrennt

# Fehler der Schnelleinstellungen als Benachrichtigungen
qs-action-failed = „{ $action }“ fehlgeschlagen
qs-action-failed-refreshing = { $error }. Der aktuelle Gerätezustand wird aktualisiert.
qs-bluetooth-connect-failed = { $device } konnte nicht verbunden werden
qs-bluetooth-disconnect-failed = { $device } konnte nicht getrennt werden
qs-bluetooth-disconnected = { $device } wurde von diesem Gerät getrennt
qs-vpn-change-failed = { $profile } konnte nicht geändert werden

# Portal- und Berechtigungsblätter: App-Auswahl, globale Tastenkürzel, Prüfungen, Administratorzugriff, Netzwerkgeheimnisse, Aktionsfreigaben
portal-choose-app-title = App auswählen
portal-chooser-this-item = dieses Element
portal-chooser-detail = { $app } möchte { $subject } öffnen
portal-open = Öffnen
portal-cancel = Abbrechen
portal-allow = Erlauben
portal-authenticate = Authentifizieren
portal-connect = Verbinden
portal-shortcuts-title = Globale Tastenkürzel festlegen
portal-shortcuts-detail = Wähle eine Zeile und drücke dann das Tastenkürzel. Tastenkürzel von Tilo und der aktiven App können nicht belegt werden. Rücktaste leert eine Zeile.
portal-save-shortcuts = Tastenkürzel speichern
portal-shortcut-not-set = Nicht festgelegt
portal-shortcut-needs-modifier = Halte Strg, Alt, Umschalt oder Super gedrückt und drücke dann eine Taste.
portal-shortcut-reserved = Von Tilo reserviert. Wähle ein anderes Tastenkürzel.
portal-shortcut-duplicate = In dieser Anfrage bereits vergeben. Wähle ein anderes Tastenkürzel.
portal-shortcut-taken = Bereits von { $app } verwendet. Wähle ein anderes Tastenkürzel.
portal-shortcut-another-app = einer anderen App
portal-auth-heading = Administratorzugriff
portal-auth-ended = Die Authentifizierung wurde unerwartet beendet.
portal-auth-rejected = Dieses Passwort wurde nicht akzeptiert.
portal-grant-title = { $app } möchte { $target } verwenden
portal-grant-detail-confirms = { $risk }
    Du wirst trotzdem bei jeder Ausführung um Bestätigung gebeten.
portal-grant-detail-unattended = { $risk }
    Die Aktion läuft, ohne dich erneut zu fragen.
portal-grant-once = Einmal erlauben
portal-grant-session = Für diese Sitzung erlauben
portal-grant-always = In diesem Arbeitsbereich immer erlauben
portal-grant-deny = Ablehnen
portal-risk-none = Zeigt nur Informationen an
portal-risk-low = Geringes Risiko: eine gewöhnliche, umkehrbare Änderung
portal-risk-destructive = Destruktiv: kann löschen oder überschreiben
portal-risk-privileged = Privilegiert: überschreitet eine Sicherheitsgrenze
portal-risk-sensitive = Sensibel: liest oder exportiert private Daten
portal-usb-title = USB-Geräte auswählen
portal-usb-reason = Wähle Geräte einzeln aus; mit Lese- und Schreibzugriff kann ein Gerät verändert werden.
portal-usb-resource = Leertaste oder Klick schaltet eine Zeile um. Der Zugriff endet beim Freigeben oder Trennen.
portal-usb-confirm = Auswahl erlauben
portal-usb-unnamed-device = USB-Gerät
portal-usb-row-read-write = { $device } — Lesen und Schreiben
portal-usb-row-read-only = { $device } — Nur lesen
portal-account-title = Deine Identität teilen?
portal-account-reason = Die App fragt nach deinem Namen und deiner Kontokennung.
portal-account-resource = Deine ausgewählte Identität für diesen Arbeitsbereich
portal-account-confirm = Identität teilen
portal-launcher-web-app-title = Diese Web-App installieren?
portal-launcher-title = Diesen Starter hinzufügen?
portal-launcher-reason = Prüfe den Starter, bevor er zu deiner App-Bibliothek hinzugefügt wird. Das mitgelieferte Bild wird geprüft, aber nie über einen App-Pfad oder Dateideskriptor geöffnet.
portal-launcher-confirm = Starter hinzufügen
portal-launcher-icon-supplied = Mitgeliefertes Bild (geprüft)
portal-launcher-icon-monogram = Tilo-Monogramm
portal-chooser-nothing-granted = Dieser App wurden keine Dateien zur Auswahl freigegeben.
portal-chooser-file-not-granted = Dieser App wurde { $path } nicht freigegeben.
portal-chooser-no-file = Es gibt keine Datei zum Öffnen.
portal-chooser-session-closing = Die Sitzung wird beendet.
portal-chooser-other-workspace = Der Arbeitsbereich der App ist nicht der angezeigte.
portal-launcher-icon-compass = Tilo-Webkompass
portal-background-title = Hintergrundaktivität erlauben?
portal-background-resource = Die App kann weiterarbeiten, nachdem ihr Fenster geschlossen wurde.
portal-background-confirm = Im Hintergrund erlauben
portal-wallpaper-title = Hintergrundbild des Arbeitsbereichs festlegen?
portal-wallpaper-preview = Vor dem Anwenden dieser privaten Arbeitsbereichseinstellung wird eine Vorschau angezeigt.
portal-wallpaper-private = Dies ändert nur den privaten Arbeitsbereichshintergrund dieses Geräts.
portal-wallpaper-resource = Auf { $target } anwenden
    Privates importiertes Bild
portal-wallpaper-confirm = Hintergrundbild festlegen
portal-email-title = Deine Mail-App öffnen?
portal-email-reason = Prüfe die vorausgefüllte Nachricht, bevor sie an deinen eingerichteten Mail-Anbieter übergeben wird.
portal-email-confirm = Mail-App öffnen
portal-email-cc = Cc: { $address }
portal-email-bcc = Bcc: { $address }
portal-email-no-recipient = Kein Empfänger vorausgefüllt
portal-email-subject = Betreff: { $subject }
portal-email-attachments = { $count ->
    [one] 1 Anhang wird über das Dokumentportal verfügbar sein.
   *[other] { $count } Anhänge werden über das Dokumentportal verfügbar sein.
}
portal-remember-device = Auf diesem Gerät merken
portal-secret-required = Fülle alle Pflichtfelder aus.
portal-secret-wifi-length = Ein WLAN-Passwort hat 8 bis 63 Zeichen oder 64 Hexadezimalziffern.
portal-network-heading = Netzwerk
portal-network-this-network = dieses Netzwerk
portal-network-password = Passwort
portal-network-key = Schlüssel
portal-network-user-name = Benutzername
portal-network-private-key-password = Passwort des privaten Schlüssels
portal-network-anonymous-identity = Anonyme Identität (optional)
portal-network-wifi-title = WLAN-Passwort erforderlich
portal-network-enter-password = Gib das Passwort für „{ $network }“ ein.
portal-network-wired-sign-in-title = Anmeldung am kabelgebundenen Netzwerk
portal-network-sign-in-title = Netzwerkanmeldung
portal-network-sign-in-lead = „{ $network }“ benötigt deinen Benutzernamen und dein Passwort.
portal-network-vpn-title = VPN-Anmeldung
portal-network-vpn-lead = Gib die Zugangsdaten für „{ $network }“ ein.
portal-network-sim-pin-title = SIM-PIN erforderlich
portal-network-sim-pin-lead = Gib die PIN ein, um die SIM für „{ $network }“ zu entsperren.
portal-network-sim-pin = SIM-PIN
portal-network-mobile-title = Passwort für mobiles Breitband erforderlich
portal-network-dsl-title = DSL-Passwort erforderlich
portal-network-retry = Der letzte Versuch hat nicht funktioniert. Prüfe die Eingabe und versuche es erneut.
portal-network-wps = Du kannst auch die WPS-Taste am Router drücken.
portal-network-cert-pinned = Das Zertifikat des Netzwerks wird mit der Zertifizierungsstelle dieser Verbindung geprüft.
portal-network-cert-pinned-domain = Das Zertifikat des Netzwerks wird mit der Zertifizierungsstelle dieser Verbindung geprüft. Es muss zu { $domain } gehören.
portal-network-cert-system = Das Zertifikat des Netzwerks wird mit den vertrauenswürdigen Zertifizierungsstellen des Systems geprüft.
portal-network-cert-system-domain = Das Zertifikat des Netzwerks wird mit den vertrauenswürdigen Zertifizierungsstellen des Systems geprüft. Es muss zu { $domain } gehören.
portal-network-cert-unchecked = Das Zertifikat des Netzwerks wird nicht geprüft. Fahre nur in einem Netzwerk fort, dem du vertraust.

# Obere Leiste
topbar-clocks-notification-title = Uhren
topbar-clocks-notification-body = Die Zeit ist abgelaufen

# Arbeitsbereichsaktivität: woran eine mitarbeitende Person gerade arbeitet
host-activity-working-on = Arbeitet an { $target }
host-activity-working-in = Arbeitet in { $app }
host-activity-editing = Bearbeitet gemeinsame Inhalte
host-activity-organizing = Ordnet gemeinsame Inhalte
host-activity-updating-details = Aktualisiert Details des Arbeitsbereichs
host-activity-reviewing-discussion = Sieht sich die Diskussion an
host-activity-cleaning-up = Räumt gemeinsame Inhalte auf
host-activity-restoring = Stellt gemeinsame Inhalte wieder her
host-activity-managing-access = Verwaltet den Zugriff auf den Arbeitsbereich
host-activity-maintaining-history = Pflegt den Verlauf des Arbeitsbereichs
host-activity-reviewing-sensitive = Prüft vertraulichen Verlauf

# Bildschirmfreigabe, wie das Personen-Popover sie beschreibt
host-exposure-kind-unidentified = nicht identifizierte Sitzung
host-exposure-kind-agent = Agent
host-exposure-kind-person = Person
host-exposure-surface = Oberfläche { $surface }
host-exposure-controls = { $name } ({ $kind }) steuert { $target }
host-exposure-views = { $name } ({ $kind }) sieht { $target }

# Einladungen
host-invite-unavailable-title = Einladen nicht möglich
host-invite-unavailable-body = Du brauchst die Berechtigung, Personen in diesen Arbeitsbereich einzuladen.
host-this-workspace = diesen Arbeitsbereich

# Absender von Benachrichtigungen
host-source-applications = Anwendungen
host-source-system = System
host-source-app-actions = App-Aktionen

# App-Starts und Sitzungsaktionen
host-app-did-not-open-title = Anwendung wurde nicht geöffnet
host-app-did-not-open-body = Die App wurde gestartet, hat aber kein Fenster erstellt. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-action-failed = Sitzungsaktion fehlgeschlagen
host-session-lock-failed = Die Sitzung konnte nicht gesperrt werden. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-logout-failed = Abmelden fehlgeschlagen. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-switch-user-failed = Benutzerwechsel fehlgeschlagen. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-suspend-failed = Bereitschaft fehlgeschlagen. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-restart-failed = Neustart fehlgeschlagen. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-shut-down-failed = Herunterfahren fehlgeschlagen. Sieh im Sitzungsprotokoll nach und versuche es erneut.
host-session-action-blocked = Sitzungsaktion blockiert

# Suche und App-Aktionen
host-action-started = „{ $title }“ gestartet
host-action-admitted = Aktionsauftrag { $job } wurde angenommen
host-action-cannot-run = „{ $title }“ kann nicht ausgeführt werden
host-search-placeholder = Suchen …
host-search-in = In { $scope } suchen …
host-search-in-resource-fallback = Ressource
host-search-scope-app = App { $title }
host-json-arguments = JSON-Argumente: {"{"}{ $names }{"}"}
host-a11y-filter-all = Filter: Alle
host-a11y-filter-more = Filter: Mehr
host-a11y-filter-kind = Filter: { $kind }
host-a11y-scope = Bereich: { $scope }
host-a11y-search-filters = Suchfilter
host-a11y-result-kinds = Ergebnisarten
host-a11y-search-scope = Suchbereich

# Tabs
host-tab-duplicate-failed = Dieser Tab kann nicht dupliziert werden
host-tab-move-failed = Dieser Tab kann nicht verschoben werden
host-a11y-tab-menu = Tab-Menü für { $title }

# WLAN-Verbindungen aus den Schnelleinstellungen
host-wifi-connected-title = Mit { $ssid } verbunden
host-wifi-connected-shared-credential = Die verschlüsselten Zugangsdaten von einem deiner zertifizierten Geräte wurden verwendet.
host-wifi-connected-sharing-unavailable = Verbunden, aber verschlüsseltes Teilen ist vorübergehend nicht verfügbar.
host-wifi-connected-shared = Verbunden und für jedes zertifizierte Gerät einzeln verschlüsselt.
host-wifi-connected-saved = Verbunden. Dieses gespeicherte Netzwerk wird automatisch wieder verbunden.
host-wifi-connected-open = Mit dem offenen Netzwerk verbunden.
host-wifi-saved-password-stale = Das gespeicherte Passwort funktioniert nicht mehr. Gib das aktuelle Passwort ein.
host-wifi-join-check-password = Verbindung fehlgeschlagen. Prüfe das Passwort.
host-wifi-connect-failed-title = Verbindung mit { $ssid } fehlgeschlagen
host-wifi-connect-failed-body = Das Netzwerk hat die Verbindung nicht angenommen. Versuche es in den WLAN-Einstellungen erneut.

# Schnelleinstellungen, die das Gerät abgelehnt hat
host-qs-change-failed-brightness = Helligkeit konnte nicht geändert werden
host-qs-change-failed-volume = Lautstärke konnte nicht geändert werden
host-qs-change-failed-value-restored = Das Bedienelement zeigt wieder den aktuellen Wert des Geräts.
host-qs-change-failed-wifi = WLAN konnte nicht geändert werden
host-qs-change-failed-bluetooth = Bluetooth konnte nicht geändert werden
host-qs-change-failed-wired = Kabelnetzwerk konnte nicht geändert werden
host-qs-change-failed-airplane = Flugmodus konnte nicht geändert werden
host-qs-change-failed-hotspot = Persönlicher Hotspot konnte nicht geändert werden
host-qs-change-failed-mobile = Mobilfunk konnte nicht geändert werden
host-qs-change-failed-power = Energiemodus konnte nicht geändert werden
host-qs-change-failed-state-restored = Das Bedienelement zeigt wieder den aktuellen Zustand des Geräts.

# Ergebnisse im Arbeitsbereichsverlauf
host-history-partial-undo-title = Ausgewählte Arbeit teilweise rückgängig gemacht
host-history-partial-undo-body = Spätere Änderungen anderer wurden beibehalten. Der Verlauf zeigt, welche Teile unverändert blieben.
host-history-reverted-title = Raumänderung zurückgesetzt
host-history-reverted-body = Vorwärts geschrieben: Die Version, von der du zurückgesetzt hast, bleibt im Verlauf.
host-history-compared-title = Vergleich bereit
host-history-compared-body = Der genaue kausale Vorher-nachher-Zustand wird im Verlauf angezeigt.
host-history-named-title = Version benannt
host-history-named-body = Im Verlauf dieses Arbeitsbereichs gespeichert, damit alle hier sie sehen.
host-history-task-updated-title = Aufgabenzentrale aktualisiert
host-history-task-updated-body = Die Aufgabe und ihr Prüfprotokoll werden mit diesem Arbeitsbereich geteilt.
host-review-activity-title = Arbeitsbereichsaktivität prüfen
host-review-activity-body = Der Verlauf ist geöffnet, damit du die zugehörigen Änderungen vor deiner Entscheidung ansehen kannst.

# Anrufübergabe zwischen Geräten
host-call-handoff-title = Anrufübergabe
host-call-handoff-approved = Einmal genehmigt; dieses Gerät bereitet seine lokalen Medien vor
host-call-handoff-declined = Auf diesem Gerät abgelehnt
host-call-handoff-waiting = Warte auf ausdrückliche Genehmigung auf { $device }; der Anruf läuft hier weiter
host-call-move-failed = Anruf konnte nicht verschoben werden

# Geräte und Medien: was eine Aktion im Untermenü bewirkt hat
host-fabric-notice-title = Geräte & Medien
host-fabric-action-failed = Geräteaktion konnte nicht abgeschlossen werden
host-fabric-certified-display = einen zertifizierten Bildschirm
host-fabric-the-display = den Bildschirm
host-fabric-calibration-applied = Vorgeschlagene Anordnung für { $device } übernommen
host-fabric-calibration-dismissed = Anordnungsvorschlag für { $device } verworfen
host-fabric-calibration-restored = Vorherige Anordnung für { $device } wiederhergestellt
host-fabric-audio-allowed = { $source } darf in dieser Sitzung hier abspielen
host-fabric-audio-declined = Audio von { $source } abgelehnt
host-fabric-speaker-joined = { $group } als { $role } beigetreten
host-fabric-speaker-declined = Beitritt zu { $group } abgelehnt
host-fabric-speaker-stopped = Wiedergabe auf { $group } beendet
host-fabric-speaker-playing = Wiedergabe auf { $group }
host-fabric-speaker-playing-media = Wiedergabe auf { $group }; die Medien laufen weiter von hier
host-fabric-speaker-left = Dieses Gerät hat { $group } verlassen
host-fabric-speaker-listen-allowed = { $group } kann mit diesem Mikrofon kalibrieren
host-fabric-speaker-listen-declined = { $group } verwendet dieses Mikrofon nicht
host-fabric-speaker-listen-stopped = { $group } kann dieses Mikrofon nicht mehr verwenden
host-fabric-audio-route-dismissed = Unterbrochene Übertragung { $route } verworfen
host-fabric-audio-route-stopped = Übertragung { $route } beendet
host-fabric-input-allowed = { $app } darf diese Eingabe einmal verwenden
host-fabric-input-declined = Entfernte Eingabe für { $app } abgelehnt
host-fabric-input-focused-app-allowed = Die fokussierte App darf diese entfernte Eingabe einmal verwenden
host-fabric-input-stopped = Sitzung für entfernte Eingabe beendet
host-fabric-mirror-allowed = { $device } darf einmal hierher spiegeln
host-fabric-mirror-declined = Bildschirmspiegelung von { $device } abgelehnt
host-fabric-mirror-stopped = Bildschirmspiegelung mit { $device } beendet
host-fabric-pairing-approved = Kopplung von { $accessory } für diese Übergabe genehmigt
host-fabric-pairing-declined = Kopplung von { $accessory } abgelehnt
host-fabric-handoff-retrying = Übergabe von { $accessory } wird erneut versucht
host-fabric-handoff-cancelling = Übergabe von { $accessory } wird abgebrochen
host-fabric-controller-allowed = { $controller } von { $device } für diese Sitzung erlaubt
host-fabric-controller-declined = Controller von { $device } abgelehnt
host-fabric-controller-stopped = Weiterleitung von { $controller } beendet
host-fabric-media-move-cancelled = Verschieben von { $title } auf { $device } abgebrochen
host-fabric-media-retrying = { $title } auf { $device } wird erneut versucht
host-fabric-output-routed = Audio wird jetzt an { $output } geleitet
host-fabric-input-selected = Aufnahmequelle: { $input }
host-fabric-tethering-allowed = { $device } darf die Verbindung dieses Telefons nutzen
host-fabric-tethering-declined = Tethering für { $device } abgelehnt
host-fabric-tethering-disconnected = Tethering mit { $device } getrennt
host-fabric-asked-mirror = { $device } wurde gebeten, die Bildschirmspiegelung einmal zu erlauben
host-fabric-asked-capture = Einmalige Freigabe bei { $device } angefragt: { $input }
host-fabric-asked-audio = { $device } wurde gebeten, dieses Audio einmal zu erlauben; bis zur Genehmigung bleibt die Wiedergabe lokal
host-fabric-asked-tethering = { $device } wurde gebeten, seine Verbindung zu teilen
host-fabric-asked-controller = { $device } wurde gebeten, diesen Controller einmal für die fokussierte App zu erlauben
host-fabric-media-preparing = Medien für { $device } werden vorbereitet; dieses Gerät bleibt aktiv, bis sie dort geöffnet sind
host-fabric-bluetooth-moving = { $accessory } wird auf { $device } verschoben; es bleibt hier, bis das Gerät seine Kopplung bestätigt
host-fabric-place-left = { $device } wird nach der Bestätigung links von diesem Bildschirm angeordnet
host-fabric-place-right = { $device } wird nach der Bestätigung rechts von diesem Bildschirm angeordnet
host-fabric-place-above = { $device } wird nach der Bestätigung über diesem Bildschirm angeordnet
host-fabric-place-below = { $device } wird nach der Bestätigung unter diesem Bildschirm angeordnet
host-fabric-control-revoked = { $device } kann dieses Gerät nicht mehr steuern
host-fabric-control-granted = { $device } darf dieses Gerät 15 Minuten lang steuern

# Barrierefreiheitsbaum: Arbeitsbereich-Overlays
a11y-workspace = Tilo-Arbeitsbereich
a11y-member-sheet = Berechtigungsentscheidung für den Arbeitsbereich
a11y-member-sheet-description = Wähle mit Pfeil nach oben oder unten eine klar beschriftete Berechtigung, bestätige mit Eingabe oder brich mit Escape ab. Folgenreiche Änderungen verlangen eine eigene ausdrückliche Bestätigung und bieten die Aktivität des Arbeitsbereichs zur Prüfung an.
a11y-overview = Arbeitsbereichsübersicht
a11y-new-workspace = Neuer Arbeitsbereich
a11y-workspace-settings = Arbeitsbereichseinstellungen
a11y-workspace-settings-description = Identität, Personen, Berechtigungen, Apps und Dateien des Arbeitsbereichs sowie signierte Eigentümerverwaltung.
a11y-manage-ownership = Eigentum am Arbeitsbereich verwalten
a11y-manage-ownership-description = Öffnet dieselben signierten Eigentumsaktionen, die auch Suche und Automatisierung anbieten. Nimmt die empfangende Person an, ist eine ausstehende Übertragung sofort abgeschlossen.
a11y-workspace-name = Name des Arbeitsbereichs
a11y-workspace-settings-content = Inhalt der Arbeitsbereichseinstellungen
a11y-at-a-glance = Auf einen Blick

# Barrierefreiheitsbaum: Verlauf der Zwischenablage
a11y-clipboard-data = Daten der Zwischenablage
a11y-clipboard-item-description = Von { $source }; { $age }; { $bytes } Byte; { $pinned ->
        [yes] angeheftet
       *[no] nicht angeheftet
    }
a11y-clipboard-use = Eintrag verwenden
a11y-clipboard-unpin = Eintrag lösen
a11y-clipboard-pin = Eintrag anheften
a11y-clipboard-remove = Eintrag entfernen
a11y-clipboard-search = Verlauf der Zwischenablage durchsuchen
a11y-clipboard-search-description = Tippe, um nach Vorschau, Inhaltstyp oder Quellgerät zu filtern. Rücktaste bearbeitet, Escape leert die Suche.
a11y-clipboard = Zwischenablage
a11y-clipboard-description = Privater Verlauf der Zwischenablage, geteilt nur mit deinen authentifizierten Geräten. Tippe zum Suchen. Wähle mit Pfeil nach oben und unten, Bild auf und Bild ab, Pos1 oder Ende einen Eintrag und verwende ihn mit Eingabe oder Leertaste.
a11y-clipboard-list = Aufbewahrte Einträge der Zwischenablage

# Barrierefreiheitsbaum: Bluetooth-Zubehör verschieben
a11y-bluetooth-accessory = Bluetooth-Zubehör
a11y-bluetooth-disconnect = { $accessory } von diesem Gerät trennen
a11y-bluetooth-move-to = Zu { $device } verschieben
a11y-bluetooth-destination-local = Das ist das aktuelle zertifizierte Gerät
a11y-bluetooth-destination-online = Das Ziel bestätigt seine eigene lokale Kopplung, bevor dieses Gerät die Verbindung trennt
a11y-bluetooth-destination-offline = Zertifiziertes Gerät, offline
a11y-bluetooth-move = { $accessory } verschieben
a11y-bluetooth-move-description = Wähle ein zertifiziertes Ziel, das bereits mit diesem Zubehör gekoppelt ist. Kopplungsschlüssel verlassen keines der Geräte. Mit Pfeil nach oben und unten, Bild auf und Bild ab, Pos1 oder Ende erreichst du jedes Ziel.
a11y-bluetooth-destinations = Bluetooth-Ziele

# Barrierefreiheitsbaum: Zeilen für Geräte und Medien
a11y-certified-display = Zertifizierter Bildschirm
a11y-placement-suggested = Vorgeschlagene Anordnung für { $display }
a11y-placement-suggested-description = Aktuelle lokale Sensordaten schlagen eine umkehrbare Anordnung vor; die Nähe hat keine Identität festgestellt
a11y-placement-apply = Diese umkehrbare Bildschirmanordnung übernehmen
a11y-placement-dismiss = Diesen Anordnungsvorschlag verwerfen
a11y-placement-undo = Anordnung für { $display } rückgängig machen
a11y-display-fallback = Bildschirm
a11y-placement-undo-description = Stellt die genaue Anordnung von vor der Kalibrierung wieder her
a11y-speaker-join = { $group } als { $role } beitreten
a11y-speaker-join-description = { $leader } hat gefragt; du wirst einmal gefragt und kannst jederzeit austreten
a11y-speaker-join-allow = Dieser Lautsprechergruppe beitreten
a11y-speaker-join-decline = Diese Lautsprechergruppe ablehnen
a11y-speaker-playing-on = Wiedergabe auf { $group }
a11y-speaker-session-description = Wiedergabe in der Lautsprechergruppe; aktivieren zum Beenden
a11y-speaker-group-description = Lautsprechergruppe, { $summary }; wähle zuerst eine Quelle und aktiviere dann, um darauf abzuspielen
a11y-speaker-setup = Lautsprecher einrichten
a11y-speaker-setup-description = Öffnet die Einstellungen, um mehrere Geräte als Stereopaar oder Raumlautsprecher zu nutzen
a11y-speaker-membership = { $role } in { $group }
a11y-speaker-membership-description = Dieses Gerät spielt in dieser Lautsprechergruppe; aktivieren zum Austreten
a11y-remote-audio-play-here = { $media } hier abspielen
a11y-remote-audio-consent-description = Ein zertifiziertes Gerät hat eine Audioausgabe für eine Sitzung angefragt
a11y-remote-audio-allow = Dieses entfernte Audio einmal erlauben
a11y-remote-audio-decline = Dieses entfernte Audio ablehnen
a11y-remote-audio-source-app = Audio der fokussierten App wählen; die Weiterleitung endet, wenn der Fokus wechselt
a11y-remote-audio-source-system = Gesamtes Systemaudio wählen, dann ein zertifiziertes Ziel wählen
a11y-remote-audio-route-interrupted = Entfernte Audioweiterleitung unterbrochen; aktivieren zum Verwerfen
a11y-remote-audio-route-active = Entfernte Audioweiterleitung aktiv oder wartet auf Freigabe; aktivieren zum Beenden
a11y-remote-capture-share = { $input } dieses Geräts mit { $app } teilen
a11y-remote-capture-consent-description = Ein zertifiziertes Gerät hat eine private Aufnahmesitzung angefragt
a11y-remote-capture-allow = Diese entfernte Eingabe einmal erlauben
a11y-remote-capture-decline = Diese entfernte Eingabe ablehnen
a11y-remote-capture-source = { $input } von einem anderen Gerät verwenden
a11y-remote-capture-source-description = Wähle ein zertifiziertes Quellgerät; es muss die Anfrage freigeben
a11y-remote-capture-session = Entfernte Eingabe: { $input }
a11y-remote-capture-requesting = Warte auf die Freigabe durch das Quellgerät
a11y-remote-capture-provisioning = Privater lokaler Endpunkt wird vorbereitet
a11y-remote-capture-app-approval = Die lokale App muss freigegeben werden
a11y-remote-capture-waiting-receiver = Warte auf das empfangende Gerät
a11y-remote-capture-streaming = Private Sitzung aktiv; aktivieren zum Beenden
a11y-remote-capture-unavailable = Entfernte Eingabe nicht verfügbar: { $input }
a11y-remote-capture-source-unavailable = Das Quellgerät kann diese Eingabe nicht bereitstellen
a11y-remote-capture-receiver-unavailable = Dieses Gerät kann den privaten Empfänger nicht erstellen
a11y-remote-capture-interrupted = Die zertifizierte Verbindung wurde unterbrochen und geschlossen
a11y-mirror-from = Bildschirm von { $source } spiegeln
a11y-mirror-consent-description = Ein zertifiziertes Gerät hat eine private Bildschirmspiegelung nur zum Ansehen angefragt
a11y-mirror-allow = Bildschirmspiegelung einmal erlauben
a11y-mirror-decline = Bildschirmspiegelung ablehnen
a11y-mirror-source = Diesen Bildschirm spiegeln
a11y-mirror-source-description = Wähle einen zertifizierten Videobildschirm; das Ziel muss einmal zustimmen und erhält keine Eingaberechte
a11y-mirror-with = Bildschirmspiegelung mit { $peer }
a11y-mirror-awaiting = Warte auf Freigabe durch das Ziel; aktivieren zum Abbrechen
a11y-mirror-starting = Privater Stream nur zum Ansehen startet; aktivieren zum Beenden
a11y-mirror-streaming = Privater Stream nur zum Ansehen aktiv; aktivieren zum Beenden
a11y-mirror-failed = Bildschirmspiegelung beendet: { $reason }
a11y-mirror-stopped = Bildschirmspiegelung beendet
a11y-call-continue = Anruf in { $room } fortsetzen
a11y-call-consent-description = Diese eine Übergabe und die angefragten lokalen Aufnahmequellen freigeben
a11y-call-allow = Diese Anrufübergabe einmal erlauben
a11y-call-decline = Diese Anrufübergabe ablehnen
a11y-call-in = Anruf in { $room }
a11y-call-source-description = Wähle diesen laufenden Anruf und dann ein zertifiziertes Ziel
a11y-bluetooth-pair = { $accessory } koppeln
a11y-bluetooth-pair-description = Eine von { $source } angefragte Übergabe freigeben
a11y-bluetooth-pair-allow = Kopplung für diese Übergabe einmal erlauben
a11y-bluetooth-pair-decline = Diese Bluetooth-Übergabe ablehnen
a11y-bluetooth-handoff = { $accessory } mit { $peer }
a11y-handoff-preparing = Ziel wird authentifiziert und geprüft; aktivieren zum Abbrechen
a11y-handoff-awaiting-approval = Warte auf Freigabe am Ziel; aktivieren zum Abbrechen
a11y-handoff-releasing = Zubehör wird auf diesem Gerät freigegeben; aktivieren zum Abbrechen
a11y-handoff-pairing = Kopplung am Ziel; aktivieren zum Abbrechen
a11y-handoff-connecting = Verbindung am Ziel; aktivieren zum Abbrechen
a11y-handoff-confirming = Verbunden; warte auf authentifizierte Bestätigung; aktivieren zum Abbrechen
a11y-handoff-completed = Übergabe abgeschlossen
a11y-handoff-failed-retry = Übergabe fehlgeschlagen: { $reason }; aktivieren zum Wiederholen
a11y-handoff-failed = Übergabe fehlgeschlagen: { $reason }
a11y-handoff-cancelled-retry = Übergabe abgebrochen; aktivieren zum Wiederholen
a11y-handoff-cancelled = Übergabe abgebrochen
a11y-controller-use = { $controller } verwenden
a11y-controller-consent-description = Eine typisierte Controller-Sitzung von { $source } für { $app } freigeben
a11y-controller-allow = Diesen Controller einmal erlauben
a11y-controller-decline = Diesen Controller ablehnen
a11y-controller-forwarding = Controller-Weiterleitung aktiv oder wartet auf Freigabe; aktivieren zum Beenden
a11y-controller-local = Wähle diesen lokalen Controller und dann ein zertifiziertes Ziel
a11y-media-handoff = { $media } auf { $target }
a11y-media-handoff-pending = Warte, bis das Ziel öffnet; aktivieren zum Abbrechen
a11y-media-handoff-completed = Auf dem Ziel geöffnet
a11y-media-playing = Läuft gerade; dieses Medium zum Weiterleiten wählen
a11y-media-paused = Pausiert; dieses Medium zum Weiterleiten wählen
a11y-output-current = Aktueller lokaler Audioausgang
a11y-output-route = Aktuelles und künftiges lokales Audio hierher leiten
a11y-input-lower = Eingangspegel senken
a11y-input-raise = Eingangspegel erhöhen
a11y-tethering-with = Tethering mit { $peer }
a11y-tethering-consent = Ein zertifiziertes Gerät hat eine kurzlebige Tethering-Freigabe angefragt
a11y-tethering-active = Tethering ist aktiv; aktivieren zum Trennen
a11y-tethering-status = Tethering-Status
a11y-tethering-allow = Tethering einmal erlauben
a11y-tethering-decline = Tethering ablehnen
a11y-place-left = Bildschirm links anordnen
a11y-place-right = Bildschirm rechts anordnen
a11y-place-above = Bildschirm oberhalb anordnen
a11y-place-below = Bildschirm unterhalb anordnen
a11y-control-revoke = Steuerung dieses Geräts entziehen
a11y-control-allow = Diesem Gerät 15 Minuten lang die Steuerung dieses Geräts erlauben

# Eingebettete Berechtigungsabfrage: was eine App anfragt
capability-filesystem-read = Lesezugriff auf { $path }
capability-filesystem-read-write = Lese- und Schreibzugriff auf { $path }
capability-camera = Kamerazugriff
capability-microphone = Mikrofonzugriff
capability-screencast = Bildschirmaufnahme
capability-remote-desktop = Fernsteuerung (Zeiger + Tastatur)
capability-print = Dokumente an einen Drucker senden
capability-network-hosts = Netzwerkzugriff auf { $hosts }
capability-network = Netzwerkzugriff

# Die Hilfe-Pille in der Bildschirmecke
help-pill = Hilfe  F1 · Super+?

# Arbeitsbereichsübersicht
overview-go-to = Wechseln zu
overview-no-room = Kein Raum ausgewählt
overview-you-are-here = Du bist hier
overview-shared-out = Von hier aus geteilt
overview-nothing-shared = Aus diesem Arbeitsbereich ist nichts nach außen geteilt
overview-more-shares = und { $count } weitere
overview-share-with-two = { $first } und { $second }
overview-share-with-many = { $count ->
        [one] { $first } und { $count } weitere Person
       *[other] { $first } und { $count } weitere Personen
    }
overview-share-can-edit = { $with } · darf bearbeiten
overview-new-workspace = Neuer Arbeitsbereich

# Der Ansicht einer Person folgen
follow-label = Du folgst { $person }
follow-detail = Deine Ansicht · alle hier sehen das
follow-stop = Beenden

# Filterchips der Suche
search-filter-all = Alle
search-filter-more = Mehr

# Schnelleinstellungen: Zeilen für Geräte und Medien
qs-devices-mic-other = Mikrofon · von hier aufnehmen
qs-devices-mic-current = Aktuelles Mikrofon
qs-devices-mic-level = { " " }· Eingangspegel { $level } %
qs-devices-mic-muted = { " " }· stumm
qs-devices-this-device = Dieses Gerät · identitätszertifiziert
qs-devices-offline = Offline · identitätszertifiziert
qs-devices-sending = Sichere Übertragung · { $percent } % · fortsetzbar
qs-devices-sent = Sicher übertragen
qs-devices-transfer-failed = Übertragung fehlgeschlagen · Quelle bleibt verfügbar
qs-devices-mutual-control = Gegenseitige Steuerung aktiv · lokale Eingaben haben immer Vorrang
qs-devices-may-control = Darf dieses Gerät steuern · lokale Eingaben haben immer Vorrang
qs-devices-control-granted = Steuerung erlaubt · neben diesem Bildschirm anordnen, um hinüberzuwechseln
qs-devices-play-here = { $media } hier abspielen · direkte verschlüsselte Übergabe
qs-devices-audio-only = Online · bei diesem Video nur Audio
qs-devices-no-output = Online · keine Medienausgabe
qs-devices-unavailable-media = Online · für dieses Medium nicht verfügbar
qs-devices-online = Online · identitätszertifiziert
qs-devices-health = { $reachability ->
        [online] erreichbar
        [stale] Präsenz veraltet
       *[offline] offline
    } · { $latency } · { $power }{ $saver ->
        [yes] { " " }· Energiesparmodus
       *[no] {""}
    } · { $routes } aktive Verbindungen
qs-devices-latency-unknown = Latenz unbekannt
qs-devices-latency = { $ms } ms
qs-devices-battery-unknown = Akku unbekannt
qs-devices-external-power = Netzbetrieb
qs-devices-charging = lädt { $percent } %
qs-devices-battery = Akku { $percent } %

# Schnelleinstellungen: Bluetooth-Zubehör verschieben
qs-bluetooth-move-note = Das Ziel muss bereits selbst gekoppelt sein · Kopplungsschlüssel verlassen nie ein Gerät
qs-devices-fabric-unavailable = Das persönliche Gerätenetz ist nicht verfügbar
qs-bluetooth-this-device = Dieses Gerät
qs-bluetooth-disconnect = { $accessory } trennen, ohne es zu vergessen
qs-bluetooth-this-certified = Dieses zertifizierte Gerät
qs-bluetooth-move-here = Hierher verschieben, sobald die lokale Kopplung bestätigt ist
qs-devices-showing = { $first }–{ $last } von { $total } · scrollen für mehr

# Schnelleinstellungen: Bereich Geräte und Medien
qs-devices-header = Geräte & Medien
qs-devices-no-identity = Richte deine persönliche Identität ein, um Geräte zu verbinden
qs-devices-note = Direkt, wenn erreichbar · keine Hersteller-Cloud · nur zertifizierte Geräte
qs-devices-empty = Verbundene Ausgänge und registrierte Geräte erscheinen hier
qs-devices-place = { $display } { $side ->
        [left] links
        [right] rechts
        [above] oben
       *[below] unten
    } anordnen
qs-devices-place-note = Lokale Sensordaten · vor dem Übernehmen prüfen
qs-devices-placement-undo-note = Genaue Anordnung von vor der Kalibrierung wiederherstellen
qs-devices-speaker-join = { $group } als { $role } beitreten?
qs-devices-speaker-join-note = { $leader } hat gefragt · einmalige Frage, jederzeit austreten
qs-devices-speaker-join-note-output = { $leader } hat gefragt · spielt auf { $output } · einmalige Frage, jederzeit austreten
qs-devices-speaker-for = für { $peer } · auswählen zum Beenden
qs-devices-another-device = ein anderes Gerät
qs-devices-speaker-connecting = { $playing } spielen, { $waiting } verbinden sich · auswählen zum Beenden
qs-devices-speakers-playing = { $count ->
        [one] { $count } Lautsprecher spielt · auswählen zum Beenden
       *[other] { $count } Lautsprecher spielen · auswählen zum Beenden
    }
qs-devices-speaker-syncing = { $playing } spielen, { $syncing } gleichen die Uhr ab… · auswählen zum Beenden
qs-devices-speaker-clock-failed = { $count ->
        [one] { $count } Lautsprecher kann nicht synchron spielen: { $reason } · auswählen zum Beenden
       *[other] { $count } Lautsprecher können nicht synchron spielen: { $reason } · auswählen zum Beenden
    }
qs-devices-speaker-setup-note = Stereopaar oder Raumlautsprecher · öffnet Einstellungen → Lautsprecher
qs-devices-speaker-membership-note = eingerichtet auf { $leader } · auswählen zum Austreten
qs-devices-speaker-listen = { $leader } dieses Mikrofon für { $group } verwenden lassen?
qs-devices-speaker-listen-note = nur während der Lautsprecherkalibrierung · die Aufnahme bleibt hier
qs-devices-speaker-listen-note-microphone = verwendet { $microphone } · nur während der Lautsprecherkalibrierung · die Aufnahme bleibt hier
qs-devices-speaker-listener-note = eingerichtet auf { $leader } · auswählen zum Beenden
qs-devices-another-certified = Ein anderes zertifiziertes Gerät
qs-devices-requested-allow-once = { $source } hat das angefragt · einmal erlauben oder ablehnen
qs-devices-audio-app = Audio der fokussierten App · endet, wenn der Fokus wechselt
qs-devices-audio-system = Gesamtes Systemaudio · unten ein zertifiziertes Ziel wählen
qs-devices-certified-device = zertifiziertes Gerät
qs-devices-audio-awaiting = wartet auf Freigabe mit { $peer } · auswählen zum Beenden
qs-devices-audio-streaming = streamt mit { $peer } · auswählen zum Beenden
qs-devices-audio-interrupted = unterbrochen mit { $peer } · auswählen zum Verwerfen
qs-devices-audio-syncing = gleicht die Uhr mit { $peer } ab… · auswählen zum Beenden
qs-devices-audio-clock-failed = kann mit { $peer } nicht synchron spielen: { $reason } · auswählen zum Beenden
qs-devices-capture-share = { $input } dieses Geräts teilen
qs-devices-capture-requested = { $source } hat das für { $app } angefragt · einmal erlauben oder ablehnen
qs-devices-capture-use-remote = Entfernte Eingabe verwenden: { $input }
qs-devices-capture-source-note = Unten ein zertifiziertes Quellgerät wählen · dort ist eine Freigabe nötig
qs-devices-capture-waiting-on = Warte auf Freigabe auf { $peer }
qs-devices-capture-allow-app = Der fokussierten App einmal erlauben
qs-devices-capture-waiting-ready = Warte, bis { $peer } bereit ist
qs-devices-capture-private = Private Sitzung mit { $peer } · auswählen zum Beenden
qs-devices-capture-source-unavailable = Die Quelle kann diese Eingabe nicht bereitstellen
qs-devices-capture-receiver-unavailable = Privater Empfänger konnte nicht erstellt werden
qs-devices-capture-interrupted = Zertifizierte Verbindung unterbrochen · Sitzung geschlossen
qs-devices-mirror-allow = Bildschirmspiegelung erlauben?
qs-devices-mirror-requested = { $source } hat einen privaten Stream nur zum Ansehen angefragt · einmal erlauben oder ablehnen
qs-devices-mirror-source-note = Unten einen zertifizierten Videobildschirm wählen · dort ist eine Freigabe nötig
qs-devices-mirror-awaiting = Warte auf Freigabe · auswählen zum Abbrechen
qs-devices-mirror-starting = Stream nur zum Ansehen startet · auswählen zum Beenden
qs-devices-mirror-streaming = Stream nur zum Ansehen aktiv · auswählen zum Beenden
qs-devices-mirror-with = Spiegelung · { $peer }
qs-devices-call-requested = { $source } hat das angefragt · auswählen, um einmal zu erlauben
qs-devices-call-source-note = Läuft hier · unten ein zertifiziertes Ziel wählen
qs-devices-bluetooth-requested = { $source } hat diese Übergabe angefragt · auswählen, um einmal freizugeben
qs-devices-handoff-checking = { $peer } wird geprüft · auswählen zum Abbrechen
qs-devices-handoff-awaiting = Warte auf Freigabe auf { $peer } · auswählen zum Abbrechen
qs-devices-handoff-releasing = Wird hier freigegeben · auswählen zum Abbrechen
qs-devices-handoff-pairing = Kopplung auf { $peer } · auswählen zum Abbrechen
qs-devices-handoff-connecting = Verbindung auf { $peer } · auswählen zum Abbrechen
qs-devices-handoff-confirming = Verbunden · authentifizierte Übergabe wird bestätigt
qs-devices-handoff-connected = Jetzt mit { $peer } verbunden
qs-devices-handoff-failed-retry = { $reason } · auswählen zum Wiederholen
qs-devices-cancelled-retry = Abgebrochen · auswählen zum Wiederholen
qs-devices-cancelled = Abgebrochen
qs-devices-controller-requested = { $source } hat das für { $app } angefragt · einmal erlauben
qs-devices-controller-local = Lokaler Controller · unten ein zertifiziertes Ziel wählen
qs-devices-controller-forwarding = Weiterleitung an { $target } · auswählen zum Beenden
qs-devices-controller-awaiting = Warte auf Freigabe auf { $target } · auswählen zum Abbrechen
qs-devices-media-waiting = Warte auf das Ziel · auswählen zum Abbrechen
qs-devices-media-opened = Auf dem Ziel geöffnet
qs-devices-media-failed = Medium konnte nicht verschoben werden ({ $reason }) · auswählen zum Wiederholen
qs-devices-media-playing = Läuft gerade · unten ein Ziel wählen
qs-devices-media-paused = Pausiert · unten ein Ziel wählen
qs-devices-output-default = Lokaler Ausgang · aktueller Standard
qs-devices-output-route = Lokaler Ausgang · aktuelles Audio hierher leiten
qs-devices-output-bluetooth = Bluetooth { $codec } · aktuelles Audio hierher leiten
qs-devices-tether-requesting = Warte auf Freigabe am Telefon
qs-devices-tether-consent = Einmal erlauben oder ablehnen
qs-devices-tether-requester = Über dieses Telefon verbunden · auswählen zum Trennen
qs-devices-tether-provider = Verbindung dieses Telefons wird geteilt · auswählen zum Trennen
qs-devices-tether-denied = Am Telefon abgelehnt
qs-devices-tether-expired = Anfrage abgelaufen
qs-devices-tether-offline = Gerät ist offline · getrennt
qs-devices-tether-disabled = WLAN-Kontinuität wurde deaktiviert
qs-devices-tether-failed = Tethering konnte nicht hergestellt werden
qs-devices-tether-disconnected = Getrennt
qs-devices-tether-with = Tethering · { $peer }
qs-devices-ask-phone = Dieses Telefon bitten, seine Verbindung zu teilen · dort ist eine Freigabe nötig
qs-devices-use-controller = Gewählten Controller hier verwenden · das Ziel gibt einmal frei
qs-devices-mirror-here = Diesen Bildschirm hier spiegeln · nur ansehen · das Ziel gibt einmal frei
qs-devices-no-video = Dieses Gerät kann kein Video empfangen
qs-devices-use-input = { $input } dieses Geräts verwenden · die Quelle gibt einmal frei
qs-devices-play-audio-here = Gewähltes Audio hier abspielen · das Ziel gibt einmal frei
qs-devices-no-audio = Dieses Gerät hat keinen Audioausgang
qs-devices-place-this-left = Diesen Bildschirm links anordnen
qs-devices-place-this-right = Diesen Bildschirm rechts anordnen
qs-devices-place-this-above = Diesen Bildschirm oberhalb anordnen
qs-devices-place-this-below = Diesen Bildschirm unterhalb anordnen

# Schnelleinstellungen: Verlauf der Zwischenablage
qs-clipboard-search-empty = Zwischenablage durchsuchen … · tippen zum Filtern
qs-clipboard-search-matches = { $count ->
        [one] Suche: { $query } · { $count } Treffer
       *[other] Suche: { $query } · { $count } Treffer
    }
qs-clipboard-empty-hint = Kopierte Einträge erscheinen hier
qs-clipboard-no-matches = Keine Treffer für „{ $query }“
qs-clipboard-search-range = Zwischenablage durchsuchen … · { $first }–{ $last } von { $total }
qs-clipboard-search-count = { $count ->
        [one] Zwischenablage durchsuchen … · { $count } Eintrag
       *[other] Zwischenablage durchsuchen … · { $count } Einträge
    }
qs-clipboard-clear = Leeren
qs-clipboard-clear-a11y = Nicht angeheftete Einträge der Zwischenablage löschen
qs-clipboard-use-a11y = Eintrag der Zwischenablage verwenden
qs-clipboard-mime-bytes = { $mime } · { $bytes } Byte
qs-clipboard-row-detail = { $source } · { $age } · { $bytes } B
qs-clipboard-remove-a11y = Eintrag der Zwischenablage entfernen

# Schnelleinstellungen: einem geschützten WLAN beitreten
qs-wifi-protected = Dieses Netzwerk ist geschützt

# Aufbewahrter Verlauf: Quellen und Schweregrade
history-this-device = Dieses Gerät

# On-screen display for the hardware keys
osd-play = Wiedergabe
osd-pause = Pause
osd-next-track = Nächster Titel
osd-previous-track = Vorheriger Titel
osd-stop = Stopp
osd-muted = Stummgeschaltet
osd-volume = Lautstärke { $percent } %
osd-brightness = Helligkeit { $percent } %
osd-microphone-muted = Mikrofon stummgeschaltet
osd-microphone-on = Mikrofon an
osd-keyboard-backlight-off = Tastaturbeleuchtung aus
osd-keyboard-backlight = Tastaturbeleuchtung { $percent } %
osd-no-backlight = Kein regelbarer Bildschirm
osd-airplane-mode-on = Flugmodus an
osd-airplane-mode-off = Flugmodus aus
osd-touchpad-on = Touchpad an
osd-touchpad-off = Touchpad aus
osd-magnifier-off = Bildschirmlupe aus
osd-magnifier = Bildschirmlupe { $zoom }
osd-pointer-held = Zeiger von App gehalten · { $chord } gibt ihn frei
osd-pointer-released = Zeiger freigegeben
osd-mouse-keys-on = Maustasten an
osd-mouse-keys-off = Maustasten aus

# System status, the tray's accessible summary
status-wired = Kabelgebunden
status-wired-named = Kabelgebunden: { $name }
status-wifi = WLAN
status-wifi-named = WLAN: { $name }
status-offline = Offline
status-muted = Stummgeschaltet
status-volume = Lautstärke { $percent } %
status-battery-charging = Akku { $percent } %, wird geladen
status-battery = Akku { $percent } %
status-summary-empty = Systemstatus
status-other-displays-a11y = Weitere Bildschirme

# Notification toasts
notification-source-caption = { $app } · Von { $device }
notification-app-host-apps = Host-Apps
notification-app-power = Energie
notification-remote-input-stopped = Fernsteuerung beendet
notification-remote-input-stopped-sessions = Fernsteuerungs- und Freigabesitzungen wurden beendet.
notification-remote-input-stopped-shares = { $count ->
    [one] Die Fernsteuerung wurde entzogen und die Freigabe beendet.
   *[other] Die Fernsteuerung wurde { $count } Freigaben entzogen; die Freigabe wurde beendet.
}

# Media codec pack notices
codec-hardware-ready = Hardware-H.264/HEVC ist bereit
codec-installed = Mediencodecs installiert
codec-installed-mesa-waiting = { $applies } Hardware-H.264/HEVC folgt, sobald RPM Fusion einen zu diesem System passenden Mesa-Build veröffentlicht.
codec-failed = Mediencodecs konnten nicht installiert werden
codec-failed-retry = Öffne Einstellungen, Updates, um es erneut zu versuchen.
codec-failed-with-error = { $error } Öffne Einstellungen, Updates, um es erneut zu versuchen.
codec-app-name = Mediencodecs

# Screen capture surfaces
capture-live-alt = Live-Aufnahme: { $title }
capture-pinned-alt = Angehefteter Screenshot: { $title }
capture-pinned-preview-alt = Vorschau des angehefteten Screenshots: { $title }
capture-live-selection = Live-Auswahl
capture-live-app = Live-App
capture-live-screen = Live-Bildschirm
capture-live-areas = { $count ->
    [one] { $count } Live-Bereich
   *[other] { $count } Live-Bereiche
}
capture-pinned-title = Screenshot · { $width } × { $height }

# What a drag is carrying
drag-image = Bild
drag-video = Video
drag-audio = Audio
drag-pdf = PDF-Dokument
drag-text = Text
drag-items = { $count ->
    [one] 1 Element
   *[other] { $count } Elemente
}

# App-Startfehler, als Benachrichtigungen
launch-error-provider-unavailable = Der benötigte Dienst zum Ausführen von Apps ist nicht verfügbar. Versuche es erneut, sobald er verfügbar ist.
launch-error-not-installed = Diese App ist nicht mehr installiert. Aktualisiere die App-Liste und versuche es erneut.
launch-error-not-gui-app = Diese App hat kein Fenster.
launch-error-not-managed-service = Dieser Dienst kann nicht als App geöffnet werden.
launch-error-not-authorized = Du darfst diesen Arbeitsbereichsdienst nicht starten.
launch-error-launcher-unavailable = Der App-Starter ist nicht verfügbar. Versuche es erneut.
launch-error-no-display = Der Desktop startet noch. Versuche es gleich noch einmal.
launch-error-no-workspace = Wähle einen Arbeitsbereich und versuche es dann erneut.
launch-error-shell-owned-intent = Öffne dieses Ziel über die Desktop-Steuerelemente.
launch-error-identity-unavailable = Die Identität des Arbeitsbereichs konnte nicht geladen werden. Versuche es erneut.
launch-error-registry-unreadable = Die App-Liste konnte nicht gelesen werden. Versuche es erneut.
launch-error-host-app-refused = Der Desktop-Eintrag dieser App kann nicht gestartet werden.
launch-failed-launcher-unavailable = Der App-Starter ist nicht verfügbar. Öffne „Anwendungen“ und versuche es erneut.
launch-failed-stopped = Die App wurde beendet, bevor sie sich öffnete. Öffne „Anwendungen“ und versuche es erneut.
launch-failed-missing-library = Der App fehlt eine Bibliothek auf diesem System, daher wurde sie beendet, bevor sie sich öffnete.
launch-failed-sandbox = Die Sandbox der App konnte nicht eingerichtet werden, daher wurde sie beendet, bevor sie sich öffnete.
launch-failed-crashed = Die App ist abgestürzt, bevor sie sich öffnete. Öffne „Anwendungen“ und versuche es erneut.
launch-unnamed-app = Anwendung
launch-failure-title = { $app } konnte nicht geöffnet werden

# Tab- und Fensteraktionen, die nicht ausgeführt werden konnten
window-duplicate-no-tab = Es gibt keinen Tab zum Duplizieren.
window-duplicate-foreign = Dieses Fenster gehört zu einer App, die der Compositor nicht hostet, daher kann es nicht kopiert werden. Öffne stattdessen über die Suche ein zweites.
window-tab-action-no-tab = Es gibt keinen Tab für diese Aktion.
window-tab-action-unknown-tab = Dieser Tab ist nicht mehr geöffnet.
window-tab-action-only-tab = Das ist der einzige Tab dieses Fensters; er kann nicht abgetrennt werden.
window-tab-action-document-refused = Das Arbeitsbereichsdokument hat die Änderung nicht angenommen, daher wurde nichts verschoben.

# Tab-Kontextmenü, Arbeitsbereichsmeldungen und Platzhalterkacheln
shell-workspace-renamed = Arbeitsbereich umbenannt
shell-tab-menu-close = Tab schließen
shell-tab-menu-split-right = Rechts abtrennen
shell-tab-menu-new-group = In neue Gruppe verschieben
shell-terminal-unavailable = Terminal: { $error }
shell-app-unavailable = Anwendung nicht verfügbar
shell-activity-title = Aktivität
shell-activity-loading = Aktivität im Arbeitsbereich wird geladen …

# Sitzungsmenü
power-menu-title = Sitzung
power-menu-description-switch-user = Diesen Computer sperren, abmelden, Benutzer wechseln, in Bereitschaft versetzen, neu starten oder ausschalten
power-menu-description = Diesen Computer sperren, abmelden, in Bereitschaft versetzen, neu starten oder ausschalten
power-lock = Sperren
power-lock-detail = Apps laufen weiter
power-logout = Abmelden
power-logout-detail = Diese Sitzung beenden
power-switch-user = Benutzer wechseln
power-switch-user-detail = Diese Sitzung läuft weiter
power-suspend = Bereitschaft
power-suspend-detail = Dort weitermachen, wo du aufgehört hast
power-restart = Neu starten
power-restart-detail = Den Computer neu starten
power-shut-down = Ausschalten
power-shut-down-detail = Den Computer ausschalten

# Sperrbildschirm
lock-screen-a11y-name = Tilo-Sperrbildschirm
lock-screen-locked = Sitzung gesperrt
lock-screen-locked-at = Sitzung gesperrt, { $clock }
lock-screen-locked-at-date = Sitzung gesperrt, { $clock }, { $date }
lock-screen-password = Passwort
lock-screen-help = Gib dein Passwort ein und drücke die Eingabetaste
lock-screen-verifying-password = Passwort wird geprüft
lock-screen-prompt-elsewhere = Die Passwortabfrage ist auf dem Hauptbildschirm
lock-screen-enter-password = Passwort eingeben
lock-screen-verifying = Wird geprüft …
lock-screen-press-enter = Zum Entsperren Eingabetaste drücken
lock-screen-cannot-verify = Prüfung nicht möglich · Gib dein Passwort erneut ein
lock-screen-sign-in-failed = Anmeldung fehlgeschlagen · Gib dein Passwort erneut ein

# Bluetooth-Kopplungsdialog
bluetooth-prompt-title-confirm-passkey = Zeigt { $device } diesen Code an?
bluetooth-prompt-title-display = Gib das auf { $device } ein
bluetooth-prompt-title-request-passkey = Gib den auf { $device } angezeigten Code ein
bluetooth-prompt-title-request-pin = Gib die PIN für { $device } ein
bluetooth-prompt-title-confirm-pairing = Mit { $device } koppeln?
bluetooth-prompt-title-authorize-service = { $device } verbinden lassen?
bluetooth-prompt-explain-confirm-passkey = Vergleiche ihn mit dem Code auf dem anderen Gerät. Wenn sie sich unterscheiden, versucht etwas anderes, sich zu koppeln – lehne dann ab.
bluetooth-prompt-explain-display = Gib diesen Code auf dem anderen Gerät ein, um die Kopplung abzuschließen.
bluetooth-prompt-explain-request = Der Code wird auf dem Gerät angezeigt, das du koppelst. Viele Tastaturen verlangen, dass du ihn eintippst und die Eingabetaste drückst.
bluetooth-prompt-explain-confirm-pairing = Dieses Gerät kann keinen Code zum Vergleichen anzeigen, es gibt also nichts zu prüfen. Fahre nur fort, wenn du diese Kopplung selbst gestartet hast.
bluetooth-prompt-explain-authorize-service = Ein bereits gekoppeltes Gerät möchte einen neuen Dienst nutzen. Gekoppelte Geräte können Eingaben und Audio an diesen Computer senden.
bluetooth-prompt-digits-entered = { $entered } von 6 Ziffern eingegeben
bluetooth-prompt-service = Dienst { $uuid }
bluetooth-prompt-pin-placeholder = PIN
bluetooth-prompt-code-placeholder = 6-stelliger Code
bluetooth-prompt-reject = Nicht koppeln
bluetooth-prompt-cancel = Abbrechen
bluetooth-prompt-accept-match = Sie stimmen überein
bluetooth-prompt-accept-allow = Erlauben
bluetooth-prompt-accept-pair = Koppeln
bluetooth-prompt-code-label = Auf dem Gerät angezeigter Code
bluetooth-prompt-pin-label = PIN für das Gerät
bluetooth-prompt-a11y-code = Code { $code }

# Portal-Voreinstellungen: Dateiauswahl-Schaltflächen, Standardaktion von Benachrichtigungen, unbenannte Apps, Aufnahmequellen
portal-file-save = Speichern
portal-file-choose-folder = Ordner auswählen
portal-file-open = Öffnen
portal-notification-open = Öffnen
portal-inhibit-unnamed-app = Eine App
portal-capture-default-microphone = Standardmikrofon
portal-capture-default-camera = Standardkamera
portal-capture-primary-display = Hauptbildschirm
portal-capture-system-audio = Systemaudio

# Die Rolle eines Lautsprechers in einer Lautsprechergruppe, als $role eines Satzes („Wohnzimmer als rechter Lautsprecher beitreten“)
speaker-role-left = linker Lautsprecher
speaker-role-right = rechter Lautsprecher
speaker-role-centre = Mittellautsprecher
speaker-role-subwoofer = Subwoofer
speaker-role-surround-left = linker Surround-Lautsprecher
speaker-role-surround-right = rechter Surround-Lautsprecher
speaker-role-room = Raumlautsprecher
speaker-group-summary = { $total ->
    [one] { $ready } von 1 Lautsprecher bereit
   *[other] { $ready } von { $total } Lautsprechern bereit
}
speaker-group-summary-waiting = { $total ->
    [one] { $ready } von 1 Lautsprecher bereit · Freigabe ausstehend: { $waiting }
   *[other] { $ready } von { $total } Lautsprechern bereit · Freigabe ausstehend: { $waiting }
}

# Eine entfernte Eingabe, als $input eines Satzes
remote-input-camera = Kamera
remote-input-microphone = Mikrofon
remote-input-scanner = Scanner

# Warum die Bildschirmspiegelung endete, als $reason eines Satzes
mirror-failure-denied = auf dem Zielgerät abgelehnt
mirror-failure-peer-offline = das zertifizierte Gerät ist offline gegangen
mirror-failure-timed-out = Zeitüberschreitung der Sitzung
mirror-failure-transport = die private Videoverbindung ist fehlgeschlagen
mirror-failure-policy = die Bildschirmübergabe wurde deaktiviert

# Warum das Verschieben von Medien auf ein anderes Gerät fehlschlug, als $reason eines Satzes
media-handoff-failure-destination-unavailable = Ziel nicht verfügbar
media-handoff-failure-unsupported-output = das Ziel kann dieses Medium nicht abspielen
media-handoff-failure-invalid-workspace = der Arbeitsbereich ist dort nicht verfügbar
media-handoff-failure-open-failed = das Ziel konnte es nicht öffnen
media-handoff-failure-timed-out = das Ziel hat nicht geantwortet
media-handoff-failure-cancelled = abgebrochen

# Warum das Verschieben eines Bluetooth-Zubehörs fehlschlug, als $reason eines Satzes
bluetooth-handoff-failure-inspect-target = Prüfung des Zielgeräts fehlgeschlagen
bluetooth-handoff-failure-target-unavailable = Zielgerät nicht verfügbar oder abgelehnt
bluetooth-handoff-failure-disconnect-source = die Quelle konnte das Zubehör nicht freigeben
bluetooth-handoff-failure-pair-target = Kopplung mit dem Zielgerät fehlgeschlagen
bluetooth-handoff-failure-connect-target = Verbindung zum Zielgerät fehlgeschlagen
bluetooth-handoff-failure-send = die zertifizierte Verbindung wurde unterbrochen
bluetooth-handoff-failure-timed-out = Zeitüberschreitung des Vorgangs
bluetooth-handoff-failure-rollback = Zurücksetzen der Quelle fehlgeschlagen

# Arten von Suchergebnissen, wie Filterchips und Gruppenüberschriften sie nennen
search-kind-actions = Aktionen
search-kind-applications = Anwendungen
search-kind-host-apps = Host-Apps
search-kind-notes = Notizen
search-kind-records = Einträge
search-kind-files = Dateien
search-kind-folders = Ordner
search-kind-creators = Kanäle
search-kind-videos = Videos
search-kind-playlists = Playlists
search-kind-tabs = Tabs
search-kind-window-groups = Fenstergruppen

# Suchbereiche
search-scope-this-app = Diese App
search-scope-workspace = Arbeitsbereich
search-scope-everywhere = Überall

# Wo eine gestartete App geöffnet wird
search-opens-beside = Öffnet sich ganz rechts
search-opens-here = Öffnet sich hier als Tab

# Suchquellen, wie die Aktionen der Sucheinstellungen sie nennen
search-provider-now-playing = Aktuelle Wiedergabe
search-provider-records = Einträge
search-provider-notes = Notizen
search-provider-editor = Editor
search-provider-session = Sitzung
search-provider-shortcuts = Kurzbefehle
search-provider-tray = Statusbereich
search-provider-files = Dateien
search-provider-host-apps = Host-Apps
search-provider-app-actions = Ressourcen und Aktionen von Anwendungen

# Kennzeichen von Suchergebnissen
search-badge-media = Medien
search-badge-session = Sitzung
search-badge-quick-action = Schnellaktion
search-badge-shortcut = Kurzbefehl
search-badge-tray = Statusbereich

# Suche: Wiedergabesteuerung eines laufenden Players
search-media-pause = { $player } pausieren
search-media-play = { $player } abspielen
search-media-next = Nächster Titel in { $player }
search-media-previous = Vorheriger Titel in { $player }
search-media-seek = In { $player } springen

# Suche: Einträge
search-record-file-missing = Die Datei, auf die dieser Eintrag verweist, ist nicht mehr vorhanden.
search-record-in-this-workspace = In diesem Arbeitsbereich
search-record-in-workspace = In { $workspace }
search-record-in-another-workspace = In einem anderen Arbeitsbereich

# Suche: Dateien
search-files-no-app = Keine App auf diesem Desktop öffnet .{ $ext }-Dokumente.

# Suche: Host-Apps
search-host-app-open = { $app } öffnen
search-host-app-subtitle = { $source } · nicht isolierte Host-App · { $id }

# Suche: Aufträge und Ressourcen von App-Aktionen
search-app-job = Aktionsauftrag { $id }
search-app-job-cancel = Aktionsauftrag { $id } abbrechen
search-app-job-cancel-detail = Bittet die App, den Auftrag abzubrechen
search-app-job-accepted = angenommen
search-app-job-running = läuft
search-app-job-streaming = liefert Ergebnisse
search-app-job-succeeded = abgeschlossen
search-app-job-failed = fehlgeschlagen
search-app-job-cancelled = abgebrochen
search-app-resource-contains = { $app } · enthält Ressourcen
search-app-action-launches = { $resource } · { $app } · startet die Anwendung

# Suche: Sitzungsaktionen
search-session-logout = Von Tilo abmelden
search-session-logout-detail = Diese Desktop-Sitzung sauber beenden und zur Anmeldung zurückkehren
search-session-unsaved-work = Eine App hat ungesicherte Arbeit
search-session-inhibited = { $reason } – schließe die App oder beende zuerst ihre Arbeit.
search-session-not-offered = Diese Sitzung teilt den Computer mit einem anderen Desktop, der seine Energieverwaltung steuert.

# Suche: Kurzbefehle
search-shortcut-run-on = Auf { $resource } anwenden

# Suche: Elemente im Statusbereich
search-tray-open = { $item } öffnen
search-tray-in-top-bar = { $item } in der oberen Leiste
search-tray-entry = { $item }: { $entry }
search-tray-entry-on = { $item }: { $entry } (an)
search-tray-entry-off = { $item }: { $entry } (aus)

# Suche: Fenster auf dem Desktop

# Suche: Shell-Aktionen als Palettenzeilen
search-action-shortcut-help-toggle = Desktop-Hilfe anzeigen
search-action-shortcut-help-toggle-subtitle = F1 öffnet sie überall; Super+/ ist das anpassbare Tastenkürzel für dieselbe Anleitung
search-action-desktop-tour-open = Desktop-Tour starten
search-action-desktop-tour-open-subtitle = Die dreiminütige Einführung in Navigation, Organisation und Zusammenarbeit noch einmal ansehen
search-action-overview-toggle = Arbeitsbereichsübersicht ein- oder ausblenden
search-action-overview-toggle-subtitle = Alle Arbeitsbereiche in der Radansicht zeigen
search-action-settings-open = Arbeitsbereichseinstellungen öffnen
search-action-settings-open-subtitle = Identität, Personen, Datenschutz, Apps und Dateien dieses Arbeitsbereichs
search-action-settings-members-open = Personen im Arbeitsbereich öffnen
search-action-settings-members-open-subtitle = Personen, Rollen und Einladungen in diesem Arbeitsbereich
search-action-settings-privacy-open = Datenschutz und Sicherheit des Arbeitsbereichs öffnen
search-action-settings-privacy-open-subtitle = App-Freigaben und durchgesetzte Zugriffe in diesem Arbeitsbereich
search-action-settings-apps-open = Apps und Dateien des Arbeitsbereichs öffnen
search-action-settings-apps-open-subtitle = Gemeinsame App-Standards und deine Anpassungen für diesen Arbeitsbereich
search-action-identity-devices-copy = Liste verbundener Geräte kopieren
search-action-identity-devices-copy-subtitle = Gerätenamen, Fingerabdrücke, Fähigkeiten und Widerrufsstatus kopieren
search-action-identity-device-enroll = Weiteres Gerät hinzufügen
search-action-identity-device-enroll-subtitle = Einen Einrichtungscode für ein Gerät kopieren, der fünf Minuten gültig ist
search-action-identity-home-peer-enroll = Heimserver koppeln
search-action-identity-home-peer-enroll-subtitle = Einen fünf Minuten gültigen Einrichtungscode mit ständigem Vollsynchronisations-Zugriff kopieren
search-action-quick-settings-open = Schnelleinstellungen öffnen
search-action-quick-settings-open-subtitle = Helligkeit, Lautstärke, WLAN, Bluetooth, Energieprofil
search-action-quick-settings-clipboard = Zwischenablage-Verlauf öffnen
search-action-quick-settings-clipboard-subtitle = Private Einträge der Zwischenablage durchsuchen, anheften, entfernen oder wiederverwenden
search-action-quick-settings-devices = Geräte und Medien öffnen
search-action-quick-settings-devices-subtitle = Zertifizierte Geräte, Medienwege, entfernte Eingabe und Übergaben
search-action-projection-open = Wählen, wo der Desktop erscheint
search-action-projection-open-subtitle = Erweitern, spiegeln, ein Bildschirm oder ein Miracast-, Google-Cast-, AirPlay- oder FCast-Empfänger
search-action-display-layout-extend = Bildschirme erweitern
search-action-display-layout-extend-subtitle = Alle verbundenen Bildschirme als einen größeren Desktop nutzen
search-action-display-layout-mirror = Bildschirme spiegeln
search-action-display-layout-mirror-subtitle = Auf allen verbundenen Bildschirmen dasselbe Bild zeigen
search-action-display-layout-builtin-only = Nur auf diesem Bildschirm anzeigen
search-action-display-layout-builtin-only-subtitle = Externe Bildschirme ausschalten und den eingebauten Bildschirm behalten
search-action-display-layout-external-only = Nur auf externen Bildschirmen anzeigen
search-action-display-layout-external-only-subtitle = Den eingebauten Bildschirm ausschalten und die externen Bildschirme nutzen
search-action-display-change-keep = Bildschirmeinstellungen behalten
search-action-display-change-keep-subtitle = Die gerade angewendete Bildschirmanordnung bestätigen
search-action-display-change-revert = Bildschirmeinstellungen zurücksetzen
search-action-display-change-revert-subtitle = Die Bildschirmanordnung von vor der letzten Änderung wiederherstellen
search-action-bluetooth-pairing-decline = Bluetooth-Kopplung ablehnen
search-action-bluetooth-pairing-decline-subtitle = Die angezeigte Kopplungsanfrage ablehnen oder die Kopplung beenden, deren Code angezeigt wird
search-action-capture-open = Bildschirmbereich aufnehmen oder schweben lassen
search-action-capture-open-subtitle = Bildschirmfoto, Aufnahme oder Bild-im-Bild aus einem oder mehreren Bereichen
search-action-collaborators-open = Mitwirkende anzeigen
search-action-collaborators-open-subtitle = Mitwirkende, die gerade in diesem Arbeitsbereich sind
search-action-presence-follow-stop = Nicht mehr folgen
search-action-presence-follow-stop-subtitle = Diese Ansicht wieder lokal navigieren
search-action-room-activity-open = Aktivität im Arbeitsbereich anzeigen
search-action-room-activity-open-subtitle = Verlauf dieses Arbeitsbereichs: Änderungen, Berechtigungen, wer was wann getan hat
search-action-peer-invite = Mitwirkende einladen …
search-action-peer-invite-subtitle = Eine Einladung prüfen, bevor sie erstellt wird
search-action-peer-connect = Dialog zum Beitreten öffnen
search-action-peer-connect-subtitle = Eine Einladung von Mitwirkenden eingeben und prüfen
search-action-tab-new-terminal-here = Neuer Terminal-Tab hier
search-action-tab-new-terminal-here-subtitle = Ein Terminal als Tab in der fokussierten Fenstergruppe öffnen
search-action-window-new-terminal = Neues Terminalfenster
search-action-window-new-terminal-subtitle = Ein Terminal neben dem fokussierten Fenster öffnen
search-action-tab-duplicate = Diesen Tab duplizieren
search-action-tab-duplicate-subtitle = Eine zweite Instanz der fokussierten App daneben öffnen
search-action-tab-close = Diesen Tab schließen
search-action-tab-close-subtitle = Den fokussierten Tab schließen; war er der letzte, schließt sich auch sein Fenster
search-action-tab-reopen = Geschlossenen Tab wieder öffnen
search-action-tab-reopen-subtitle = Den zuletzt in diesem Arbeitsbereich geschlossenen Tab an seinem alten Platz zurückholen
search-action-tab-split-right = Diesen Tab nach rechts teilen
search-action-tab-split-right-subtitle = Den fokussierten Tab in eine neue Fenstergruppe neben der aktuellen verschieben
search-action-tab-move-to-new-group = Diesen Tab in eine neue Gruppe verschieben
search-action-tab-move-to-new-group-subtitle = Den fokussierten Tab in eine neue Fenstergruppe am Ende seiner Leiste verschieben
search-action-window-focus-previous = Vorheriges Fenster fokussieren
search-action-window-focus-previous-subtitle = Den Fokus auf das Fenster links in der Leiste setzen
search-action-window-focus-next = Nächstes Fenster fokussieren
search-action-window-focus-next-subtitle = Den Fokus auf das Fenster rechts in der Leiste setzen
search-action-window-focus-nth = Fenster nach Position fokussieren
search-action-window-focus-nth-subtitle = Den Fokus auf die N-te Fenstergruppe der fokussierten Leiste setzen
search-action-strip-focus-previous = Zur Leiste darüber
search-action-strip-focus-previous-subtitle = Den Fokus auf die Leiste darüber in diesem Arbeitsbereich setzen
search-action-strip-focus-next = Zur Leiste darunter
search-action-strip-focus-next-subtitle = Den Fokus auf die Leiste darunter in diesem Arbeitsbereich setzen
search-action-workspace-previous = Zum vorherigen Arbeitsbereich
search-action-workspace-previous-subtitle = Zum Arbeitsbereich vor diesem wechseln
search-action-workspace-next = Zum nächsten Arbeitsbereich
search-action-workspace-next-subtitle = Zum Arbeitsbereich nach diesem wechseln
search-action-tab-previous = Vorheriger Tab
search-action-tab-previous-subtitle = Den vorherigen Tab der fokussierten Fenstergruppe fokussieren
search-action-tab-next = Nächster Tab
search-action-tab-next-subtitle = Den nächsten Tab der fokussierten Fenstergruppe fokussieren
search-action-group-move-left = Fenstergruppe nach links verschieben
search-action-group-move-left-subtitle = Die fokussierte Fenstergruppe in ihrer Leiste um einen Platz nach links verschieben
search-action-group-move-right = Fenstergruppe nach rechts verschieben
search-action-group-move-right-subtitle = Die fokussierte Fenstergruppe in ihrer Leiste um einen Platz nach rechts verschieben
search-action-group-move-to-strip-previous = Fenstergruppe in die Leiste darüber verschieben
search-action-group-move-to-strip-previous-subtitle = Die fokussierte Fenstergruppe in die Leiste darüber tragen
search-action-group-move-to-strip-next = Fenstergruppe in die Leiste darunter verschieben
search-action-group-move-to-strip-next-subtitle = Die fokussierte Fenstergruppe in die Leiste darunter tragen
search-action-group-move-to-display-previous = Fenstergruppe auf den vorherigen Bildschirm verschieben
search-action-group-move-to-display-previous-subtitle = Die fokussierte Fenstergruppe auf den linken Bildschirm verschieben
search-action-group-move-to-display-next = Fenstergruppe auf den nächsten Bildschirm verschieben
search-action-group-move-to-display-next-subtitle = Die fokussierte Fenstergruppe auf den rechten Bildschirm verschieben
search-action-group-maximize = Fenstergruppe maximieren oder wiederherstellen
search-action-group-maximize-subtitle = Die fokussierte Fenstergruppe auf ihren Bildschirm vergrößern oder wiederherstellen
search-action-group-fullscreen = Vollbild oder wiederherstellen
search-action-group-fullscreen-subtitle = Das fokussierte Fenster im Vollbild zeigen oder wiederherstellen
search-action-group-narrower = Fenstergruppe schmaler machen
search-action-group-narrower-subtitle = Die fokussierte Fenstergruppe um einen Schritt verkleinern
search-action-group-wider = Fenstergruppe breiter machen
search-action-group-wider-subtitle = Die fokussierte Fenstergruppe um einen Schritt verbreitern
search-action-tab-move-to-group-previous = Diesen Tab in die linke Gruppe verschieben
search-action-tab-move-to-group-previous-subtitle = Den fokussierten Tab in die benachbarte Fenstergruppe links verschieben
search-action-tab-move-to-group-next = Diesen Tab in die rechte Gruppe verschieben
search-action-tab-move-to-group-next-subtitle = Den fokussierten Tab in die benachbarte Fenstergruppe rechts verschieben
search-action-notifications-dnd-toggle = „Bitte nicht stören“ umschalten
search-action-notifications-dnd-toggle-subtitle = Benachrichtigungsbanner stummschalten; zurückgehaltene Benachrichtigungen landen im Verlauf
search-action-appearance-theme-dark = Dunkles Theme setzen
search-action-appearance-theme-dark-subtitle = Dunkle Erscheinung für Chrome und Apps verwenden
search-action-appearance-theme-light = Helles Theme setzen
search-action-appearance-theme-light-subtitle = Helle Erscheinung für Chrome und Apps verwenden
search-action-appearance-high-contrast = Hohen Kontrast setzen
search-action-appearance-high-contrast-subtitle = Kontrast der aktuellen Erscheinung erhöhen
search-action-appearance-language = Sprache festlegen
search-action-appearance-language-subtitle = Desktop-Sprache in den Einstellungen ändern
search-action-identity-connect = Vorhandene Identität hinzufügen
search-action-identity-connect-subtitle = Identitätszugangsdaten importieren und in diesem Arbeitsbereich verwenden
search-action-identity-select = Identität in diesem Arbeitsbereich verwenden
search-action-identity-select-subtitle = Eine der Identitäten wählen, die bereits mit diesem Gerät verbunden sind
search-action-identity-device-revoke = Gerät einer Identität widerrufen
search-action-identity-device-revoke-subtitle = Einem verlorenen, ausgemusterten oder kompromittierten Gerät dauerhaft nicht mehr vertrauen
search-action-capture-screenshot-send = Letztes Bildschirmfoto senden
search-action-capture-screenshot-send-subtitle = An eines deiner Geräte oder in die Dateien dieses Arbeitsbereichs
search-action-presence-follow = Mitwirkenden folgen
search-action-presence-follow-subtitle = Diese Ansicht an eine verbundene Person oder einen sichtbaren Agenten koppeln
search-action-presence-status-set = Status im Arbeitsbereich festlegen
search-action-presence-status-set-subtitle = Einen ausdrücklichen Status veröffentlichen oder ihn mit einem leeren Wert löschen
search-action-presence-status-sharing-set = Status-Privatsphäre festlegen
search-action-presence-status-sharing-set-subtitle = Wählen, ob der Status aus, nur ausdrücklich oder automatisch abgeleitet ist
search-action-peer-invite-named = Mitwirkende benennen und Einladung öffnen
search-action-peer-invite-named-subtitle = Den Anzeigenamen festhalten und dann die Einladung prüfen, bevor sie erstellt wird
search-action-agent-invite = Agenten in den Arbeitsbereich einladen
search-action-agent-invite-subtitle = Eine eingegrenzte Agenten-Einrichtung starten; jeden Bereich wählen, den er nutzen darf
search-action-text-insert = Text in die fokussierte Notiz einfügen
search-action-text-insert-subtitle = An einer Zeichenposition in den fokussierten Notizen- oder Editor-Tab schreiben
search-action-record-bring = Element zu diesem Arbeitsbereich hinzufügen
search-action-record-bring-subtitle = Hier verfügbar machen, ohne das Original zu verschieben
search-action-quick-settings-brightness-set = Bildschirmhelligkeit festlegen
search-action-quick-settings-brightness-set-subtitle = Die Helligkeit eines Monitors in Prozent festlegen: die Hintergrundbeleuchtung des Panels oder ein externer Monitor über DDC/CI
search-action-quick-settings-volume-set = Ausgabelautstärke festlegen
search-action-quick-settings-volume-set-subtitle = Die Standard-Audioausgabe dieses Geräts in Prozent festlegen
search-action-quick-settings-wifi-set-enabled = WLAN ein- oder ausschalten
search-action-quick-settings-wifi-set-enabled-subtitle = Das WLAN-Funkmodul dieses Geräts ein- oder ausschalten
search-action-quick-settings-power-profile-set = Energieprofil festlegen
search-action-quick-settings-power-profile-set-subtitle = Energiesparen, Ausgeglichen oder Leistung für dieses Gerät wählen
search-action-quick-settings-vpn-set-enabled = VPN ein- oder ausschalten
search-action-quick-settings-vpn-set-enabled-subtitle = Eine benannte VPN-Verbindung auf diesem Gerät ein- oder ausschalten
search-action-quick-settings-bluetooth-set-enabled = Bluetooth ein- oder ausschalten
search-action-quick-settings-bluetooth-set-enabled-subtitle = Das Bluetooth-Funkmodul dieses Geräts ein- oder ausschalten
search-action-quick-settings-airplane-mode-set-enabled = Flugmodus ein- oder ausschalten
search-action-quick-settings-airplane-mode-set-enabled-subtitle = Alle Funkverbindungen dieses Geräts aus- oder wieder einschalten

# Suche: Suchquellen und Verwaltungszeilen des Arbeitsbereichs
search-action-search-provider-set = Suchquelle aktivieren oder deaktivieren
search-action-search-provider-set-subtitle = Festlegen, welche Quellen in Such- und Agentenergebnissen erscheinen
search-action-workspace-switch-subtitle = Zu diesem Arbeitsbereich und seinen Fenstern wechseln
search-action-activity-catch-up-subtitle = Was andere seit deinem letzten Blick geändert haben
search-action-activity-tasks-subtitle = Arbeit, die hier Personen und Agenten zugewiesen ist
search-action-activity-explore-subtitle = Jede Änderung hier, nach Person, App und Zeit
search-action-activity-replay-subtitle = Schritt für Schritt sehen, wie sich dieser Arbeitsbereich verändert hat
search-action-quick-settings-unavailable = Der Dienst für die Schnelleinstellungen ist nicht verfügbar
# Zusätzliche Suchbegriffe für Arbeitsbereichswechsel und Aktivitätsansichten; nicht angezeigt.
search-action-workspace-switch-keywords = wechseln arbeitsbereich gehe switch workspace
search-action-activity-keywords = aktivität verlauf änderungen zeitleiste wer activity history
search-action-provider-enable = Suche in { $source } aktivieren
search-action-provider-enable-subtitle = Ergebnisse aus { $source } in Suche und Agentenerkennung anzeigen
search-action-provider-disable = Suche in { $source } deaktivieren
search-action-provider-disable-subtitle = Ergebnisse aus { $source } in Suche und Agentenerkennung ausblenden
search-action-provider-badge-on = AN
search-action-provider-badge-off = AUS
# Wörter, die nach den Ein/Aus-Schaltern einzelner Suchquellen fragen; nicht angezeigt.
search-action-provider-keywords = suche suchquelle quelle quellen deaktivieren aktivieren search source disable enable
search-action-governance-subtitle = Signierte Verwaltungsaktion für den aktiven Arbeitsbereich
search-action-permission-roles-subtitle = Wiederverwendbare Arbeitsbereichsrollen mit minimalen Rechten verwalten

# Suche: was eine Shell-Aktion getan hat
search-action-tab-close-failed = Dieser Tab kann nicht geschlossen werden
search-action-tab-split-failed = Dieser Tab kann nicht geteilt werden
search-action-device-list-copied = Geräteliste kopiert
search-action-device-list-copied-body = { $count ->
    [one] 1 Gerät
   *[other] { $count } Geräte
}
search-action-enrollment-copied = Einrichtungscode kopiert
search-action-enrollment-copied-body = Er läuft in fünf Minuten ab und kann keine weiteren Geräte zulassen.
search-action-home-server-copied = Einrichtungscode für den Heimserver kopiert
search-action-home-server-copied-body = Er läuft in fünf Minuten ab und gewährt die vollständige Replikation des Arbeitsbereichs.
search-action-device-revoked = Gerät widerrufen
search-action-device-revoked-body = Der Widerruf wurde mit dem Stammschlüssel signiert und in der Geräteliste dieser Identität vermerkt.

# Suche: Anwendungen
search-apps-group-start-here = Zum Einstieg
search-apps-group-everyday = Alltag
search-apps-group-work = Arbeit
search-apps-group-media = Medien
search-apps-group-system-tools = Systemwerkzeuge
search-apps-group-more-apps = Weitere Apps
search-apps-group-developer-tools = Entwicklerwerkzeuge
search-apps-detail-grouped = { $group } · { $detail }
search-apps-open = { $app } öffnen
search-apps-open-subtitle = { $target } · { $detail }
search-apps-shortcut-subtitle = { $target } · Verknüpfung
search-apps-shortcut-failed = Die Verknüpfung konnte nicht geöffnet werden. Prüfe ihr Ziel und versuche es erneut.
search-apps-verification-pending = Prüfung durch die Registry ausstehend
search-apps-store-subtitle = App Store · { $trust } · { $detail }
search-apps-view-in-store = { $app } im App Store ansehen
search-apps-store-failed = App Store für { $app } konnte nicht geöffnet werden
search-apps-opening = { $app } wird geöffnet
search-apps-opening-body = Wird geladen …

# Suche: Kontinuitätsaktionen zwischen deinen zertifizierten Geräten
search-continuity-certified-device = Zertifiziertes Gerät
search-continuity-play-on = { $source } auf { $target } abspielen
search-continuity-media-handoff-subtitle = Diese laufende Mediensitzung auf ein bestimmtes zertifiziertes Gerät verschieben
search-continuity-media-cancel = Verschieben von „{ $media }“ abbrechen
search-continuity-media-retry = Verschieben von „{ $media }“ erneut versuchen
search-continuity-media-status-subtitle = Medienübergabe an { $device }
search-continuity-bluetooth-allow = Übergabe von { $accessory } von { $device } erlauben
search-continuity-bluetooth-decline = Übergabe von { $accessory } von { $device } ablehnen
search-continuity-bluetooth-consent-subtitle = Diese Anfrage zur Übergabe einer Bluetooth-Kopplung beantworten
search-continuity-bluetooth-retry = Übergabe von { $accessory } erneut versuchen
search-continuity-bluetooth-cancel = Übergabe von { $accessory } abbrechen
search-continuity-bluetooth-status-subtitle = Bluetooth-Kontinuität mit { $device }
search-continuity-route-audio = Audio an { $output } ausgeben
search-continuity-route-audio-subtitle = Einen der aktuell erkannten lokalen Audioausgänge wählen
search-continuity-input-raise = Eingangspegel von { $input } erhöhen
search-continuity-input-lower = Eingangspegel von { $input } senken
search-continuity-input-level-subtitle = Den Pegel des aktuellen Mikrofons um eine Stufe ändern
search-continuity-input-select = { $input } als Mikrofon verwenden
search-continuity-input-select-subtitle = Ab jetzt über diesen lokalen Eingang aufnehmen
search-continuity-audio-forward-subtitle = Eine freigegebene Live-Audioverbindung zu diesem zertifizierten Gerät anfragen
search-continuity-audio-allow = Live-Audio von { $device } erlauben
search-continuity-audio-decline = Live-Audio von { $device } ablehnen
search-continuity-audio-consent-subtitle = Diese Anfrage zur Freigabe von entferntem Audio beantworten
search-continuity-audio-stop = Weiterleitung von { $source } beenden
search-continuity-audio-stop-subtitle = Diese Live-Audioverbindung schließen und zurücksetzen
search-continuity-speaker-join = { $group } als { $role } beitreten
search-continuity-speaker-decline = Beitritt zu { $group } als { $role } ablehnen
search-continuity-speaker-consent-subtitle = { $device } hat dieses Gerät gebeten, in einer Lautsprechergruppe zu spielen
search-continuity-speaker-leave = { $group } verlassen
search-continuity-speaker-leave-subtitle = Nicht mehr als { $role } in dieser Lautsprechergruppe spielen
search-continuity-speaker-play-subtitle = Über jeden bereiten Lautsprecher dieser Gruppe abspielen
search-continuity-speaker-media-subtitle = Hier weiterspielen und den Ton an diese Lautsprechergruppe senden
search-continuity-speaker-stop = Wiedergabe auf { $group } beenden
search-continuity-speaker-stop-subtitle = Die Wiedergabe dieser Lautsprechergruppe auf allen Lautsprechern beenden
search-continuity-speaker-listen-allow = { $group } mit diesem Mikrofon kalibrieren lassen
search-continuity-speaker-listen-decline = { $group } dieses Mikrofon nicht verwenden lassen
search-continuity-speaker-listen-subtitle = { $device } möchte beim Kalibrieren der Lautsprecher mit dem Mikrofon dieses Geräts hören
search-continuity-speaker-listen-stop = { $group } dieses Mikrofon nicht mehr verwenden lassen
search-continuity-speaker-listen-stop-subtitle = Die auf { $leader } eingerichtete Lautsprecherkalibrierung darf mit diesem Mikrofon hören
search-continuity-place-left = { $device } links anordnen
search-continuity-place-right = { $device } rechts anordnen
search-continuity-place-above = { $device } oberhalb anordnen
search-continuity-place-below = { $device } unterhalb anordnen
search-continuity-place-subtitle = Einen bestimmten zertifizierten Bildschirm um dieses Gerät herum anordnen
search-continuity-control-revoke = { $device } die Steuerung entziehen
search-continuity-control-grant = { $device } die Steuerung erlauben
search-continuity-control-subtitle = Die kurzlebige geräteübergreifende Eingabefreigabe ändern
search-continuity-calibration-apply = Platzierungsvorschlag für { $device } übernehmen
search-continuity-calibration-dismiss = Platzierungsvorschlag für { $device } verwerfen
search-continuity-calibration-subtitle = Auf den aktuellen, aus der Nähe abgeleiteten Platzierungsvorschlag reagieren
search-continuity-calibration-revert = Platzierung für { $device } zurücksetzen
search-continuity-calibration-revert-subtitle = Die vorherige Platzierung des zertifizierten Bildschirms wiederherstellen
search-continuity-mirror-allow = Bildschirmspiegelung von { $device } erlauben
search-continuity-mirror-decline = Bildschirmspiegelung von { $device } ablehnen
search-continuity-mirror-consent-subtitle = Diese Anfrage zur reinen Bildschirmanzeige beantworten
search-continuity-mirror-stop = Bildschirmspiegelung mit { $device } beenden
search-continuity-mirror-stop-subtitle = Diese reine Anzeigesitzung schließen
search-continuity-mirror-request = Diesen Bildschirm auf { $device } spiegeln
search-continuity-mirror-request-subtitle = Eine freigegebene, reine Anzeigeübertragung an diesen zertifizierten Bildschirm anfragen
search-continuity-tether-request = { $device } um Tethering bitten
search-continuity-tether-request-subtitle = Eine kurzlebige, auf dem Telefon bestätigte Verbindung anfragen
search-continuity-tether-allow = Tethering für { $device } erlauben
search-continuity-tether-decline = Tethering für { $device } ablehnen
search-continuity-tether-consent-subtitle = Diese Freigabeanfrage auf dem Telefon beantworten
search-continuity-tether-disconnect = Tethering mit { $device } trennen
search-continuity-tether-disconnect-subtitle = Diese Tethering-Anfrage und ihre Netzwerkverbindung widerrufen
search-continuity-capture-camera = Kamera von { $device } verwenden
search-continuity-capture-microphone = Mikrofon von { $device } verwenden
search-continuity-capture-scanner = Scanner von { $device } verwenden
search-continuity-capture-request-subtitle = Eine entfernte Eingabe für eine Sitzung der fokussierten App anfragen
search-continuity-capture-allow = Entfernte Eingabe für { $app } erlauben
search-continuity-capture-decline = Entfernte Eingabe für { $app } ablehnen
search-continuity-capture-consent-subtitle = Diese Aufnahmeanfrage auf dem Quellgerät beantworten
search-continuity-capture-approve-app = Fokussierte App für { $device } erlauben
search-continuity-capture-stop = Entfernte Eingabe von { $device } beenden
search-continuity-capture-session-subtitle = Auf diese entfernte Aufnahmesitzung reagieren
search-continuity-controller-stop = Weiterleitung von { $controller } beenden
search-continuity-controller-stop-subtitle = Diese Controller-Sitzung zurücksetzen und schließen
search-continuity-controller-start = { $controller } auf { $device } verwenden
search-continuity-controller-start-subtitle = Normalisierte Eingaben an die aktuell fokussierte App weiterleiten
search-continuity-controller-allow = { $controller } von { $device } erlauben
search-continuity-controller-decline = { $controller } von { $device } ablehnen
search-continuity-controller-consent-subtitle = Controller-Zugriff für die lokal bestätigte fokussierte App
search-continuity-call-move = Anruf in { $room } auf { $device } verschieben
search-continuity-call-move-subtitle = Übergabe an ein bestimmtes zertifiziertes Gerät vorschlagen
search-continuity-call-allow = Übergabe für { $room } erlauben
search-continuity-call-decline = Übergabe für { $room } ablehnen
search-continuity-call-consent-subtitle = Diese Freigabe zur Anrufübergabe beantworten

# Identitätseinrichtung bei der ersten Anmeldung: was die Identität ist
onboarding-intro-title = Deine lokale Tilo-Identität erstellen
onboarding-intro-body = Tilo erstellt eine kryptografische Identität, die dir gehört, und zertifiziert dann einen eigenen Schlüssel für dieses Gerät. Tilo nutzt diese Kette für die Zusammenarbeit und für Einladungen an andere Geräte. Noch sind nicht alle Bearbeitungen in Apps kryptografisch signiert.
onboarding-intro-point-collaboration = Sie gibt dir eine beständige Identität für die Zusammenarbeit und zertifiziert dieses Gerät gegenüber anderen.
onboarding-intro-point-local = Sie wird hier auf diesem Rechner erstellt. Nichts wird hochgeladen, und kein Server bewahrt eine Kopie auf.
onboarding-intro-point-not-account = Sie ist kein Konto und hat kein Passwort – es gibt nichts, bei dem du dich anmelden müsstest.
onboarding-intro-card-title = Wofür sie da ist
onboarding-host-intro-title = Deine Tilo-Identität einrichten
onboarding-host-intro-body = Du bist bereits an diesem Computer angemeldet. Bevor sich der Tilo Desktop öffnet, braucht er eine lokale kryptografische Identität für die Zusammenarbeit und für Einladungen an andere Geräte. Sie ersetzt weder deine Linux-Anmeldung, noch erstellt sie ein Online-Konto. Noch sind nicht alle Bearbeitungen in Apps kryptografisch signiert.

# Identitätseinrichtung: kein Wiederherstellungsdienst
onboarding-no-recovery-title = Verlierst du dieses Gerät ohne Sicherung, ist diese Identität verloren
onboarding-no-recovery-point-no-service = Es gibt keinen Wiederherstellungsdienst. Kein Sicherungscode und keine Wiederherstellungsphrase können diese Identität neu erstellen.
onboarding-no-recovery-point-backup = Die als Nächstes angebotene verschlüsselte Sicherung ist der einzige Weg zur Wiederherstellung. Bewahre Datei und Passphrase getrennt auf. Ohne sie braucht ein verlorenes oder zurückgesetztes Gerät eine neue Identität; die Daten der Arbeitsbereiche bleiben erhalten, aber die alte Identität gehört dann nicht mehr dir.
onboarding-acknowledgement = Mir ist klar, dass ich diese Identität verliere, wenn ich dieses Gerät ohne Sicherung verliere.
onboarding-protect-title = Deine Tilo-Identität schützen
onboarding-protect-message = Es gibt keine Zurücksetzen-Taste. Erstelle nach diesem Schritt eine verschlüsselte, übertragbare Sicherung.
onboarding-create-failed = Deine Identität wurde nicht erstellt
onboarding-create-failed-title = Deine Identität konnte nicht erstellt werden
onboarding-create-failed-message = Es wurde nichts geschrieben, und der Desktop ist weiterhin geschützt. Geh zurück und versuche es erneut, nachdem du Speicherplatz und Berechtigungen geprüft hast.

# Identitätseinrichtung: Fortschrittsanzeige
onboarding-progress-eyebrow = Einrichtung der Tilo-Identität
onboarding-progress-stage = Schritt { $step } von { $total }  ·  { $stage }
onboarding-progress-a11y = Identitätseinrichtung, Schritt { $step } von { $total }: { $stage }
onboarding-stage-choose = Auswählen
onboarding-stage-restore = Wiederherstellen
onboarding-stage-protect = Schützen
onboarding-stage-media = Medien
onboarding-stage-ready = Fertig
onboarding-stage-backup = Sicherung

# Identitätseinrichtung: Fortsetzen nach einem Neustart
onboarding-progress-attention = Der Einrichtungsfortschritt braucht deine Aufmerksamkeit
onboarding-progress-unreadable = Die gespeicherte Einrichtungsposition konnte nicht gelesen werden ({ $error }). Deine Identität ist sicher; die Einrichtung wurde beim Abschlussschritt fortgesetzt.
onboarding-progress-invalid-identity = Die Einrichtung hat unvollständigen Fortschritt gefunden, konnte die vorhandene Identität aber nicht prüfen: { $error }
onboarding-progress-save-failed = Die Einrichtung konnte ihre Neustartposition nicht speichern: { $error }. Es wurde keine Identitätsaktion wiederholt.
onboarding-progress-clear-failed = Die Einrichtung ist abgeschlossen, aber ihre Neustartmarkierung konnte nicht entfernt werden: { $error }. Wähle erneut „Fertigstellen“, damit dieser Bildschirm bei der nächsten Anmeldung nicht wieder erscheint.

# Identitätseinrichtung: gemeinsame Schaltflächen
onboarding-back = Zurück
onboarding-cancel = Abbrechen
onboarding-continue = Weiter
onboarding-done = Fertig
onboarding-use-existing = Vorhandene verwenden
onboarding-create-identity = Identität erstellen
onboarding-create-local-identity = Lokale Identität erstellen
onboarding-restore-backup = Ausgewählte Sicherung wiederherstellen

# Identitätseinrichtung: vorhandene Identität verbinden
onboarding-connect-title = Eine vorhandene Identität verbinden
onboarding-connect-recovery-title = Deine vorhandene Identität muss wiederhergestellt werden
onboarding-connect-recovery-message = Tilo konnte die Kette deiner vorhandenen Identität nicht prüfen. Entsperre den Schlüsselbund des Desktops und versuche es erneut, oder stelle eine verschlüsselte Sicherung wieder her. Tilo ersetzt diese Identität nicht automatisch.
onboarding-connect-failed = Diese Identität konnte nicht verbunden werden
onboarding-connect-nearby = Mit einem Gerät in der Nähe verbinden
onboarding-connect-nearby-instructions = Öffne auf dem anderen Gerät Einstellungen → Geräte & Synchronisierung → Persönliches Gerät hinzufügen und vergleiche dann den sechsstelligen Code auf beiden Bildschirmen.
onboarding-connect-retry-keyring = Schlüsselbund erneut versuchen
onboarding-connect-browse = Durchsuchen …
onboarding-connect-file = Sicherungsdatei der Identität
onboarding-connect-file-placeholder = Verschlüsselte Sicherung oder ältere Identitäts-JSON auswählen
onboarding-connect-passphrase = Passphrase der Sicherung
onboarding-connect-passphrase-placeholder = Passphrase (bei älterer JSON leer lassen)
onboarding-connect-nearby-card = Vertrauenswürdiges Gerät in der Nähe
onboarding-connect-backup-card = Verschlüsselte Sicherung
onboarding-connect-choose-file-first = Wähle zuerst eine verschlüsselte Sicherung oder eine ältere Identitätsdatei aus.
onboarding-keyring-unavailable = Der Schlüsselbund des Desktops ist weiterhin nicht verfügbar. Entsperre ihn und versuche es erneut, oder stelle eine verschlüsselte Sicherung wieder her: { $error }
onboarding-picker-import-title = Eine Tilo-Identität auswählen
onboarding-picker-import-filter = Tilo-Identität

# Identitätseinrichtung: Koppeln mit einem Gerät in der Nähe
onboarding-nearby-searching = Suche nach einem vertrauenswürdigen Gerät in der Nähe …
onboarding-nearby-start-failed = Die Einrichtung über ein Gerät in der Nähe konnte nicht gestartet werden: { $error }
onboarding-nearby-unavailable = Die Einrichtung über ein Gerät in der Nähe ist in dieser Sitzung nicht verfügbar
onboarding-nearby-cancelled = Einrichtung über ein Gerät in der Nähe abgebrochen. Wähle „Mit einem Gerät in der Nähe verbinden“, um es erneut zu versuchen.
onboarding-nearby-code = Code { $code } · wartet auf Freigabe auf deinem vertrauenswürdigen Gerät
onboarding-nearby-connected = Verbunden. Deine Arbeitsbereiche erscheinen automatisch.
onboarding-nearby-invalid-identity = Das vertrauenswürdige Gerät hat eine ungültige Identität geliefert. Wähle „Mit einem Gerät in der Nähe verbinden“, um es erneut zu versuchen.
onboarding-nearby-failed = Die Einrichtung über ein Gerät in der Nähe wurde nicht abgeschlossen: Das Kopplungsfenster wurde geschlossen oder die Freigabe abgebrochen. Wähle „Mit einem Gerät in der Nähe verbinden“, um es erneut zu versuchen.
onboarding-nearby-failed-detail = Die Einrichtung über ein Gerät in der Nähe wurde nicht abgeschlossen: { $detail }. Wähle „Mit einem Gerät in der Nähe verbinden“, um es erneut zu versuchen.

# Identitätseinrichtung: verschlüsselte Sicherung
onboarding-backup-title = Deine Tilo-Identität sichern
onboarding-backup-identity-created = Deine lokale Identität wurde erstellt
onboarding-backup-explanation = Speichere eine verschlüsselte Kopie an einem anderen Ort. Bewahre Datei und Passphrase getrennt auf; zusammen stellen sie diese Identität wieder her.
onboarding-backup-passphrase = Neue Passphrase für die Sicherung
onboarding-backup-passphrase-placeholder = Lange, einzigartige Passphrase – mindestens 12 Zeichen
onboarding-backup-confirmation = Neue Passphrase für die Sicherung bestätigen
onboarding-backup-confirmation-placeholder = Passphrase bestätigen
onboarding-backup-save = Speicherort wählen und Sicherung speichern …
onboarding-backup-save-another = Weitere Kopie speichern
onboarding-backup-skip-acknowledgement = Mir ist klar, dass ich ohne verschlüsselte Sicherung diese Identität verliere, wenn ich dieses Gerät verliere.
onboarding-backup-continue-without = Ohne Sicherung fortfahren
onboarding-backup-failed = Die Sicherung wurde nicht gespeichert
onboarding-backup-verified = Verschlüsselte Sicherung geprüft
onboarding-backup-verified-earlier = Die geprüfte Sicherung wurde vor dem Neustart der Einrichtung gespeichert.
onboarding-passphrase-too-short = Verwende mindestens 12 Zeichen. Mach sie lang und einzigartig; Länge allein macht eine gängige Passphrase nicht sicher.
onboarding-passphrase-mismatch = Die beiden Passphrasen stimmen nicht überein.
onboarding-passphrase-unavailable = Die Passphrasefelder sind vorübergehend nicht verfügbar.
onboarding-picker-backup-title = Verschlüsselte Identitätssicherung speichern
onboarding-picker-backup-filter = Verschlüsselte Tilo-Identität

# Identitätseinrichtung: Beispielzustände für die Vorschau
onboarding-snapshot-restore-error = Diese Sicherung konnte nicht geöffnet werden. Prüfe Datei und Passphrase und versuche es dann erneut.
onboarding-snapshot-create-error = Die Identität konnte nicht gespeichert werden. Es wurde nichts erstellt; prüfe den verfügbaren Speicherplatz und versuche es erneut.

# Identitätseinrichtung: Identität bereit
onboarding-done-title = Deine Identität ist bereit
onboarding-done-body = Dieser Fingerabdruck kennzeichnet deinen lokalen kryptografischen Stamm. Tilo kann ihn in Abläufen der Zusammenarbeit anzeigen, die eine Überprüfung unterstützen; du musst ihn nicht eintippen.
onboarding-done-ready = Identität auf diesem Gerät gespeichert
onboarding-host-done-title = Deine Tilo-Identität ist bereit
onboarding-host-done-body = Tilo verwendet diese lokale Identität, wenn du zusammenarbeitest oder andere Geräte einlädst. Sie bleibt von deiner Linux-Anmeldung getrennt und ist kein Online-Konto.
onboarding-host-done-ready = Bereit für die Zusammenarbeit mit Tilo
onboarding-done-next-title = Wie es weitergeht
onboarding-done-next-body = Der Desktop ist bereit. Teilen bleibt ausgeschaltet, bis du es bewusst in den Einstellungen aktivierst.
onboarding-done-backup-verified = Sicherung geprüft
onboarding-done-backup-verified-body = Bewahre die verschlüsselte Sicherungsdatei und ihre Passphrase an getrennten Orten auf.
onboarding-done-backup-recommended = Sicherung weiterhin empfohlen
onboarding-done-backup-recommended-body = Erstelle in den Einstellungen eine verschlüsselte, übertragbare Sicherung, bevor du dich auf diese Identität verlässt.
onboarding-done-system-care-title = Systempflege
onboarding-done-system-care-body = Tilo sucht nach der Einrichtung automatisch nach signierten Systemupdates, sobald der Rechner online ist. In den Einstellungen kannst du Versionen prüfen, vormerken und zurücksetzen.
onboarding-tour-take = Kurze Tour starten
onboarding-finish = Weiter zum Desktop

# Einrichtung: Mediencodecs
onboarding-codecs-title = Mediencodecs installieren?
onboarding-codecs-body = H.264 und HEVC, die Formate der meisten Videos, Kameras und Anrufe, sind patentiert und nicht Teil von Tilo. Du kannst sie jetzt mit einem Klick hinzufügen.
onboarding-codecs-enables-title = Was sie ermöglichen
onboarding-codecs-enables-hardware = Hardwarebeschleunigtes Video auf AMD- und Intel-Grafik.
onboarding-codecs-enables-playback = Wiedergabe von H.264 und HEVC in Videos, die du öffnest.
onboarding-codecs-enables-encoding = Kodierung in H.264 und HEVC für Bildschirmfreigabe, Aufnahmen und Anrufe.
onboarding-codecs-source-title = Woher sie kommen
onboarding-codecs-source = Dieser Computer lädt sie direkt von RPM Fusion und von Cisco (OpenH264) unter den Lizenzen dieser Projekte herunter und prüft die Signatur jedes Pakets. Tilo stellt sie weder bereit noch verteilt es sie. Du kannst sie jederzeit unter Einstellungen, Updates entfernen.
onboarding-codecs-background = Die Einrichtung kann weiterlaufen, während der Download läuft. Du erhältst eine Benachrichtigung, sobald die Codecs bereit sind.
onboarding-codecs-progress = Fortschritt: { $percent } %
onboarding-codecs-starting = Wird gestartet …
onboarding-codecs-installing-title = Mediencodecs werden installiert
onboarding-codecs-installing-message = { $status } Die Einrichtung kann weiterlaufen, während der Download läuft. Du erhältst eine Benachrichtigung, sobald die Codecs bereit sind.
onboarding-codecs-offline-title = Die Mediencodecs werden installiert, sobald du online bist
onboarding-codecs-offline-message = Dieser Computer ist nicht mit dem Internet verbunden. Deine Wahl ist gespeichert: Die Codecs werden automatisch installiert, sobald er online ist, und du erhältst eine Benachrichtigung.
onboarding-codecs-installed-title = Mediencodecs installiert
onboarding-codecs-failed-title = Mediencodecs konnten nicht installiert werden
onboarding-codecs-failed-message = Versuche es erneut oder installiere sie später unter Einstellungen, Updates.
onboarding-codecs-failed-message-detail = { $error } Versuche es erneut oder installiere sie später unter Einstellungen, Updates.
onboarding-codecs-installer-failed = Das Installationsprogramm konnte nicht gestartet werden: { $error }
onboarding-codecs-skip = Überspringen
onboarding-codecs-not-now = Nicht jetzt
onboarding-codecs-retry = Erneut versuchen
onboarding-codecs-install = Mediencodecs installieren

# Desktop-Tour
onboarding-tour-eyebrow = Tilo  •  DESKTOP-TOUR
onboarding-tour-stage = { $stage }  ·  { $step } von 4
onboarding-tour-progress-a11y = Desktop-Tour, Schritt { $step } von 4: { $stage }
onboarding-tour-stage-move = Bewegen
onboarding-tour-stage-arrange = Anordnen
onboarding-tour-stage-share = Teilen
onboarding-tour-stage-begin = Loslegen
onboarding-tour-skip = Tour überspringen
onboarding-tour-enter = Tilo betreten
onboarding-tour-navigate-title = Alles in Reichweite
onboarding-tour-navigate-intro = Tilo hält den Desktop ruhig, bis du nach etwas fragst. Jede Geste hier hat auch einen Weg über die Tastatur.
onboarding-tour-search-title = Suche  ·  Super+K oder Super+Leertaste
onboarding-tour-search-title-mac = Suche  ·  Befehl+K oder Wahl+Leertaste
onboarding-tour-search-body = Öffne sie ohne Eingabe, um Erste Schritte, Alltag, Arbeit, Medien und Systemwerkzeuge zu durchstöbern; tippe, um jede App, Datei, Einstellung oder Aktion zu finden.
onboarding-tour-overview-title = Übersicht  ·  Super+Tab oder Finger zusammenziehen
onboarding-tour-overview-body = Sieh alle Arbeitsbereiche und Fenstergruppen und wähle dann mit Auf, Ab und Eingabe einen davon aus.
onboarding-tour-move-title = Bewegen  ·  Super+Pfeiltasten oder Wischen mit drei Fingern
onboarding-tour-move-body = Links und Rechts springen zwischen gekachelten Fenstern entlang des aktuellen Streifens. Auf und Ab wechseln zum Streifen darüber oder darunter; Super+Bild auf und Bild ab oder Wischen mit vier Fingern wechseln den Arbeitsbereich.
onboarding-tour-organise-title = Ein Ort für jede Art von Arbeit
onboarding-tour-organise-intro = Die Anordnung ist einfach, sobald ihre vier Ebenen Namen haben. Tilo merkt sich die Anordnung für dich.
onboarding-tour-workspace-title = Arbeitsbereich
onboarding-tour-workspace-body = Ein dauerhafter Ort für ein Projekt, seine Personen, Berechtigungen, Apps und seinen Verlauf.
onboarding-tour-strip-title = Streifen
onboarding-tour-strip-body = Eine waagerechte Reihe von Fenstergruppen in einem Arbeitsbereich. Füge einen weiteren Streifen hinzu, wenn die Arbeit eine zweite Spur braucht.
onboarding-tour-window-group-title = Fenstergruppe
onboarding-tour-window-group-body = Ein gekachelter Rahmen für Tabs, die zusammengehören. Wechsle mit Super+Links oder Super+Rechts zwischen Gruppen.
onboarding-tour-tab-title = Tab
onboarding-tour-tab-body = Eine App oder ein Dokument in einer Fenstergruppe. Ziehe Tabs, um sie ohne überlappende Fenster anzuordnen.
onboarding-tour-collaborate-title = Teilen bleibt bewusst
onboarding-tour-collaborate-intro = Zusammenarbeit ist in den Arbeitsbereich eingebaut, nicht an einzelne Apps angeflanscht. Nichts wird geteilt, nur weil es geöffnet ist.
onboarding-tour-invite-settings-title = Aus den Arbeitsbereich-Einstellungen einladen
onboarding-tour-invite-settings-body = Wähle eine Rolle und prüfe genau, welchen Arbeitsbereich die Einladung öffnet.
onboarding-tour-follow-title = Folgen nur, wenn du es willst
onboarding-tour-follow-body = Alle behalten ihren eigenen Fokus und ihre eigene Ansicht. Folgen ist eine ausdrückliche, umkehrbare Aktion.
onboarding-tour-system-features-title = Rückgängig und Berechtigungen sind Systemfunktionen
onboarding-tour-system-features-body = Der Verlauf folgt der Urheberschaft, und Apps fragen Tilo nach Fähigkeiten, statt stillschweigend Zugriff zu erhalten.
onboarding-tour-start-title = Deine ersten fünf Minuten
onboarding-tour-start-intro = Beginne mit einem Ziel und lass den Arbeitsbereich dann mit dem Projekt wachsen. Du findest dies über die Suche unter „Desktop-Tour“ wieder.
onboarding-tour-name-title = 1  Arbeitsbereich benennen
onboarding-tour-name-body = Öffne die Arbeitsbereich-Einstellungen über den Titel und wähle einen gut erkennbaren Projektnamen.
onboarding-tour-open-title = 2  Einen gemeinsamen Ausgangspunkt öffnen
onboarding-tour-open-body = Wähle Notizen, Kalender oder Chat und füge dann weitere Apps daneben hinzu.
onboarding-tour-invite-title = 3  Die richtigen Personen einladen
onboarding-tour-invite-body = Wähle „Personen einladen“ in den Arbeitsbereich-Einstellungen und prüfe jede Rolle, bevor du teilst.
onboarding-tour-check-title = 4  Arbeit prüfen und Fehler beheben
onboarding-tour-check-body = Öffne die Aktivität, um auf den neuesten Stand zu kommen, Versionen zu vergleichen oder nur die fehlerhafte Änderung rückgängig zu machen.

# Einladungskarte für Arbeitsbereiche: jemanden einladen, mit einer Einladung beitreten, ausstehende Einladungen
peer-sync-title-review = Einladung prüfen
peer-sync-title-invite = Jemanden einladen
peer-sync-title-join = Einem Arbeitsbereich beitreten
peer-sync-title-workspace-invite = Einladung zum Arbeitsbereich
peer-sync-paste-token = Einladungscode einfügen oder eingeben
peer-sync-identity-copied = Identität kopiert – schick sie der Person, die dich einlädt
peer-sync-invite-failed = Einladung fehlgeschlagen: { $error }
peer-sync-identity-unavailable = Identität nicht verfügbar: { $error }
peer-sync-joined-room = Raum { $room } beigetreten
peer-sync-join-refused = Abgelehnt: { $error }
peer-sync-pending-unavailable = Ausstehende Einladungen nicht verfügbar: { $error }
peer-sync-resend-failed = Erneutes Senden fehlgeschlagen: { $error }
peer-sync-invite-canceled = Einladung zurückgezogen
peer-sync-cancel-failed = Zurückziehen fehlgeschlagen: { $error }
peer-sync-unexpected-response = Unerwartete Antwort der Zusammenarbeit
peer-sync-using-verified-identity = Verifizierte Identität wird verwendet · { $verification }
peer-sync-ask-for-identity = Bitte die Person, dir ihre Tilo-Identität zu schicken, und füge sie dann hier ein.
peer-sync-name-required = Gib an, für wen diese Einladung ist
peer-sync-identity-required = Füge den Tilo-Identitätsschlüssel der Person ein, damit die Einladung nicht gestohlen werden kann
peer-sync-recipient-required = Wähle eine verifizierte Person oder füge ihre Tilo-Identität ein, damit die Einladung nicht gestohlen werden kann
peer-sync-copied = Kopiert
peer-sync-pasted = Eingefügt – drück die Eingabetaste zum Beitreten
peer-sync-clipboard-empty = Die Zwischenablage ist leer
peer-sync-identity-bound = Identität gebunden · { $fingerprint }
peer-sync-identity-invalid = Das ist kein Tilo-Identitätsschlüssel (erwartet werden 64 Hexadezimalzeichen)
peer-sync-pending-heading = AUSSTEHENDE EINLADUNGEN
peer-sync-pending-heading-paged = AUSSTEHENDE EINLADUNGEN · { $first }–{ $last } von { $total } · scrollen
peer-sync-role-expires = { $role } · { $unit ->
    [now] läuft gleich ab
    [seconds] läuft in { $count ->
        [one] 1 Sekunde
       *[other] { $count } Sekunden
    } ab
    [minutes] läuft in { $count ->
        [one] 1 Minute
       *[other] { $count } Minuten
    } ab
    [hours] läuft in { $count ->
        [one] 1 Stunde
       *[other] { $count } Stunden
    } ab
   *[days] läuft in { $count ->
        [one] 1 Tag
       *[other] { $count } Tagen
    } ab
}
peer-sync-received-role-expires = Zugriff als { $role } · { $unit ->
    [now] läuft gleich ab
    [seconds] läuft in { $count ->
        [one] 1 Sekunde
       *[other] { $count } Sekunden
    } ab
    [minutes] läuft in { $count ->
        [one] 1 Minute
       *[other] { $count } Minuten
    } ab
    [hours] läuft in { $count ->
        [one] 1 Stunde
       *[other] { $count } Stunden
    } ab
   *[days] läuft in { $count ->
        [one] 1 Tag
       *[other] { $count } Tagen
    } ab
}
peer-sync-role-viewer = Betrachter
peer-sync-role-commenter = Kommentator
peer-sync-role-editor = Bearbeiter
peer-sync-copy = Kopieren
peer-sync-cancel = Abbrechen
peer-sync-back = Zurück
peer-sync-join = Beitreten
peer-sync-paste = Einfügen
peer-sync-create-invite = Einladung erstellen
peer-sync-review-invite = Einladung prüfen
peer-sync-paste-identity = Neue Identität einfügen
peer-sync-copy-identity = Meine Identität kopieren
peer-sync-recipient-heading = WEN LÄDST DU EIN?
peer-sync-recipient-new = Jemand Neues · Tilo-Identität austauschen
peer-sync-role-heading = ZUGRIFF, DEN DIE PERSON ERHÄLT
peer-sync-review-workspace = Arbeitsbereich · { $workspace }
peer-sync-review-workspace-this = Arbeitsbereich · dieser Arbeitsbereich
peer-sync-review-inviter = Eingeladen von · { $inviter }
peer-sync-review-inviter-you = Eingeladen von · dir
peer-sync-review-recipient = Empfänger · { $name }
peer-sync-review-recipient-unnamed = Empfänger · unbenannt
peer-sync-review-role = Zugriff · { $role }
peer-sync-review-identity = Gebundene Identität · { $fingerprint }
peer-sync-review-identity-missing = Gebundene Identität · fehlt
peer-sync-review-note = Nur diese Identität kann die signierte, eine Stunde gültige Einladung einlösen.
peer-sync-selected-person = Ausgewählte Person
peer-sync-name-new-person = Name der neuen Person
peer-sync-name-placeholder = Name
peer-sync-recipient-hint = Wähle eine verifizierte Person oder füge ihre Tilo-Identität ein.
peer-sync-identity-remembered = Verifizierte gespeicherte Identität · { $fingerprint }
peer-sync-identity-new = Neue Identität gebunden · { $fingerprint }
peer-sync-invite-anyone = Jeder mit dieser Einladung
peer-sync-invite-received = Einladung erhalten
peer-sync-paste-invite-hint = Füge die Einladung ein, die man dir geschickt hat.
peer-sync-token-characters = { $token } · { $count ->
    [one] 1 Zeichen
   *[other] { $count } Zeichen
}
peer-sync-token-incomplete = Noch keine vollständige Einladung

# Übrige Shell-Hinweise: Dokumente öffnen, Herkunft von Host-Apps, Ruhestatus, Zusammenarbeit
doc-open-failed = { $name } konnte nicht geöffnet werden
doc-open-unnamed-file = Datei
host-app-source-flatpak = Flatpak
host-app-source-system = System
new-document-unavailable = Neues Dokument nicht verfügbar: { $error }
collab-unavailable-title = Zusammenarbeit nicht verfügbar
collab-unavailable-body = Apps in dieser Sitzung können keine Änderungen teilen. { $reason }
quiet-status-dnd = Bitte nicht stören
quiet-status-dnd-presenting = Bitte nicht stören während der Präsentation
quiet-status-dnd-sharing = Bitte nicht stören während der Bildschirmfreigabe
quiet-status-focus-until = Fokus bis { $time }

# Gespeicherte Arbeitsbereiche, die beim Start nicht geöffnet werden konnten
state-notice-newer-title = Deine Arbeitsbereiche wurden nicht geöffnet
state-notice-unreadable-title = Deine Arbeitsbereiche konnten nicht gelesen werden
state-notice-newer-body = Von einer neueren Tilo-Version gespeichert. Es wurde nichts geändert.
state-notice-unreadable-body = Die Datei konnte nicht gelesen werden. Es wurde nichts geändert.
state-notice-newer-detail = { $path } wurde von einer neueren Tilo-Version geschrieben (Layout { $found }; diese Version versteht { $understood }). Die Datei wurde nicht verändert und nichts wurde darüber geschrieben – diese Sitzung läuft ohne Speichern, das ist also ausgeschlossen. Aktualisiere erneut oder wechsle auf die neuere Version, um wieder an deine Arbeitsbereiche zu kommen.
state-notice-unreadable-detail = { $path } ist vorhanden, konnte aber nicht gelesen werden ({ $error }). Die Datei wurde nicht verändert, und diese Sitzung läuft ohne Speichern, das ist also ausgeschlossen.

# Arbeitsbereichsübersicht: wer in einem Raum ist und was er teilt
overview-access-shared-one = Geteilt mit { $name }
overview-access-shared-two = Geteilt mit { $first } und { $second }
overview-access-shared-many = { $count ->
    [one] Geteilt mit { $name } und 1 weiteren Person
   *[other] Geteilt mit { $name } und { $count } weiteren Personen
}
overview-items-shared = { $count ->
    [one] 1 Element geteilt
   *[other] { $count } Elemente geteilt
}

# Fenster und Dokumente, die die Shell anzeigt
shell-window-restoring = Dieses Fenster wird wiederhergestellt …
shell-document-unavailable = Dokument nicht verfügbar
shell-named-document-unavailable = { $name } nicht verfügbar
shell-path-open-failed = { $path } kann nicht geöffnet werden: { $error }
shell-path-read-failed = { $path } kann nicht gelesen werden: { $error }
editor-disk-conflict = Die Datei wurde auf dem Datenträger geändert; ungespeicherte Änderungen bleiben für die Konfliktlösung erhalten
editor-hint-disk-conflict = Datei auf dem Datenträger geändert – ungespeicherte Änderungen bleiben erhalten
editor-hint-focused = Live geteilter Code  ·  Strg+Z zum Rückgängigmachen
editor-hint-click = Zum Bearbeiten klicken
notes-empty-title = Diese Notiz ist leer
notes-empty-body = Schreib hier, was du willst. Änderungen werden beim Tippen gespeichert.
notes-empty-action = Losschreiben
window-move-unknown-tab = Dieses Fenster ist nicht mehr geöffnet.
window-move-unknown-workspace = Dieser Arbeitsbereich existiert nicht mehr.
window-move-same-workspace = Das Fenster ist bereits in diesem Arbeitsbereich.
window-move-pinned = Dieses Fenster gehört zu seinem Arbeitsbereich und kann nicht verschoben werden.
window-move-document-refused = Das Arbeitsbereichsdokument hat die Änderung nicht angenommen, daher wurde nichts verschoben.
window-move-hosted-workspace = Diesen Arbeitsbereich führt sein Besitzer, und dieses Gerät erreicht ihn gerade nicht, daher können keine Fenster hinein- oder herausgebracht werden.
window-move-not-permitted = In diesem Arbeitsbereich darfst du diese App nicht öffnen.
window-move-already-moving = Dieses Fenster wird bereits verschoben.
window-move-nobody-to-confirm = Zum Verschieben eines Fensters muss die Person an diesem Desktop zustimmen, was mitgeht, und es ist niemand da.
window-transfer-title = „{ $window }“ nach { $to } verschieben?
window-transfer-carries = Das Dokument in „{ $window }“ geht mit seinem vollständigen Bearbeitungsverlauf nach { $to }, und das Fenster öffnet sich dort so, wie du es verlassen hast.
window-transfer-move-detail = Verschieben: Es verlässt { $from }; der Verlauf dort vermerkt, dass es nach { $to } verschoben wurde.
window-transfer-copy-detail = Kopieren: { $to } erhält eine eigene Kopie mit dem bisherigen Verlauf, das Original bleibt in { $from }, und ab dann ändern sich beide getrennt.
window-transfer-access-only-you = In { $from } kannst nur du es öffnen.
window-transfer-access-others = In { $from } können { $people } es gerade öffnen. Nach dem Verschieben verlieren sie den Zugriff; nach dem Kopieren behalten sie das Original.
window-transfer-access-destination = In { $to } entscheiden die Mitglieder und Berechtigungen dieses Arbeitsbereichs, wer es öffnen kann.
window-transfer-grants-stay = Was diese App in { $from } darf, bleibt dort und kommt nicht mit: { $grants }.
window-transfer-move-blocked = Verschieben ist nicht möglich, weil ein anderes Fenster in { $from } dieses Dokument noch zeigt. Schließe es zuerst oder erstelle eine Kopie.
window-transfer-choice-move = Verschieben – { $from } hat es danach nicht mehr
window-transfer-choice-copy = Kopieren – { $from } behält das Original
window-transfer-confirm = Weiter
window-transfer-fresh-title = „{ $window }“ kann seine Arbeit nicht nach { $to } mitnehmen
window-transfer-fresh-detail = Diese App hält deine Arbeit nur im laufenden Programm, daher kann nichts davon in einen anderen Arbeitsbereich wechseln. „{ $window }“ bleibt unverändert in { $from } geöffnet. Stattdessen kannst du in { $to } ein neues, leeres Fenster der App öffnen.
window-transfer-choice-fresh = Neues Fenster in { $to } öffnen
window-transfer-fresh-confirm = Öffnen
window-transfer-failed-title = „{ $window }“ konnte nicht verschoben werden
window-transfer-failed-detail = { $reason } Es ging nichts verloren: Das Dokument ist weiterhin in { $from }.
window-transfer-failed-busy = Die App hat das Dokument noch geöffnet.
window-transfer-failed-record = Das Dokument der App konnte nicht gelesen werden.
window-transfer-failed-storage = Der Speicher der App war nicht erreichbar.
window-transfer-failed-timeout = Die App hat im anderen Arbeitsbereich nicht rechtzeitig ein Fenster geöffnet.
window-transfer-failed-closed = Das neue Fenster wurde geschlossen, bevor es sich geöffnet hat.
window-transfer-reopen-failed = Das Dokument ist hier, aber die App hat kein Fenster geöffnet. Öffne es erneut, um weiterzumachen.
window-transfer-bringing-edits = Deine letzten Änderungen werden übernommen …
window-transfer-receipt-copied = „{ $window }“ aus { $from } hierher kopiert
window-transfer-receipt-moved-out = „{ $window }“ nach { $to } verschoben
window-transfer-receipt-moved-in = „{ $window }“ aus { $from } hierher verschoben
window-transfer-lost-title = „{ $window }“ wurde während des Verschiebens geschlossen
window-transfer-lost-detail = Die App wurde unerwartet beendet, daher fehlen eventuell Änderungen aus ihren letzten Sekunden. Alles, was sie gespeichert hatte, ist jetzt in { $to }.
window-transfer-late-edits-title = Deine letzten Änderungen in „{ $window }“ sind mitgekommen
window-transfer-late-edits-detail = Änderungen, die in { $from } während des Verschiebens gemacht wurden, wurden dem Dokument hinzugefügt.
window-transfer-move-needs-window = Zum Verschieben muss das Fenster in { $from } geöffnet sein, damit es das Original dort als verschoben markieren kann. Öffne es zuerst oder erstelle eine Kopie.
window-transfer-owner-accepts = { $to } führt sein Besitzer, der das Dokument annehmen muss, bevor es dort geöffnet wird.
window-transfer-awaiting-owner = Warte darauf, dass der Besitzer von { $to } es annimmt …
window-transfer-too-large-hosted = Es ist noch zu groß, um es nach { $to } zu verschieben.
window-transfer-proposal-failed = { $to } hat es nicht angenommen: { $reason }
window-transfer-declined = Der Besitzer von { $to } hat es abgelehnt.
window-transfer-owner-timeout = Der Besitzer von { $to } hat nicht rechtzeitig geantwortet.
window-transfer-kept-as-copy-title = „{ $window }“ wurde kopiert, nicht verschoben
window-transfer-kept-as-copy-detail = Es hat sich in { $from } geändert, während der Besitzer von { $to } entschied, daher bleiben beide. { $to } hat es so, wie es beim Verschieben war.
window-transfer-not-retired-title = „{ $window }“ ist noch in { $from }
window-transfer-not-retired-detail = Sein Fenster wurde geschlossen, bevor es das Dokument als verschoben markieren konnte, daher bleibt das Original auch in { $from }.
window-transfer-late-edits-stay-title = Deine letzten Änderungen sind in { $from } geblieben
window-transfer-late-edits-stay-detail = Änderungen, die in { $from } kurz vor dem Schließen des Fensters gemacht wurden, stehen dort im Verlauf des Dokuments, nicht in { $to }.
document-import-title = „{ $title }“ in { $to } annehmen?
document-import-detail = { $person } möchte „{ $title }“ ({ $app }) samt Verlauf nach { $to } verschieben. Alle in { $to } können es dann öffnen.
document-import-accept = In { $to } annehmen
document-import-decline = Ablehnen
document-import-confirm = Antworten
document-import-someone = Jemand
document-import-answer-failed-title = Deine Antwort zu „{ $title }“ hat seinen Arbeitsbereich nicht erreicht
window-transfer-moved-away-title = „{ $window }“ wurde in einen anderen Arbeitsbereich verschoben
window-transfer-moved-away-to = { $person } hat es nach { $to } verschoben, wo es sich jetzt für dich öffnet. Was du hier siehst, ist der Stand beim Verschieben; hier kann es nicht mehr bearbeitet werden.
window-transfer-moved-away-elsewhere = { $person } hat es in einen Arbeitsbereich verschoben, in dem du nicht bist. Was du hier siehst, ist der Stand beim Verschieben; hier kann es nicht mehr bearbeitet werden.

# Startfläche für leere Arbeitsbereiche und die Standardbefehle
global-search-create-workspace = Arbeitsbereich erstellen
global-search-rename-workspace = Arbeitsbereich umbenennen
global-search-switch-workspace = Arbeitsbereich wechseln …
global-search-open-settings = Einstellungen öffnen
global-search-open-file = Datei öffnen

# Kurzbefehl-Schritte, die vor der Ausführung fragen, und Tastaturübernahme
shortcut-step-confirm-detail = „{ $shortcut }“ möchte „{ $action }“ für { $resource } ausführen.
shortcut-step-type-phrase = Gib „{ $phrase }“ ein, um zu bestätigen.
shortcut-step-confirm-title = { $action }?
shortcut-step-confirm = Bestätigen
shortcut-step-ask-detail = „{ $shortcut }“ fragt nach { $param }.
shortcut-step-continue = Weiter
shortcut-step-yes = Ja
shortcut-step-no = Nein
shortcut-trigger-failed = Der Kurzbefehl wurde nicht ausgeführt
osd-keys-captured = Die App empfängt alle Tasten · { $chord } stellt die Tastenkürzel wieder her
osd-keys-released = Tastenkürzel wiederhergestellt
osd-pad-mode = Tablett-Modus { $mode } von { $modes }
pad-map-hint = Wird angezeigt, solange die Taste gedrückt ist
pad-map-button = Taste { $number }
pad-map-ring = Ring { $number }
pad-map-strip = Streifen { $number }
pad-map-entry = { $control }: { $action }
pad-map-mode-switch = Nächster Modus (jetzt { $mode } von { $modes })
pad-map-shows-map = Zeigt diese Übersicht
pad-map-app-decides = Legt die App fest
pad-map-nothing = Tut nichts
pad-map-dial-scroll = Blättern
pad-map-dial-zoom = Zoomen
pad-map-dial-brush-size = Pinselgröße
pad-map-dial-volume = Lautstärke
pad-map-dial-brightness = Helligkeit

# Gerätefremde Ansichten, Dokumente öffnen, Identitätssicherung und Einladungen
peer-view-surface-title = Entfernte Oberfläche #{ $id }
peer-view-awaiting = Warte auf die entfernte Oberfläche …
doc-open-no-handler = Keine App auf diesem Desktop öffnet .{ $ext }-Dateien ({ $name })
doc-open-no-file-type = { $name } hat keinen Dateityp, mit dem es sich öffnen lässt
doc-open-no-workspace = Es gibt keinen aktiven Arbeitsbereich, in dem es sich öffnen lässt
identity-backup-busy = Schließe den aktuellen Identitätsvorgang ab, bevor du eine weitere Sicherung erstellst.
identity-backup-locate-failed = Die aktive Identität wurde nicht gefunden: { $error }
invite-workspace-verified-identity = Im Arbeitsbereich bestätigte Identität
palette-query-permission-roles = Berechtigungsrolle

# Barrierefreiheitsbaum: die Anzeige der Hardwaretasten
a11y-status-readout = Statusanzeige

# Compositor-Hinweise: Sperrbildschirm-Status, X11-Kompatibilität, Zusammenarbeit, Nearby-Übertragungen, Aufnahmen
lock-screen-status = Sperrstatus
xwayland-unavailable-title = X11-Kompatibilität nicht verfügbar
xwayland-unavailable-body = Ältere X11-Apps können in diesem Arbeitsbereich gerade nicht geöffnet werden. Tilo konnte den Kompatibilitätsdienst nicht am Laufen halten; verwende eine Wayland-App oder starte die Desktop-Sitzung neu.
collab-unavailable-services-root = Das Dienstverzeichnis konnte nicht erstellt werden: { $error }
collab-unavailable-socket-taken = Eine andere Sitzung auf diesem Rechner belegt bereits den Socket für die Zusammenarbeit.
collab-unavailable-broker = Der Broker konnte nicht gestartet werden: { $error }
fabric-focus-app-for-controller = Wähle zuerst eine App aus, bevor du einen Controller weiterleitest
fabric-focus-app-for-remote-input = Wähle zuerst eine App aus, bevor du eine entfernte Eingabe verwendest
launch-held-network-failed = das Netzwerk des Desktops konnte nicht starten ({ $error })
nearby-drop-your-device = dein Gerät
nearby-drop-sending = Wird in der Nähe gesendet
nearby-drop-sending-body = { $file } an { $device }
nearby-drop-sent = In der Nähe gesendet
nearby-drop-sent-body = { $file } an { $device }
nearby-drop-failed = Senden in der Nähe fehlgeschlagen
nearby-drop-failed-body = { $file } · { $device }
nearby-drop-received = In der Nähe empfangen
nearby-drop-received-body = { $file } von { $device }
capture-screenshot-sent = Bildschirmfoto gesendet
capture-screenshot-not-sent = Bildschirmfoto nicht gesendet
capture-recording-audio-lost = Die Aufnahme läuft ohne Ton weiter
capture-recording-audio-lost-body = { $path } · { $reason }
capture-recent-audio-lost = Die Aufnahme der letzten Momente läuft ohne Ton weiter
capture-failed = Aufnahme fehlgeschlagen
idle-inhibit-unnamed-app = Eine App

# Ein Bildschirmfoto an ein Gerät oder in die Dateien des Arbeitsbereichs senden
screenshot-send-offered = An { $device } angeboten; nimm es dort an, um es zu speichern
screenshot-send-saved = In den Dateien des Arbeitsbereichs als { $name } gespeichert
screenshot-send-gone = Das Bildschirmfoto { $path } ist nicht mehr vorhanden
screenshot-send-device-offline = Dieses Gerät ist nicht mehr online
screenshot-send-create-failed = { $path } kann nicht erstellt werden: { $error }
screenshot-send-no-file-name = { $path } hat keinen Dateinamen
screenshot-send-read-failed = { $path } kann nicht gelesen werden: { $error }
screenshot-send-save-failed = Speichern in den Dateien des Arbeitsbereichs nicht möglich: { $error }
screenshot-send-too-many = Im Ordner „Screenshots“ des Arbeitsbereichs gibt es zu viele Dateien mit diesem Namen
screenshot-send-sync-off = Die Gerätesynchronisierung läuft nicht
screenshot-send-no-folder = { $path } liegt in keinem Ordner

# Ein Fenster, dessen Bilder nicht angezeigt werden können
foreign-window-unshowable = Dieses Fenster kann nicht angezeigt werden.
foreign-window-not-responding = Diese App reagiert nicht.

# Warum eine Anfrage an eine Lautsprechergruppe fehlschlug, wie die Einstellungen es anzeigen
speaker-error-too-many-groups = zu viele Lautsprechergruppen
speaker-error-group-exists = diese Lautsprechergruppe gibt es bereits
speaker-error-group-gone = diese Lautsprechergruppe gibt es nicht mehr
speaker-error-not-in-group = dieser Lautsprecher gehört nicht zur Gruppe
speaker-error-calibration-range = diese Kalibrierungswerte liegen außerhalb des zulässigen Bereichs
speaker-error-request-expired = diese Anfrage für die Lautsprechergruppe ist abgelaufen
speaker-error-group-changed = diese Lautsprechergruppe hat sich geändert; frag erneut von ihrem Gerät aus
speaker-error-too-many-memberships = dieses Gerät ist bereits in zu vielen Lautsprechergruppen
speaker-error-not-a-member = dieses Gerät gehört nicht zu dieser Lautsprechergruppe
speaker-error-too-many-sessions = es sind zu viele Sitzungen von Lautsprechergruppen aktiv
speaker-error-group-unavailable = diese Lautsprechergruppe ist nicht verfügbar
speaker-error-none-ready = kein Lautsprecher in dieser Gruppe ist bereit
speaker-error-session-gone = diese Sitzung der Lautsprechergruppe ist nicht mehr aktiv
speaker-error-name = gib den Lautsprechern einen Namen mit höchstens 64 Zeichen
speaker-error-roles = jeder Lautsprecher braucht einen eigenen Ausgang und eine eigene Rolle, und ein Stereopaar braucht beide Seiten
speaker-error-unavailable = Lautsprechergruppen sind nicht verfügbar
device-fabric-error-no-identity = dieses Gerät hat keine zertifizierte persönliche Identität
speaker-error-speaker-unavailable = dieser Lautsprecher ist gerade nicht verfügbar
speaker-error-output-disconnected = dieser Ausgang ist nicht mehr verbunden
speaker-error-already-playing = ein Lautsprecher spielt bereits seinen Ton ab
speaker-error-speaker-output-gone = der Ausgang dieses Lautsprechers ist nicht mehr verbunden
speaker-error-play-failed = der Ton konnte nicht abgespielt werden: { $error }
speaker-error-device-invalid = diese Geräte-ID ist ungültig
speaker-error-output-invalid = dieser Ausgang ist ungültig
speaker-error-group-invalid = diese Lautsprechergruppe ist ungültig
speaker-error-media-off = Schalte „Medien“ unter „Geräte & Synchronisierung“ ein, um Lautsprecher auf deinen Geräten gemeinsam zu nutzen
speaker-error-no-calibration = es gibt keine abgeschlossene Kalibrierung zum Anwenden
speaker-error-no-applied-calibration = es gibt keine angewendete Kalibrierung zum Zurücksetzen
speaker-error-calibration-unavailable = die Kalibrierung ist nicht verfügbar
speaker-error-calibration-gone = diese Kalibrierung ist nicht mehr verfügbar
speaker-error-network-unavailable = das Gerätenetz ist nicht verfügbar
speaker-error-calibrate-on-leader = kalibriere diese Gruppe auf dem Gerät, das sie eingerichtet hat
speaker-error-not-all-ready = alle Lautsprecher müssen vor dem Kalibrieren bereit sein
speaker-error-request-failed = die Lautsprecheranfrage ist fehlgeschlagen
speaker-error-app-stopped = diese Anwendung spielt nichts mehr ab
speaker-error-audio-unavailable = der Ton für Lautsprechergruppen ist nicht verfügbar
speaker-error-other-session = auf diesem Gerät spielt bereits eine andere Lautsprechergruppe

# Lautsprecher für die Kalibrierung messen
speaker-calibration-reason-microphone-gone = dieses Mikrofon ist nicht mehr verbunden
speaker-calibration-reason-output-gone = der Ausgang dieses Lautsprechers ist nicht mehr verbunden
speaker-calibration-reason-play-failed = das Messsignal konnte nicht abgespielt werden: { $error }
speaker-calibration-reason-clock = die Uhr dieses Lautsprechers konnte nicht abgeglichen werden: { $reason }
clock-sync-failure-media-disabled = die Medien-Übergabe ist auf diesem Gerät ausgeschaltet
clock-sync-failure-no-reply = das andere Gerät hat auf den Uhrabgleich nicht geantwortet
now-playing-speakers-playing = Spielt auf { $group }
now-playing-speakers-connecting = { $group }: verbindet…
now-playing-speakers-syncing = { $group }: Uhren werden abgeglichen…
now-playing-speakers-clock-failed = { $group }: kann nicht synchron spielen – { $reason }
speaker-calibration-reason-not-heard = dort hob sich kein Messsignal vom Rauschen ab
speaker-calibration-reason-not-allowed = dieses Gerät hat sein Mikrofon nicht freigegeben
speaker-calibration-reason-out-of-window = die Anfrage kam zu spät auf diesem Gerät an
speaker-calibration-reason-busy = dieses Mikrofon war mit einem anderen Messsignal beschäftigt
speaker-calibration-reason-capture-failed = die Aufnahme ist fehlgeschlagen
speaker-calibration-reason-clock-unknown = die Uhr dieses Geräts ist noch nicht synchronisiert
speaker-calibration-reason-no-answer = dieses Gerät hat nicht rechtzeitig geantwortet
speaker-calibration-reason-not-at-seat = kein Mikrofon hat ihn deutlich genug gehört
speaker-calibration-failed = { $speaker } konnte nicht gemessen werden: { $reason }
speaker-calibration-cancelled = Die Kalibrierung wurde abgebrochen

# Drahtlose Bildschirmprojektion
projection-service-unavailable-detail = Der Projektionsdienst ist nicht verfügbar ({ $error }). Prüfe den Tilo-Projektordienst.
projection-service-exited = Der Projektionsdienst wurde beendet: { $status }
projection-service-invalid-discovery = Der Projektionsdienst hat ungültige Suchergebnisse geliefert: { $error }
projection-mode-unsupported = { $device } unterstützt den Modus „{ $mode }“ nicht
projection-service-unavailable = Der Projektionsdienst ist nicht verfügbar
projection-service-stopped = Der Projektionsdienst wurde beendet

# Warum eine Aktion auf deinen Geräten fehlschlug, wie die Benachrichtigung es sagt
device-fabric-error-nearby-busy = Nearby Send ist ausgelastet; versuch es erneut, wenn eine andere Übertragung fertig ist
device-fabric-error-bt-identity-invalid = die Kennung dieses Bluetooth-Zubehörs ist ungültig
device-fabric-error-bt-not-connected = nur ein gekoppeltes Zubehör, das gerade mit diesem Gerät verbunden ist, kann übergeben werden
device-fabric-error-bt-name-invalid = der Name des lokalen Bluetooth-Zubehörs ist ungültig
device-fabric-error-bt-already-here = das Bluetooth-Zubehör ist bereits auf diesem Gerät
device-fabric-error-target-not-enrolled = das Ziel ist kein registriertes Gerät
device-fabric-error-target-offline = das Zielgerät ist offline
device-fabric-error-bt-coordinator = die Bluetooth-Übergabe ist nicht verfügbar
device-fabric-error-bt-too-many = es laufen bereits zu viele Bluetooth-Übergaben
device-fabric-error-bt-approval-gone = diese Freigabe für die Bluetooth-Übergabe steht nicht mehr aus
device-fabric-error-bt-inactive = diese Bluetooth-Übergabe ist nicht mehr aktiv
device-fabric-error-bt-no-retry = diese Bluetooth-Übergabe kann nicht wiederholt werden
device-fabric-error-media-session-gone = diese Mediensitzung ist nicht mehr verfügbar
device-fabric-error-no-route = zum Ziel gibt es gerade keine authentifizierte persönliche Verbindung
device-fabric-error-handoff-gone = diese Medienübergabe ist nicht mehr verfügbar
device-fabric-error-handoff-not-pending = diese Medienübergabe steht nicht mehr aus
device-fabric-error-handoff-no-retry = nur eine fehlgeschlagene oder abgebrochene Medienübergabe kann wiederholt werden
device-fabric-error-name-not-portable = der Dateiname ist nicht übertragbar
device-fabric-error-drop-one-item = Nearby Send braucht genau eine normale Datei oder einen Ordner
device-fabric-error-drop-empty = Nearby Send kann keine leere Datei senden
device-fabric-error-file-changed = die Datei hat sich beim Lesen geändert
device-fabric-error-transfer-inactive = die Übertragung ist nicht aktiv
device-fabric-error-destination-unapproved = Nearby Send braucht ein freigegebenes Ziel
device-fabric-error-consent-expired = die Zustimmung zur Übertragung ist abgelaufen
device-fabric-error-destination-expired = das Ziel für Nearby Send ist abgelaufen
device-fabric-error-transfer-unrecoverable = die Übertragung lässt sich nicht mehr fortsetzen
device-fabric-error-transfer-no-retry = die Übertragung kann nicht mehr wiederholt werden
device-fabric-nearby-device = Gerät in der Nähe
device-fabric-error-output-gone = dieser Audioausgang ist nicht mehr verbunden
device-fabric-error-input-gone = dieser Eingang ist nicht mehr verbunden
device-fabric-error-spatial-unavailable = die räumliche Kalibrierung ist nicht verfügbar
device-fabric-error-proposal-stale = dieser Kalibrierungsvorschlag ist nicht mehr aktuell
device-fabric-error-no-applied-calibration = es gibt keine angewendete Kalibrierung zum Zurücksetzen
device-fabric-error-no-placement = für dieses Gerät ist keine Bildschirmanordnung veröffentlicht
device-fabric-error-target-not-display = das Ziel ist kein erreichbarer, steuerbarer Bildschirm
device-fabric-error-target-no-display = das Ziel hat keinen veröffentlichten Bildschirm
device-fabric-error-side-taken = auf dieser Seite ist bereits ein anderer registrierter Bildschirm
device-fabric-error-controller-not-enrolled = das steuernde Gerät ist nicht registriert
device-fabric-error-controller-no-input = das steuernde Gerät hat keinen erreichbaren Zeiger und keine Tastatur
device-fabric-error-no-lease = dieses Gerät hat gerade keine Steuerungsfreigabe
device-fabric-error-control-not-granted = dieses Gerät hat keine Steuerung freigegeben
device-fabric-error-lease-input = die aktive Freigabe erlaubt dieses Eingabegerät nicht
device-fabric-error-display-local = dieser Bildschirm ist bereits lokal
device-fabric-error-display-coordinator = die Bildschirmübergabe ist nicht verfügbar
device-fabric-error-display-busy = dieses Gerät hat bereits eine aktive Bildschirmübertragung
device-fabric-error-no-video = das zertifizierte Ziel kann kein Video empfangen
device-fabric-error-mirror-coordinator = die Bildschirmspiegelung ist nicht verfügbar
device-fabric-error-mirror-too-many = es sind zu viele Bildschirmspiegelungen aktiv
device-fabric-error-mirror-consent-gone = diese Freigabe für die Bildschirmspiegelung steht nicht mehr aus
device-fabric-error-display-receiving = dieser Bildschirm empfängt bereits eine andere Übertragung
device-fabric-error-mirror-inactive = diese Bildschirmspiegelung ist nicht mehr aktiv
device-fabric-error-gamepad-capacity = es können keine weiteren Controller weitergeleitet werden
device-fabric-error-gamepad-decided = diese Controller-Freigabe ist unbekannt oder bereits entschieden
device-fabric-error-gamepad-no-lease = das Ziel hat den Controller nicht freigegeben
device-fabric-error-inspect-failed = die Quelle kann nicht geprüft werden: { $error }
device-fabric-error-transfer-start = die Übertragung konnte nicht starten: { $error }
device-fabric-error-open-failed = die Datei kann nicht geöffnet werden: { $error }
device-fabric-error-read-failed = die Datei kann nicht gelesen werden: { $error }
continuity-disabled-clipboard = die Zwischenablage-Übergabe ist auf diesem Gerät ausgeschaltet
continuity-disabled-notifications = die Benachrichtigungs-Übergabe ist auf diesem Gerät ausgeschaltet
continuity-disabled-wifi-settings = die WLAN- und Einstellungs-Übergabe ist auf diesem Gerät ausgeschaltet
continuity-disabled-media = die Medien-Übergabe ist auf diesem Gerät ausgeschaltet
continuity-disabled-display-input = die Bildschirm- und Eingabe-Übergabe ist auf diesem Gerät ausgeschaltet
continuity-disabled-file-transfer = die Dateiübertragung ist auf diesem Gerät ausgeschaltet
continuity-disabled-realtime = die Übergabe von Echtzeitsitzungen ist auf diesem Gerät ausgeschaltet
continuity-disabled-gamepad = die Controller-Weiterleitung ist auf diesem Gerät ausgeschaltet
continuity-disabled-app-continuations = die App-Übergabe ist auf diesem Gerät ausgeschaltet
remote-audio-all-system-audio = Gesamter Systemton
remote-audio-error-target-not-output = das Ziel ist kein erreichbarer Audioausgang
remote-audio-error-unavailable = die Audioübertragung ist nicht verfügbar
remote-audio-error-too-many = es sind zu viele Audioübertragungen aktiv
remote-audio-error-source-expired = diese Auswahl der Audioquelle ist abgelaufen
remote-audio-error-request-expired = diese Anfrage für die Audioübertragung ist abgelaufen
remote-audio-error-route-inactive = diese Audioübertragung ist nicht mehr aktiv
remote-audio-speaker-group = Lautsprechergruppe
remote-audio-error-no-output = es ist kein lokaler Audioausgang verfügbar
remote-audio-error-stream-ended = der Audiostream dieser Anwendung wurde beendet
remote-capture-error-unavailable = die entfernte Eingabe ist nicht verfügbar
remote-capture-error-busy = die entfernte Eingabe ist ausgelastet
remote-capture-error-capacity = es können keine weiteren Anfragen für entfernte Eingaben gestellt werden
remote-capture-error-receiver-busy = der Empfänger der entfernten Eingabe ist ausgelastet
remote-capture-error-cannot-receive = dieses Gerät kann die angefragte entfernte Eingabe nicht empfangen
remote-capture-error-unknown-request = unbekannte Anfrage für eine entfernte Eingabe
remote-capture-error-unknown-session = unbekannte Sitzung einer entfernten Eingabe
tethering-error-continuity-off = die WLAN-Übergabe ist auf diesem Gerät ausgeschaltet
tethering-error-choose-phone = wähle ein erreichbares, zertifiziertes Telefon
tethering-error-unavailable = Tethering ist nicht verfügbar
tethering-error-too-many = es sind bereits zu viele Tethering-Anfragen aktiv
tethering-error-history-full = der Tethering-Verlauf ist vorübergehend voll
tethering-error-session-inactive = diese Tethering-Sitzung ist nicht mehr aktiv
tethering-error-request-gone = diese Tethering-Anfrage steht nicht mehr aus
gamepad-error-unavailable = die Controller-Weiterleitung ist nicht verfügbar
gamepad-error-no-identity = die zertifizierte Identität dieses Geräts ist nicht verfügbar
gamepad-error-target-unknown = das Ziel gehört nicht zu deinen zertifizierten Geräten
gamepad-error-target-offline = das Zielgerät ist kein erreichbares anderes Gerät
gamepad-error-disconnected = dieser Controller ist nicht mehr verbunden
gamepad-error-already-forwarded = dieser Controller wird bereits weitergeleitet
gamepad-error-busy = der Controller war beim Starten belegt; versuch es erneut
gamepad-error-unknown = diese Controller-Weiterleitung ist unbekannt
gamepad-error-open-failed = der Controller konnte nicht geöffnet werden: { $error }
gamepad-error-prime-failed = der Controller konnte nicht vorbereitet werden: { $error }

# Warum ein WLAN nicht mit deinen Geräten geteilt werden konnte
wifi-share-error-enterprise = Unternehmens-WLAN braucht auf diesem Gerät eine Identität, eine EAP-Methode und ein Zertifikat
wifi-share-error-no-passphrase = diese WLAN-Sicherheit hat kein übertragbares Passwort
wifi-share-error-unknown-security = eine unbekannte WLAN-Sicherheit kann nicht sicher geteilt werden
wifi-share-error-unvalidated = die WLAN-Sicherheit konnte für das Teilen nicht geprüft werden
wifi-share-error-no-presence = dieses Gerät ist für deine anderen Geräte noch nicht sichtbar

# Warum die Installation einer App aus dem Katalog fehlschlug
catalogue-error-missing = dieses System hat keinen App-Katalog („app“ fehlt)
catalogue-error-spawn = das Laden konnte nicht gestartet werden
catalogue-error-run = der App-Katalog konnte nicht ausgeführt werden: { $error }
catalogue-error-exited = der App-Katalog wurde beendet: { $status }

# Warum das Senden an ein Gerät in der Nähe fehlschlug
nearby-error-destination-not-folder = das Ziel für Nearby Send ist kein Ordner
nearby-error-too-large = die Übertragung überschreitet die Größenbegrenzung
nearby-error-destination-name = für das Ziel von Nearby Send ist kein Name verfügbar
nearby-error-outside-grant = die Quelle für Nearby Send liegt außerhalb der Freigabe von Dateien
nearby-error-one-item = Nearby Send braucht genau eine normale Datei oder einen Ordner
nearby-error-queue-full = die Warteschlange für Übertragungen ist voll
nearby-error-quota-full = der Speicherplatz für Übertragungen ist voll
nearby-error-too-many = zu viele Übertragungen
nearby-error-not-regular = die Quelle ist keine normale Datei
nearby-error-folder-too-many = der Ordner enthält zu viele Einträge
nearby-error-folder-special = der Ordner enthält eine Verknüpfung oder Sonderdatei
nearby-error-folder-too-large = der Ordner überschreitet die Größenbegrenzung
nearby-error-source-changed = die Quelle hat sich während der Vorbereitung geändert
nearby-error-name-not-portable = der Name der Quelle ist nicht übertragbar

# Ein Dokument, das ein Tab nicht wiederherstellen konnte
hosted-document-unavailable = { $path } ist nicht verfügbar: { $error }

# Eine Kamera, ein Mikrofon oder ein Scanner, geteilt von einem deiner anderen Geräte
remote-capture-source-label = { $app } auf zertifiziertem Gerät

# Druckaufträge: der Titel in der Druckwarteschlange, wenn die App keinen angegeben hat
print-untitled-document = Dokument

a11y-speaker-listen = { $leader } dieses Mikrofon zum Kalibrieren von { $group } verwenden lassen
a11y-speaker-listen-description = Es nimmt nur auf, während jeder Lautsprecher ein Messsignal spielt; die Aufnahme bleibt auf diesem Gerät, gesendet werden nur Laufzeit und Pegel
a11y-speaker-listen-allow = Dieses Mikrofon erlauben
a11y-speaker-listen-decline = Dieses Mikrofon nicht erlauben
a11y-speaker-listener = { $group } kann mit diesem Mikrofon kalibrieren
a11y-speaker-listener-description = Die Lautsprecherkalibrierung darf mit diesem Mikrofon hören; aktivieren, um das zu beenden
speaker-calibration-listening-refused = { $microphone } durfte nicht zuhören, daher kann an deinem Platz nichts gemessen werden
speaker-error-listening-unavailable = wähle ein Mikrofon, das an deinem Platz zuhören kann