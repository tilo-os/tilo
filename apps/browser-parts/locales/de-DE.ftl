## History: what changed in the browser's document
browser-add-bookmark = { $url } als Lesezeichen gespeichert
browser-remove-bookmark = Lesezeichen { $item } entfernt
browser-set-site-protection = { $enabled ->
    [true] Trackerschutz für { $origin } eingeschaltet
   *[false] Trackerschutz für { $origin } ausgeschaltet
}
browser-browser-bookmarks = Die Lesezeichen geändert
browser-bookmark-title = Lesezeichen { $item } umbenannt
browser-settings-adblock-enabled = { $new ->
    [true] Blockieren von Werbung und Trackern eingeschaltet
   *[false] Blockieren von Werbung und Trackern ausgeschaltet
}
browser-settings-clear-on-exit = { $new ->
    [true] Löschen der Browserdaten beim Beenden eingeschaltet
   *[false] Löschen der Browserdaten beim Beenden ausgeschaltet
}
browser-settings-search-engine = Die Suchmaschine geändert
browser-settings-homepage = Die Startseite geändert

## History: the person's browsing history
browser-visit = { $url } besucht
browser-forget = { $url } vergessen
browser-clear-history = Den Browserverlauf gelöscht

## History: the person's address book
browser-address-book-enabled = { $new ->
    [true] Adressen automatisch ausfüllen eingeschaltet
   *[false] Adressen automatisch ausfüllen ausgeschaltet
}
browser-address-book-profiles = Die gespeicherten Adressen geändert
browser-address-profile-id = Die Kennung der Adresse { $item } geändert
browser-address-profile-label = Die Adresse { $item } umbenannt
browser-address-profile-full-name = Den vollständigen Namen der Adresse { $item } geändert
browser-address-profile-given-name = Den Vornamen der Adresse { $item } geändert
browser-address-profile-family-name = Den Nachnamen der Adresse { $item } geändert
browser-address-profile-street = Die Straße der Adresse { $item } geändert
browser-address-profile-city = Den Ort der Adresse { $item } geändert
browser-address-profile-region = Die Region der Adresse { $item } geändert
browser-address-profile-postal-code = Die Postleitzahl der Adresse { $item } geändert
browser-address-profile-country = Das Land der Adresse { $item } geändert

## Personal migration library and recovery
browser-personal-bookmarks-back = Zurück
browser-personal-bookmarks-previous = Vorherige
browser-personal-bookmarks-next = Nächste
browser-personal-bookmarks-empty = Hier gibt es keine Lesezeichen
browser-personal-bookmarks-remove = Lesezeichen entfernen
browser-personal-bookmarks-unnamed-folder = Unbenannter Ordner
browser-workspace-bookmarks = Lesezeichen des Arbeitsbereichs
browser-continuity-failed = Wir konnten das Übernehmen deiner Browserdaten nicht abschließen.
browser-continuity-retry = Erneut versuchen
browser-service-forget = Konto auf diesem Gerät vergessen
browser-service-forgetting = Dieses Konto wird auf diesem Gerät vergessen …
browser-service-forgotten = Lokale Websitesitzung entfernt
browser-service-custody-backend-needed = Gespeicherte Kontozugangsdaten wurden nicht widerrufen. Zum Entfernen wird ein Verwahrungsdienst benötigt.
browser-service-forget-open-first = Öffne zuerst das ausgewählte Konto in diesem Fenster.
browser-service-forget-wait = Warte, bis das ausgewählte Konto fertig geöffnet ist.
browser-service-forget-fence-failed = Das ausgewählte Konto konnte zum Vergessen nicht abgeschottet werden.

browser-service-forget-retry = Vergessen dieses Kontos erneut versuchen
browser-service-forget-retry-failed = Das Konto konnte nicht gelöscht werden. Gespeicherte Zugangsdaten wurden nicht widerrufen; versuche erneut, dieses Konto zu vergessen.
browser-engine-repair-message = Die eingebaute Webkomponente fehlt. Prüfe ihre Reparatur im Store; ohne deine Zustimmung wird nichts heruntergeladen.
browser-engine-repair-action = Reparatur prüfen
browser-engine-reopen = Schließe Browser und öffne ihn erneut.

