# Shell — deutsche Übersetzung (de-DE).
# Schlüssel und Variablen müssen mit en-US.ftl übereinstimmen.

# Auf diesem Desktop fortsetzen, was du gerade auf einem anderen Gerät getan hast.
continuity-continue = { $app } von { $device } fortsetzen
continuity-continue-subtitle = Den Tab fokussieren, den du gerade auf { $device } verwendet hast
continuity-dismiss = Ausblenden
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

# Eine Änderung eines Anzeigewerkzeugs (kanshi, wlr-randr) behalten oder zurücksetzen
display-confirm-tool-title = Die von { $tool } angewendeten Anzeigeeinstellungen behalten?
display-confirm-tool-unknown-title = Die von einem Werkzeug angewendeten Anzeigeeinstellungen behalten?
display-confirm-always = Änderungen von { $tool } immer behalten
display-confirm-a11y-description = Ein Anzeigewerkzeug hat deine Bildschirme geändert. Eingabe behält die Änderung, Escape stellt die vorherige Anordnung wieder her.

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

# Der Fensterwechsler (Alt+Tab).
window-switcher-title = Fenster wechseln
window-switcher-description = Fenster in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke Tab oder die Pfeiltasten zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
window-switcher-app-title = Fenster dieser App wechseln
window-switcher-app-description = Fenster dieser App in diesem Arbeitsbereich, zuletzt verwendete zuerst. Halte Alt gedrückt und drücke ` zum Bewegen, lass Alt los zum Wechseln oder drücke Escape, um zu bleiben.
window-switcher-untitled = Unbenanntes Fenster
window-switcher-caption-paged = { $title } ({ $position } von { $count })

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
guide-row-move-window = Fenster verschieben
guide-row-resize = Größe ändern
guide-row-quick-settings = Schnelleinstellungen
guide-touch-intro = Gesten folgen den horizontalen Fenstern und vertikalen Arbeitsbereichen des Desktops.
guide-touch-focus-next-window = Nächstes Fenster fokussieren
guide-touch-focus-previous-window = Vorheriges Fenster fokussieren
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
topbar-workspace-name-tooltip = Arbeitsbereichseinstellungen öffnen
topbar-workspace-fallback = Arbeitsbereich
topbar-calendar-none = Kein Kalender verbunden
topbar-sharing-agent-controls = AGENT STEUERT · { $name }
topbar-sharing-agent-viewing = AGENT SIEHT ZU · { $name }
topbar-sharing-person-controls = STEUERT · { $name }
topbar-sharing-person-viewing = SIEHT ZU · { $name }
topbar-sharing-session-controls = SITZUNG STEUERT · { $name }
topbar-sharing-session-viewing = SITZUNG SIEHT ZU · { $name }
topbar-sharing-count-controlling = { $controllers } { $controllers ->
        [one] STEUERT
       *[other] STEUERN
    } · { $viewers } { $viewers ->
        [one] SIEHT ZU
       *[other] SEHEN ZU
    }
topbar-sharing-count = { $viewers } { $viewers ->
        [one] SIEHT ZU
       *[other] SEHEN ZU
    }
topbar-sharing-surface = Oberfläche { $surface }
topbar-open-overview = Übersicht öffnen
topbar-previous-workspace = Vorheriger Arbeitsbereich
topbar-next-workspace = Nächster Arbeitsbereich
topbar-workspace-settings = Einstellungen für Arbeitsbereich { $workspace }
topbar-show-clock = Uhr anzeigen
topbar-show-system-monitor = Systemmonitor anzeigen
topbar-show-calendar = Kalender anzeigen
topbar-open-quick-settings = Schnelleinstellungen öffnen
topbar-show-collaborators = Mitwirkende anzeigen
topbar-viewer-can-control = kann { $targets } steuern
topbar-viewer-view-only = sieht nur { $targets }
topbar-viewer-entry = { $name } ({ $kind ->
        [agent] Agent
        [person] Person
       *[session] unbekannte Sitzung
    }): { $access }
topbar-collaborators-visible = Bildschirm sichtbar für { $viewers }. Mitwirkende anzeigen
topbar-capture-privacy = Datenschutzeinstellungen für aktive Aufnahme
topbar-control = Steuerelement der oberen Leiste
topbar-editor-control = Bearbeitungselement der oberen Leiste
topbar-editor-remove-clock = Uhr aus der oberen Leiste entfernen
topbar-editor-remove-system-monitor = Systemmonitor aus der oberen Leiste entfernen
topbar-editor-remove-calendar = Kalender aus der oberen Leiste entfernen
topbar-editor-add-clock = Uhr zur oberen Leiste hinzufügen
topbar-editor-add-system-monitor = Systemmonitor zur oberen Leiste hinzufügen
topbar-editor-add-calendar = Kalender zur oberen Leiste hinzufügen
topbar-editor-title = Obere Leiste bearbeiten
topbar-editor-hint = Zum Anordnen ziehen · „Entfernen“ klicken
topbar-widget-clock = Uhr
topbar-widget-system-monitor = Systemmonitor
topbar-widget-calendar = Kalender
topbar-editor-remove = Entfernen
topbar-editor-add-clock-chip = + Uhr
topbar-editor-add-monitor-chip = + Monitor
topbar-editor-add-calendar-chip = + Kalender
topbar-invite = Einladen
topbar-clocks-notification-title = Uhren
topbar-clocks-notification-body = Die Zeit ist abgelaufen
topbar-activity-unread = { $headline }  ·  { $count } neu
topbar-clock-resume = Fortsetzen
topbar-clock-pause = Pausieren
topbar-clock-lap = Runde
topbar-clock-add-minute = +1 Min.
topbar-clock-close-stopwatch = Schwebende Stoppuhr schließen
topbar-clock-close-timer = Schwebenden Timer schließen
topbar-clock-floating-stopwatch = Schwebende Stoppuhr
topbar-clock-floating-timer = Schwebender Timer
topbar-clock-floating-description = Zum Verschieben ziehen. Mit der Tastatur Alt gedrückt halten und die Pfeiltasten verwenden; zusätzlich Umschalt für größere Schritte.
topbar-clock-stopwatch = Stoppuhr
topbar-clock-timer = Timer
topbar-clock-paused = Pausiert
topbar-clock-running = Läuft
topbar-clock-record-lap = Runde erfassen
topbar-clock-add-one-minute = Eine Minute hinzufügen
topbar-workspace-position = Arbeitsbereich { $position } von { $total }
topbar-desktop-navigation = Desktop-Navigation
topbar-display-navigation = Bildschirmnavigation

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
host-fabric-input-selected = Aufnahme von { $input }
host-fabric-tethering-allowed = { $device } darf die Verbindung dieses Telefons nutzen
host-fabric-tethering-declined = Tethering für { $device } abgelehnt
host-fabric-tethering-disconnected = Tethering mit { $device } getrennt
host-fabric-asked-mirror = { $device } wurde gebeten, die Bildschirmspiegelung einmal zu erlauben
host-fabric-asked-capture = { $device } wurde gebeten, einmal { $input } zu teilen
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

# Barrierefreiheitsbaum: aufbewahrte Benachrichtigungen
a11y-notifications-none = Keine aufbewahrten Benachrichtigungen.
a11y-notifications-description = Neueste Zeilen { $first } bis { $last } von { $total }, nach App gruppiert. Mit Pfeil nach oben und unten wählst du eine Benachrichtigung, mit Bild auf und Bild ab springst du um eine Seite, mit Pos1 oder Ende zur neuesten oder ältesten aufbewahrten Benachrichtigung. Eingabe öffnet die gewählte Benachrichtigung oder klappt ihre App-Gruppe ein, Entf verwirft sie.
a11y-notifications-list = Aufbewahrte Benachrichtigungen
a11y-notifications-clear = Alle Benachrichtigungen löschen
a11y-notifications-clear-description = Löscht den aufbewahrten Benachrichtigungsverlauf dieser Sitzung. Acht Sekunden lang kannst du das rückgängig machen.
a11y-notifications-cleared = Benachrichtigungen gelöscht
a11y-notifications-undo = Löschen der Benachrichtigungen rückgängig machen
a11y-notifications-undo-description = Stellt die gelöschten Benachrichtigungen in ihrer ursprünglichen Reihenfolge wieder her. Drücke Eingabe oder Leertaste.
a11y-notifications = Benachrichtigungen

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

# Schnelleinstellungen: Benachrichtigungsverlauf
qs-notifications-title = Benachrichtigungen
qs-notifications-empty = Keine neuen Benachrichtigungen
qs-notifications-cleared = Benachrichtigungen gelöscht
qs-notifications-clear-all = Alle löschen
qs-notifications-position = Neueste { $first }–{ $last } von { $total }
qs-notifications-count = { $count ->
        [one] { $count } Benachrichtigung
       *[other] { $count } Benachrichtigungen
    }
qs-notifications-source-system = System
qs-notifications-newer = ↑ Neuere
qs-notifications-at-newest = Neueste erreicht
qs-notifications-older = ↓ Ältere
qs-notifications-at-oldest = Älteste erreicht

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
qs-devices-speaker-setup-note = Stereopaar oder Raumlautsprecher · öffnet Einstellungen → Lautsprecher
qs-devices-speaker-membership-note = eingerichtet auf { $leader } · auswählen zum Austreten
qs-devices-another-certified = Ein anderes zertifiziertes Gerät
qs-devices-requested-allow-once = { $source } hat das angefragt · einmal erlauben oder ablehnen
qs-devices-audio-app = Audio der fokussierten App · endet, wenn der Fokus wechselt
qs-devices-audio-system = Gesamtes Systemaudio · unten ein zertifiziertes Ziel wählen
qs-devices-certified-device = zertifiziertes Gerät
qs-devices-audio-awaiting = wartet auf Freigabe mit { $peer } · auswählen zum Beenden
qs-devices-audio-streaming = streamt mit { $peer } · auswählen zum Beenden
qs-devices-audio-interrupted = unterbrochen mit { $peer } · auswählen zum Verwerfen
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
history-severity-info = Information
history-severity-warning = Warnung
history-severity-error = Fehler
history-source-system = System
history-notification-description = { $severity }. Von { $source }. { $time }
history-notification-description-body = { $severity }. Von { $source }. { $time }. { $body }

# Zeilen der Benachrichtigungszentrale
notification-center-group-description = { $count } Benachrichtigungen. Eingabe zum { $expanded ->
        [yes] Einklappen
       *[no] Ausklappen
    }, Entf, um alle zu verwerfen.
notification-center-dismiss-all = Alle von { $app } verwerfen
notification-center-dismiss = Verwerfen
notification-center-hand-to = An { $agent } übergeben
notification-center-hand-to-description = Übergibt diese Benachrichtigung als Aufgabe an den Agenten des Arbeitsbereichs.
notification-center-group-count = { $count ->
        [one] { $count } Benachrichtigung
       *[other] { $count } Benachrichtigungen
    }

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
notification-lock-count = { $count ->
    [one] 1 Benachrichtigung
   *[other] { $count } Benachrichtigungen
}
notification-dnd-held = { $status } · { $count } zurückgehalten
notification-stack-a11y = Benachrichtigungen
notification-group-system = System
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
shell-activity-title = Während du weg warst
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

# People panel, as assistive technology reads it
a11y-shell-people = Personen
a11y-shell-people-description = Live-Anwesenheit im Arbeitsbereich, aktuelle Tätigkeit, Absicht und Datenschutz für den Status
a11y-shell-people-screen-visible = Jetzt sichtbarer Bildschirm: { $exposure }
a11y-shell-people-here-now = Jetzt anwesend
a11y-shell-people-agent = Agent
a11y-shell-people-invited = Eingeladen, derzeit nicht anwesend
a11y-shell-people-present = Jetzt anwesend
a11y-shell-people-now = Jetzt: { $activity }
a11y-shell-people-intent = Absicht: { $status }
a11y-shell-people-intent-inferred = Absicht, abgeleitet: { $status }
a11y-shell-status-name = Dein Zusammenarbeitsstatus
a11y-shell-status-inferred = Auf diesem Gerät aus letzten Aktionen abgeleitet
a11y-shell-status-set = Lege fest, woran du gerade arbeitest
a11y-shell-status-automatic = Automatischer Status
a11y-shell-status-manual = Nur manueller Status
a11y-shell-status-off = Status ausblenden
a11y-shell-status-automatic-description = Einen kurzen Status lokal aus den letzten Aktionen im Arbeitsbereich ableiten
a11y-shell-status-manual-description = Nur selbst eingegebenen Text teilen
a11y-shell-status-off-description = Keinen Status in diesem Arbeitsbereich veröffentlichen
a11y-shell-follow-collaborator = Mitwirkender Person folgen
a11y-shell-follow = { $name } folgen
a11y-shell-follow-description = Die Live-Ansicht dieser Person im Arbeitsbereich spiegeln
a11y-shell-people-previous = Vorherige Personen
a11y-shell-people-previous-description = Die vorherigen Personen in diesem Arbeitsbereich anzeigen
a11y-shell-people-next = Weitere Personen
a11y-shell-people-next-description = Die nächsten Personen in diesem Arbeitsbereich anzeigen
a11y-shell-invite = Personen einladen
a11y-shell-invite-description = Eine authentifizierte Einladung in den Arbeitsbereich erstellen
a11y-shell-join = Arbeitsbereich beitreten
a11y-shell-join-description = Mit einer authentifizierten Einladung verbinden
a11y-shell-workspace-activity = Aktivität im Arbeitsbereich
a11y-shell-workspace-activity-description = Gruppierte Ergebnisse prüfen und sehen, was in deiner Abwesenheit passiert ist

# Empty-workspace launchpad
a11y-shell-launchpad-title = Gemeinsam etwas schaffen
a11y-shell-launchpad-description = Wähle eine gemeinsame Unterhaltung, Notizen, Aufgaben, ein Meeting, eine Einladung oder die Aktivität im Arbeitsbereich.
a11y-shell-launchpad-open-app = App öffnen
a11y-shell-launchpad-discuss = Besprechen
a11y-shell-launchpad-notes = Notizen
a11y-shell-launchpad-tasks = Aufgaben
a11y-shell-launchpad-meet = Meeting planen
a11y-shell-launchpad-invite = Personen einladen
a11y-shell-launchpad-activity = Auf dem Laufenden bleiben
a11y-shell-launchpad-discuss-detail = Die gemeinsame Unterhaltung öffnen
a11y-shell-launchpad-notes-detail = Gemeinsam Ideen aufschreiben
a11y-shell-launchpad-tasks-detail = Arbeit im Kalender planen
a11y-shell-launchpad-meet-detail = Chat öffnen, dann einen Sprach- oder Videoanruf starten
a11y-shell-launchpad-invite-detail = Jemanden in diesen Arbeitsbereich holen
a11y-shell-launchpad-activity-detail = Sehen, was sich geändert hat und warum

# Workspace activity, as assistive technology reads it
a11y-shell-activity-everyone = Alle
a11y-shell-activity-more-people = Weitere Personen
a11y-shell-activity-more-people-description = Weitere Filter für Mitwirkende anzeigen
a11y-shell-activity-resume = { $target } fortsetzen
a11y-shell-activity-resume-description = Die zuletzt bearbeitete Ressource öffnen: { $target }
a11y-shell-activity-boundary-newer = Die letzte Aktion in die neuere Arbeitssitzung verschieben
a11y-shell-activity-boundary-older = Die erste Aktion in die ältere Arbeitssitzung verschieben
a11y-shell-activity-boundary-newer-description = Diese Grenze neu ziehen, indem eine Aktion in die neuere Arbeitssitzung wandert
a11y-shell-activity-boundary-older-description = Diese Grenze neu ziehen, indem eine Aktion in die ältere Arbeitssitzung wandert
a11y-shell-activity-more-sessions = Weitere Arbeitssitzungen
a11y-shell-activity-more-sessions-description = Weitere Arbeitssitzungen anzeigen
a11y-shell-activity-new-task = Neue Aufgabe
a11y-shell-activity-new-task-description = Eine Aufgabe im Arbeitsbereich erstellen, ohne ein Modell zu benötigen
a11y-shell-activity-create-task = Aufgabe erstellen
a11y-shell-activity-cancel-task = Erstellen der Aufgabe abbrechen
a11y-shell-activity-everything = Alles
a11y-shell-activity-agent-tasks = Agent-Aufgaben
a11y-shell-activity-unclassified = Nicht zugeordnet
a11y-shell-activity-more-lenses = Weitere Ansichten
a11y-shell-activity-more-lenses-description = Weitere Verlaufsansichten anzeigen
a11y-shell-activity-history-entries = Verlaufseinträge
a11y-shell-activity-previous-checkpoint = Vorheriger Zwischenstand
a11y-shell-activity-next-checkpoint = Nächster Zwischenstand
a11y-shell-activity-pause-replay = Wiedergabe pausieren
a11y-shell-activity-play-replay = Wiedergabe abspielen
a11y-shell-activity-history-scope = Verlaufsumfang
a11y-shell-activity-history-view = Verlaufsansicht
a11y-shell-activity-time-range = Zeitraum
a11y-shell-activity-person-filter = Personenfilter
a11y-shell-activity-brief-description = { $detail }; zuletzt geändert { $when }; { $count } { $count ->
        [one] Änderung
       *[other] Änderungen
    } von { $authors }
a11y-shell-activity-task-unassigned = nicht zugewiesen
a11y-shell-activity-task-assigned-agent = einem Agenten zugewiesen
a11y-shell-activity-task-no-resource = keine angehängte Ressource
a11y-shell-activity-task-route = Ausführungsweg der Aufgabe
a11y-shell-activity-task-composer-action = Aktion zum Erstellen der Aufgabe
a11y-shell-activity-task-action = Aktion für die ausgewählte Aufgabe
a11y-shell-activity-history-lens = Verlaufsansicht
a11y-shell-activity-undo-task-description = Jeden betroffenen Eintrag dieser Agent-Aufgabe rückgängig machen; spätere Änderungen anderer bleiben erhalten
a11y-shell-activity-history-action = Aktion für den ausgewählten Verlauf
a11y-shell-activity-explorer-panel = Explorer-Bereich
a11y-shell-activity-timeline-description = Verlaufseinträge; mit den Pfeiltasten navigieren
a11y-shell-activity-checkpoints-description = Durch die Zwischenstände des Arbeitsbereichs gehen
a11y-shell-activity-replay-description = Die Wiedergabe des Arbeitsbereichs abspielen oder pausieren
a11y-shell-activity-view-catch-up = Eine Zusammenfassung der Ergebnisse, seit dieses Gerät den Arbeitsbereich zuletzt angesehen hat
a11y-shell-activity-view-tasks = Dauerhafte Aufgaben im Arbeitsbereich mit Ressourcenlinks, Ausführungswegen und Prüfprotokollen
a11y-shell-activity-view-explore = Der vollständige Verlauf des Arbeitsbereichs mit Filtern und Herkunft
a11y-shell-activity-view-replay = Eine kausale Wiedergabe der Anordnung und der Aktivität der Beteiligten
a11y-shell-activity-entry-author = Von { $author }
a11y-shell-activity-tasks = Aufgaben im Arbeitsbereich
a11y-shell-activity-selected-detail = Details zum ausgewählten Verlauf
a11y-shell-activity-selected-detail-description = { $title }; { $count } { $count ->
        [one] Aktion
       *[other] Aktionen
    }; von { $authors }
a11y-shell-activity-agent-run-description = Anweisung: { $instruction }; zugewiesen von { $issuer }; ausgeführt von { $agent }; Begründung: { $reason }
a11y-shell-activity-reason-not-recorded = nicht erfasst
a11y-shell-activity-agent-step = { $app } in { $record }: { $summary }
a11y-shell-activity-agent-review = Prüfung der Agent-Aufgabe
a11y-shell-activity-task-description = { $status }; { $route }; { $assignee }; { $count } { $count ->
        [one] Prüfereignis
       *[other] Prüfereignisse
    }
a11y-shell-activity-task-latest = zuletzt: { $latest }

# Workspace wheel
a11y-shell-wheel = Arbeitsbereichsrad
a11y-shell-wheel-switch = Zu { $workspace } wechseln
a11y-shell-wheel-add = Arbeitsbereich hinzufügen

# The emoji picker (Super+. by default)
emoji-picker-title = Emoji
emoji-picker-search = Emoji suchen
emoji-picker-groups = Emoji-Kategorien
emoji-picker-recent = Zuletzt verwendet
emoji-picker-results = Passende Emoji
emoji-picker-no-results = Keine Emoji gefunden
emoji-picker-hint = Eingabe fügt ein · Esc schließt
emoji-picker-hint-tones = Eingabe fügt ein · Umschalt+Eingabe wählt einen Hautton
emoji-picker-tones = Hauttöne
emoji-picker-tones-hint = Rechtsklick, langes Drücken oder Umschalt+Eingabe für Hauttöne
emoji-picker-a11y-description = Tippen Sie, um zu suchen. Die Pfeiltasten wechseln zwischen den Emoji, Tab wechselt die Kategorie, Eingabe fügt das markierte Emoji in das Textfeld ein, in dem Sie geschrieben haben, Umschalt+Eingabe wählt einen Hautton, Escape schließt.
# Ein Emoji in einem Hautton: „Daumen hoch, mittlerer Hautton“.
emoji-picker-toned = { $name }, { $tone }
emoji-picker-tone-light = heller Hautton
emoji-picker-tone-medium-light = mittelheller Hautton
emoji-picker-tone-medium = mittlerer Hautton
emoji-picker-tone-medium-dark = mitteldunkler Hautton
emoji-picker-tone-dark = dunkler Hautton
