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

# Was ein änderbares Tastenkürzel tut, als Handlung in „…“
shortcut-action-search = Suche öffnen
shortcut-action-workspace-overview = Arbeitsbereichsübersicht öffnen
shortcut-action-lock = Bildschirm sperren
shortcut-action-quick-settings = Schnelleinstellungen öffnen
shortcut-action-shortcut-help = Diese Hilfe öffnen
shortcut-action-focus-previous-window = Vorheriges Fenster fokussieren
shortcut-action-focus-next-window = Nächstes Fenster fokussieren
shortcut-action-previous-tab = Vorherigen Tab auswählen
shortcut-action-next-tab = Nächsten Tab auswählen
shortcut-action-previous-strip = Vorherige Leiste auswählen
shortcut-action-next-strip = Nächste Leiste auswählen
shortcut-action-previous-workspace = Vorherigen Arbeitsbereich auswählen
shortcut-action-next-workspace = Nächsten Arbeitsbereich auswählen
shortcut-action-toggle-maximize = Fenster maximieren oder wiederherstellen
shortcut-action-detach-tab = Tab in eine neue Gruppe verschieben
shortcut-action-attach-tab-left = Tab in die Gruppe links verschieben
shortcut-action-attach-tab-right = Tab in die Gruppe rechts verschieben
shortcut-action-move-group-left = Gruppe nach links verschieben
shortcut-action-move-group-right = Gruppe nach rechts verschieben
shortcut-action-move-group-to-previous-display = Gruppe auf den linken Bildschirm verschieben
shortcut-action-move-group-to-next-display = Gruppe auf den rechten Bildschirm verschieben
shortcut-action-resize-narrower = Fenster schmaler machen
shortcut-action-resize-wider = Fenster breiter machen
shortcut-action-reopen-closed-tab = Zuletzt geschlossenen Tab wieder öffnen
shortcut-action-focus-group-1 = Erstes Fenster der Leiste fokussieren
shortcut-action-focus-group-2 = Zweites Fenster der Leiste fokussieren
shortcut-action-focus-group-3 = Drittes Fenster der Leiste fokussieren
shortcut-action-focus-group-4 = Viertes Fenster der Leiste fokussieren
shortcut-action-focus-group-5 = Fünftes Fenster der Leiste fokussieren
shortcut-action-focus-group-6 = Sechstes Fenster der Leiste fokussieren
shortcut-action-focus-group-7 = Siebtes Fenster der Leiste fokussieren
shortcut-action-focus-group-8 = Achtes Fenster der Leiste fokussieren
shortcut-action-focus-group-9 = Neuntes Fenster der Leiste fokussieren
shortcut-action-open-files = Dateien öffnen
shortcut-action-open-terminal = Terminal öffnen
shortcut-action-clipboard-history = Verlauf der Zwischenablage öffnen
shortcut-action-notifications = Benachrichtigungen öffnen
shortcut-action-toggle-fullscreen = Fenster auf Vollbild schalten oder wiederherstellen
shortcut-action-toggle-magnifier = Bildschirmlupe ein- oder ausschalten
shortcut-action-magnifier-zoom-in = Stärker vergrößern
shortcut-action-magnifier-zoom-out = Weniger vergrößern
shortcut-action-power-menu = Ein/Aus-Menü öffnen
shortcut-action-next-input-source = Zum nächsten Tastaturlayout wechseln

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
guide-reopen-closed-tab = Geschlossenen Tab wieder öffnen
guide-overview = Übersicht
guide-quick-settings = Schnelleinstellungen
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