## Toolbar
browser-back = Zurück; gedrückt halten oder Rechtsklick für den Verlauf
browser-back-tooltip = Zurück (Alt+Links); gedrückt halten oder Rechtsklick für den Verlauf
browser-forward = Vor; gedrückt halten oder Rechtsklick für den Verlauf
browser-forward-tooltip = Vor (Alt+Rechts); gedrückt halten oder Rechtsklick für den Verlauf
browser-stop = Laden stoppen
browser-reload = Neu laden
browser-reload-tooltip = Neu laden (Strg+R)
browser-address = Adresse und Suche
browser-address-placeholder = Suchen oder Adresse eingeben
browser-go = Öffnen
browser-suggestion-search = Mit { $engine } suchen
browser-page-bookmarked = Seite ist ein Lesezeichen
browser-bookmark-page = Lesezeichen für diese Seite
browser-library-open = Browserbibliothek öffnen
browser-library-close = Browserbibliothek schließen
browser-more-commands = Weitere Befehle
browser-private = Privat
browser-private-window = Privates Fenster
browser-private-tooltip = Browserdaten werden entfernt, wenn dieses Fenster geschlossen wird. Heruntergeladene Dateien bleiben.

## The More menu
browser-menu-site-information = Website-Informationen
browser-menu-library = Verlauf, Lesezeichen und Downloads
browser-menu-picture-in-picture = Bild im Bild
browser-menu-reader-view = Leseansicht
browser-menu-pause-history = Verlauf pausieren
browser-menu-find = Auf der Seite suchen
browser-menu-developer-tools = Entwicklerwerkzeuge
browser-menu-zoom-in = Vergrößern
browser-menu-zoom-out = Verkleinern
browser-menu-actual-size = Originalgröße
browser-menu-actual-size-zoomed = Originalgröße ({ $percent } %)
browser-menu-ads-and-trackers = Werbung und Tracker
browser-menu-new-private-window = Neues privates Fenster
browser-menu-settings = Einstellungen
browser-menu-close-settings = Einstellungen schließen
browser-menu-page = Seite
browser-menu-zoom = Zoom
browser-menu-privacy = Datenschutz
browser-menu-extensions = Erweiterungen
browser-menu-manage-extensions = Erweiterungen verwalten
browser-extension-add = Zu Tilo Browser hinzufügen
browser-extension-adding = Erweiterung wird hinzugefügt
browser-extension-side-panel-close = Seitenleiste schließen
browser-extension-side-panel = Seitenleiste von { $name }
browser-extension-can = Sie kann
browser-extension-access-all-sites = Alle Ihre Daten auf allen Websites lesen und ändern
browser-extension-access-site = Ihre Daten auf { $site } lesen und ändern
browser-extension-access-sites = Ihre Daten auf { $count } Websites lesen und ändern
browser-extension-block-content = Inhalte auf jeder Seite blockieren
browser-extension-read-history = Ihren Browserverlauf lesen
browser-extension-change-history = Ihren Browserverlauf lesen und ändern
browser-extension-bookmarks = Ihre Lesezeichen lesen und ändern
browser-extension-downloads = Ihre Downloads verwalten
browser-extension-clipboard-read = Kopierte und eingefügte Daten lesen
browser-extension-clipboard-write = Kopierte und eingefügte Daten ändern
browser-extension-notifications = Benachrichtigungen anzeigen
browser-extension-location = Ihren Standort ermitteln
browser-extension-management = Ihre Apps, Erweiterungen und Designs verwalten
browser-extension-native = Mit Programmen auf diesem Gerät kommunizieren
browser-extension-privacy = Ihre Datenschutzeinstellungen ändern
browser-extension-top-sites = Eine Liste Ihrer meistbesuchten Websites lesen
browser-extension-screen = Den Inhalt Ihres Bildschirms aufnehmen
browser-extension-debugger = Die geöffneten Seiten untersuchen und steuern
browser-extension-no-access = Sie braucht keinen besonderen Zugriff
browser-extension-no-version = Diese Erweiterung läuft in Chrome nicht mehr. Viele haben eine neuere Ausgabe, etwa uBlock Origin Lite für uBlock Origin.
browser-extension-update-asks = { $name } möchte seit dem Update mehr Zugriff. Schalten Sie die Erweiterung ein, um ihn zu erlauben.
browser-extension-notification-progress = { $percent } % erledigt
browser-extension-notification-from = Von { $name }
browser-extension-asks = { $name } möchte mehr Zugriff. Wenn Sie es erlauben, kann die Erweiterung
browser-extension-added = { $name } wurde hinzugefügt
browser-extension-offer = { $name } zu Tilo Browser hinzufügen?
browser-extension-version = Version { $version }
browser-extension-confirm = Erweiterung hinzufügen
browser-extension-cancel = Abbrechen
browser-extension-close = Schließen
browser-extension-turn-on = Einschalten
browser-extension-turn-off = Ausschalten
browser-extension-settings = Einstellungen
browser-extension-pin = Anheften
browser-extension-unpin = Lösen
browser-extension-remove = Entfernen
browser-extension-pin-to-toolbar = { $name } an die Symbolleiste anheften
browser-extension-running = Läuft
browser-extension-starting = Wird gestartet…
browser-extension-off = Aus
browser-extension-unsupported = Wird von Chrome nicht mehr unterstützt
browser-extension-access-unknown = Ihr Zugriff konnte nicht gelesen werden
browser-extension-how-to-add = Öffnen Sie eine Erweiterung im Chrome Web Store und wählen Sie Zu Tilo Browser hinzufügen, oder installieren Sie eine heruntergeladene .crx- oder .zip-Datei.
browser-extension-file = Erweiterungspaket
browser-extension-file-placeholder = Pfad zu einer .crx- oder .zip-Datei
browser-extension-install-file = Aus Datei installieren
browser-extension-store-failed = Die Erweiterung konnte nicht aus dem Chrome Web Store geladen werden. Prüfen Sie Ihre Verbindung und versuchen Sie es erneut.
browser-extension-too-large = Diese Erweiterung ist zu groß zum Hinzufügen.
browser-extension-damaged = Dieses Erweiterungspaket ist beschädigt oder unvollständig. Laden Sie es neu herunter und versuchen Sie es erneut.
browser-extension-broken = Diese Erweiterung ist fehlerhaft und kann nicht hinzugefügt werden. Suchen Sie nach einer neueren Version des Anbieters.
browser-extension-not-saved = Die Erweiterung konnte auf diesem Gerät nicht gespeichert werden. Schaffen Sie Speicherplatz und versuchen Sie es erneut.
browser-extension-file-missing = Unter diesem Pfad gibt es keine Datei. Prüfen Sie den Pfad und versuchen Sie es erneut.
browser-extension-file-unreadable = Diese Datei konnte nicht geöffnet werden. Prüfen Sie, ob Sie sie lesen dürfen, und versuchen Sie es erneut.
browser-extension-turn-on-failed = { $name } konnte nicht eingeschaltet werden. Versuchen Sie es erneut.
browser-extension-turn-off-failed = { $name } konnte nicht ausgeschaltet werden. Versuchen Sie es erneut.
browser-extension-remove-failed = { $name } konnte nicht entfernt werden. Versuchen Sie es erneut.
browser-extension-pin-failed = { $pinned ->
    [true] { $name } konnte nicht an die Symbolleiste angeheftet werden. Versuchen Sie es erneut.
   *[false] { $name } konnte nicht von der Symbolleiste gelöst werden. Versuchen Sie es erneut.
}
browser-extension-load-failed = { $name } konnte nicht eingeschaltet werden. Entfernen Sie die Erweiterung und fügen Sie sie erneut hinzu.
browser-extension-unreadable = Erweiterungen konnten nicht geladen werden. Schließen Sie Browser und öffnen Sie ihn erneut.
browser-extension-page-failed = Die Seite der Erweiterung konnte nicht geöffnet werden. Versuchen Sie es erneut.
browser-extension-window-none = Dieses Fenster hat keine Erweiterungen
browser-extension-not-added = Diese Erweiterung ist nicht hinzugefügt
browser-extension-runs-no-more = Diese Erweiterung läuft in Chrome nicht mehr
browser-extension-no-button = Diese Erweiterung ist aus oder hat keine Schaltfläche in der Symbolleiste
browser-extension-no-settings = Diese Erweiterung ist aus oder hat keine Einstellungsseite
browser-extension-open-store-first = Öffnen Sie zuerst die Seite einer Erweiterung im Chrome Web Store
browser-extension-already-added = Diese Erweiterung ist bereits hinzugefügt
browser-extension-another-adding = Eine andere Erweiterung wird gerade hinzugefügt
browser-page-stopped = Diese Seite funktioniert nicht mehr
browser-popup-close = Dieses Fenster schließen

## A page's own questions (alert, confirm, prompt, leave this page)
browser-page-dialog-message-title = Nachricht von { $site }
browser-page-dialog-message-title-page = Nachricht von dieser Seite
browser-page-dialog-question-title = { $site } fragt
browser-page-dialog-question-title-page = Diese Seite fragt
browser-page-dialog-close = Schließen
browser-page-dialog-allow = Zulassen
browser-page-dialog-send = Senden
browser-page-dialog-cancel = Abbrechen
browser-page-dialog-leave-title = Diese Seite verlassen?
browser-page-dialog-reload-title = Diese Seite neu laden?
browser-page-dialog-unsaved = Vorgenommene Änderungen werden eventuell nicht gespeichert.
browser-page-dialog-leave = Verlassen
browser-page-dialog-reload = Neu laden
