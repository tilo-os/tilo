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
browser-continuity-failed = Wir konnten deine Browserdaten nicht vollständig übernehmen.
browser-continuity-retry = Erneut versuchen
browser-service-forget = Konto auf diesem Gerät vergessen
browser-service-forgetting = Dieses Konto wird auf diesem Gerät vergessen …
browser-service-forgotten = Lokale Websitesitzung entfernt
browser-service-forget-open-first = Öffne zuerst das ausgewählte Konto in diesem Fenster.
browser-service-forget-wait = Warte, bis das ausgewählte Konto fertig geöffnet ist.
browser-service-forget-fence-failed = Das ausgewählte Konto konnte zum Vergessen nicht abgeschottet werden.

browser-service-forget-retry = Vergessen dieses Kontos erneut versuchen
browser-service-forget-retry-failed = Das Konto konnte nicht gelöscht werden. Gespeicherte Zugangsdaten wurden nicht widerrufen; versuche erneut, dieses Konto zu vergessen.
browser-service-open = { $name } öffnen
browser-engine-failed = Schließe Browser und öffne ihn erneut. Startet er weiterhin nicht, aktualisiere Browser im Store oder installiere ihn neu.

## Toolbar
browser-back = Zurück
browser-back-tooltip = Zurück (Alt+Links); gedrückt halten oder Rechtsklick für den Verlauf
browser-forward = Vor
browser-forward-tooltip = Vor (Alt+Rechts); gedrückt halten oder Rechtsklick für den Verlauf
browser-stop = Laden stoppen
browser-reload = Neu laden
browser-reload-tooltip = Neu laden (Strg+R)
browser-address = Adresse und Suche
browser-address-placeholder = Suchen oder Adresse eingeben
browser-go = Öffnen
browser-suggestion-search = Mit { $engine } suchen
browser-suggestion-bookmark = Lesezeichen, { $url }
browser-page-bookmarked = Seite ist ein Lesezeichen
browser-bookmark-page = Lesezeichen für diese Seite
browser-library-open = Browserbibliothek öffnen
browser-library-close = Browserbibliothek schließen
browser-more-commands = Weitere Befehle
browser-private = Privat
browser-private-window = Privates Fenster
browser-private-tooltip = Browserdaten werden entfernt, wenn dieses Fenster geschlossen wird. Heruntergeladene Dateien bleiben.
browser-visitor-window = Nicht angemeldet
browser-visitor-open-signed-in = Mit Ihren Anmeldungen öffnen

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
browser-extension-side-panel-width = Breite der Seitenleiste
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
browser-extension-revoke = Zugriff widerrufen
browser-extension-granted = Später erlaubt: { $access }
browser-extension-granted-quiet = Sie haben ihr später mehr Zugriff erlaubt
browser-extension-nothing-granted = Diese Erweiterung hat keinen Zugriff, den Sie später erlaubt haben
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

## Die gespeicherten Adressen, aus denen der Browser Formulare füllt
browser-autofill-offline = Adressausfüllen ist hier nicht verfügbar: Dieses Fenster surft ohne deine Anmeldungen.
browser-autofill-enable = Adressausfüllen einschalten
browser-autofill-disable = Adressausfüllen ausschalten
browser-autofill-selected-address = Die gewählte Adresse
browser-autofill-delete-question = { $label } löschen?
browser-autofill-delete = Adresse löschen
browser-autofill-keep = Adresse behalten
browser-autofill-save = Adresse speichern
browser-autofill-discard = Änderungen verwerfen
browser-autofill-add = Adresse hinzufügen
browser-autofill-saved = Adresse auf diesem Gerät gespeichert.

## An address or page that did not open
browser-address-failed = Diese Adresse konnte nicht geöffnet werden
browser-address-empty = Geben Sie eine Webadresse oder Suchbegriffe ein
browser-address-invalid = Diese Adresse lässt sich nicht öffnen. Prüfen Sie sie und versuchen Sie es erneut.
browser-address-not-web = Hier öffnen sich nur Webadressen. Geben Sie eine ein, die mit https:// beginnt
browser-page-failed = Diese Seite konnte nicht geöffnet werden
browser-page-failed-not-found = Unter dieser Adresse gibt es keine Website. Prüfen Sie sie auf Tippfehler und versuchen Sie es erneut.
browser-page-failed-offline = Sie sind offline. Verbinden Sie sich mit dem Internet und versuchen Sie es erneut.
browser-page-failed-no-answer = Die Website hat nicht geantwortet. Versuchen Sie es gleich noch einmal.
browser-page-failed-not-secure = Die sichere Verbindung zur Website ist fehlgeschlagen, deshalb wurde die Seite nicht geöffnet. Versuchen Sie es später erneut.
browser-page-failed-blocked = Diese Seite ist gesperrt und kann hier nicht geöffnet werden.
browser-page-failed-retry = Die Seite wurde nicht geladen. Versuchen Sie es erneut.
browser-page-retry = Erneut versuchen

## The Library: its sections, its empty states and its rows
browser-library-title = Bibliothek
browser-library-done = Fertig
browser-library-section-history = Verlauf
browser-library-section-bookmarks = Lesezeichen
browser-library-section-downloads = Downloads
browser-library-section-extensions = Erweiterungen
browser-library-pause = Verlauf pausieren
browser-library-resume = Verlauf fortsetzen
browser-library-clear-filter = Bibliotheksfilter löschen
browser-library-no-match = Keine Treffer
browser-library-history-paused = Der Verlauf ist in diesem Fenster pausiert
browser-library-history-empty = Noch kein Verlauf
browser-library-bookmarks-empty = Noch keine Lesezeichen
browser-library-bookmarks-empty-hint = Wählen Sie den Stern in der Adressleiste, um eine Seite zu behalten.
browser-library-extensions-empty = Keine Erweiterungen installiert
browser-library-remove-bookmark = Lesezeichen { $title } entfernen
browser-library-forget-visit = { $title } aus dem Verlauf entfernen

## Downloads: the list in the Library and its progress
browser-downloads-empty = Noch keine Downloads
browser-download-open = Öffnen
browser-download-cancel = Abbrechen
browser-download-cancel-name = Download { $name } abbrechen
browser-download-reveal = In Dateien anzeigen
browser-download-reveal-name = { $name } in Dateien anzeigen
browser-download-retry = Erneut versuchen
browser-download-retry-name = Download { $name } erneut versuchen
browser-download-cannot-retry = Erneuter Versuch nicht möglich
browser-download-downloading = { $name } wird heruntergeladen
browser-download-waiting-destination = Warten auf einen Speicherort …
browser-download-progress = { $received } von { $total }
browser-download-received = { $received } heruntergeladen
browser-download-complete = Download abgeschlossen
browser-download-saved-to = Gespeichert unter { $path }
browser-download-failed = Der Download ist fehlgeschlagen
browser-download-failed-notice = Download fehlgeschlagen
browser-download-cancelled = Abgebrochen
browser-downloads-progress-status = { $count ->
    [one] { $count } Download bei { $percent } %
   *[other] { $count } Downloads bei { $percent } %
}
browser-downloads-active-status = { $count ->
    [one] { $count } aktiver Download
   *[other] { $count } aktive Downloads
}
browser-download-prompt = { $name } von { $url } herunterladen? In einem neuen Ordner in Downloads speichern.
browser-download-save = Speichern
browser-download-unavailable = Die heruntergeladene Datei ist nicht mehr vorhanden.
browser-download-open-failed = Der Download konnte nicht geöffnet werden. Versuchen Sie, seinen Ordner anzuzeigen.

## A site asking for a permission
browser-permission-request = { $origin } fordert { $rights } an. Die Anfrage läuft in { $seconds } Sekunden ab.
browser-permission-allow = Zulassen
browser-permission-deny = Ablehnen

## The site panel: information, permissions and tracker protection
browser-site-information = Website-Informationen
browser-site-launcher-tooltip = Verbindung, Berechtigungen und gespeicherte Daten
browser-site-page-protection = Schutz
browser-site-page-connection = Verbindung und Daten
browser-site-protection = Werbung und Tracker
browser-site-done = Fertig
browser-site-no-origin = Kein HTTP- oder HTTPS-Ursprung
browser-site-connection-secure = Die Verbindung ist sicher
browser-site-connection-mixed = Verschlüsselte Verbindung mit gemischten Inhalten
browser-site-connection-insecure = Die HTTP-Verbindung ist nicht verschlüsselt
browser-site-connection-unverified = Die Sicherheit der Verbindung ist nicht bestätigt
browser-site-certificate = Zertifikat
browser-site-issuer = Aussteller
browser-site-connection = Verbindung
browser-site-certificate-problem = Zertifikatsproblem
browser-site-status-code = Statuscode { $code }
browser-site-expires = Läuft ab
browser-site-permission-camera = Kamera
browser-site-permission-microphone = Mikrofon
browser-site-permission-location = Standort
browser-site-permission-notifications = Benachrichtigungen
browser-site-permission-ask = Nachfragen
browser-site-permission-blocked = Blockiert
browser-site-permission-allowed = Erlaubt
browser-site-block-permissions = Berechtigungen blockieren
browser-site-block-note = Blockiert Kamera, Mikrofon, Standort und Benachrichtigungen. Laden Sie die Seite neu, um laufende Sitzungen zu beenden.
browser-site-ask-again = Erneut fragen
browser-site-clear-data = Websitedaten löschen
browser-site-clear-note = Löscht Speicher und Cookies dieser Website, auch Cookies, die sie mit ihren Subdomains teilt. Laden Sie die Seite neu, um verbliebene Daten zurückzusetzen.
browser-site-protection-off-settings = Der Schutz ist in den Einstellungen aus
browser-site-protection-off = Der Schutz ist für diese Website aus
browser-site-protection-on = Der Schutz ist für diese Website an
browser-site-blocked-count = { $count } Anfragen auf dieser Seite blockiert oder ersetzt
browser-site-protection-enable = Einschalten und neu laden
browser-site-protection-disable = Ausschalten und neu laden
browser-site-filters-updated-on = Filter zuletzt aktualisiert am { $date }
browser-site-filters-update = Filter aktualisieren
browser-site-working = Wird ausgeführt …
browser-site-query-timeout = Die Website-Informationen kamen nicht an; neuer Versuch …
browser-site-action-timeout = Die Aktion hat zu lange gedauert. Laden Sie die Seite neu und versuchen Sie es dann erneut.
browser-site-reading = Website-Informationen werden gelesen …
browser-site-tls-unknown = TLS-Version nicht verfügbar
browser-site-unavailable = nicht verfügbar

## The filter list the site panel reports
browser-filters-built-in = Die eingebaute Filterliste wird verwendet
browser-filters-updating = Filterlisten werden aktualisiert …
browser-filters-updated = Filterlisten aktualisiert
browser-filters-cached = Gespeicherte Filterlisten werden verwendet
browser-filters-failed = Aktualisierung fehlgeschlagen: { $detail }. Die bisherigen Regeln bleiben erhalten.
browser-filters-failed-worker = Aktualisierung fehlgeschlagen: Hilfsprozess nicht verfügbar. Die bisherigen Regeln bleiben erhalten.
browser-address-suggestions = Adressvorschläge
browser-window-name = Browser
browser-page-menu-name = Seitenmenü

## Fenster- und Barrierefreiheits-Texte
browser-quit-downloads-running = Downloads laufen noch
browser-quit-cancelling-downloads = Downloads werden vor dem Beenden abgebrochen…
browser-section-passwords = Passwörter
browser-section-no-secure-page = Keine sichere Seite
browser-section-address-autofill = Adress-Autofill
browser-a11y-search-engine = Suchmaschine
browser-a11y-clear-on-quit = Daten beim Schließen des Browsers löschen
browser-a11y-block-ads = Werbung und Tracker blockieren
browser-settings-title = Einstellungen

## Fakten, Eigenschaftsnamen und Bereichstitel
browser-save-login = Anmeldung für { $site } speichern?
browser-section-saved-accounts = Gespeicherte Konten
browser-section-forms = Formulare auf dieser Seite
browser-action-keep-page = Seite so lassen, wie sie ist
browser-action-close-picture-in-picture = Bild im Bild schließen
browser-action-close-reader-view = Leseansicht schließen
browser-action-resume-history = Verlauf in diesem Fenster fortsetzen
browser-action-let-leave = Freigeben: { $request }

## Passwörter und Adress-Autofill
browser-credentials-done = Fertig
browser-credentials-refresh = Formulare und Konten neu lesen
browser-credentials-refresh-addresses = Formulare neu lesen
browser-credentials-fill = Anmeldung einsetzen
browser-credentials-fill-address = Ausgewählte Adresse einsetzen
browser-credentials-save-from-form = Anmeldung aus dem Formular speichern
browser-credentials-delete = Anmeldung löschen
browser-credentials-save-account = Neues Konto speichern
browser-credentials-update-account = Ausgewähltes Konto aktualisieren
browser-credentials-delete-account = Konto löschen
browser-credentials-keep-account = Konto behalten
browser-credentials-confirm-delete = Diese gespeicherte Anmeldung löschen?
browser-credentials-kept-here = Das Passwort liegt im Passwortspeicher dieses Geräts.
browser-credentials-no-login-form = Kein unterstütztes Anmeldeformular gefunden.
browser-credentials-no-address-form = Kein unterstütztes Adressformular auf dieser Seite gefunden.
browser-credentials-no-accounts = Keine gespeicherten Konten für diese Seite.
browser-credentials-unnamed-account = Konto ohne Namen
browser-credentials-form = Formular
browser-credentials-address-form = Adressformular
browser-credentials-create-form = Konto-anlegen-Formular
browser-credentials-sign-in-form = Anmeldeformular
browser-credentials-form-numbered = { $form } { $number }
browser-credentials-unavailable-private = Passwörter und Adress-Autofill sind hier nicht verfügbar: Dieses Fenster surft ohne deine Anmeldungen.
browser-credentials-unavailable-engine = Das Ausfüllen von Formularen ist mit dieser Browser-Engine nicht verfügbar.
browser-credentials-unavailable-insecure = Öffne eine sichere HTTPS-Seite, um ein Formular auszufüllen.
browser-credentials-checking = Diese Seite wird geprüft…
browser-credentials-select-address = Wähle vor dem Ausfüllen eine aktive, gespeicherte Adresse.
browser-credentials-capture-expired = Die Aufnahme ist abgelaufen. Lies die Anmeldung erneut.
browser-credentials-request-expired = Die Anfrage ist abgelaufen. Lies die Seite neu, bevor du es erneut versuchst; ein bereits gestarteter Vorgang kann noch fertig werden.

## Was das Aktionsverzeichnis und das Fenster benennen
browser-page-untitled = Webseite
browser-page-content-name = Webinhalt
browser-history-back = Verlauf zurück
browser-history-forward = Verlauf vor
browser-history-paused = Verlauf pausiert
browser-history-recording = Verlauf wird aufgezeichnet
browser-history-already-empty = Der Verlauf ist bereits leer
browser-resource-history = Verlauf
browser-resource-bookmarks = Lesezeichen
browser-resource-downloads = Downloads
browser-resource-extensions = Erweiterungen
browser-resource-library = Bibliothek
browser-resource-settings = Einstellungen
browser-resource-accounts = Konten
browser-resource-account = Konto
browser-resource-continuity = Surfverlauf mitnehmen
browser-resource-personal-bookmarks = Persönliche Lesezeichen
browser-resource-bookmark-navigation = Lesezeichen-Navigation
browser-library-already-clear = Der Filter der Bibliothek ist bereits leer
browser-action-needs-page = Öffne zuerst eine Webseite
browser-action-no-previous-page = Keine vorherige Seite
browser-action-no-next-page = Keine nächste Seite
browser-action-page-idle = Die Seite ist untätig

## Die Einstellungen des Browsers
browser-settings-done = Fertig
browser-settings-manage = Verwalten
browser-settings-new-private-window = Neues privates Fenster
browser-settings-clear-history = Verlauf löschen
browser-settings-clear-site-data = Websitedaten löschen
browser-settings-general = Allgemein
browser-settings-homepage-label = Startseite
browser-settings-adblock = Werbung und Tracker blockieren
browser-settings-adblock-detail = Blockiert Werbung, Tracker und Werbevideos auf YouTube.
browser-settings-clear-on-exit-label = Daten beim Schließen des Browsers löschen
browser-settings-clear-on-exit-detail = Löscht beim Beenden Verlauf und Cookies.
browser-settings-browsing-data = Surf-Daten
browser-settings-browsing-data-detail = Verlauf sowie Cookies und Dateien, die Websites auf diesem Gerät ablegen.
browser-settings-passwords = Passwörter
browser-settings-passwords-detail = Auf diesem Gerät gespeicherte Anmeldungen.
browser-settings-autofill = Adress-Autofill
browser-settings-autofill-detail = Füllt Adressformulare aus deinen gespeicherten Adressen.
browser-settings-privacy = Datenschutz und Sicherheit
browser-settings-windows = Fenster
browser-settings-private = Privates Surfen
browser-settings-private-detail = Aus einem privaten Fenster bleibt nach dem Schließen nichts erhalten.
browser-settings-search-engine-label = Suchmaschine
browser-settings-search-engine-detail = Suchen aus dem Adressfeld gehen dorthin.

## What the window shows
browser-quit-keep-downloading = Weiter herunterladen
browser-quit-cancel-downloads = Downloads abbrechen und beenden
browser-open = { $address } öffnen
browser-search-for = Nach { $query } suchen
browser-history-clear-failed = Der Verlauf wurde nicht gelöscht, weil Browser die Änderung nicht speichern konnte. Der bisherige Verlauf blieb erhalten.
browser-download-active-hint = Schließe ihn in Downloads in der Bibliothek ab oder brich ihn dort ab, und schließe dann den Tab.
browser-history-paused-hint = Seiten in diesem Fenster werden nicht im Verlauf gespeichert.
browser-service-sign-in-hint = Melde dich an, um dieses Konto fertig zu verbinden.

## A window without the person's accounts
browser-no-account-custody = Dieses Fenster surft ohne deine Anmeldungen. Öffne die Seite in einem eigenen Browser-Fenster, um dort deine Konten zu nutzen

## Actions

browser-address-book = Adressbuch
browser-effect-read-continuity = Importfortschritt lesen
browser-effect-open-service-session = Konto öffnen
browser-effect-retry-forget-service-session = Kontolöschung wiederholen
browser-effect-list-service-sessions = Konten auflisten
browser-effect-read-service-session = Konto lesen
browser-effect-retry-continuity = Import wiederholen
browser-effect-back = Zurück
browser-effect-forward = Vorwärts
browser-effect-navigate = Adresse öffnen
browser-effect-open-tab = In neuem Tab öffnen
browser-effect-open = Seite öffnen
browser-effect-show-page = Aktuelle Seite zeigen
browser-effect-show-history = Verlauf anzeigen
browser-effect-show-bookmarks = Lesezeichen anzeigen
browser-effect-show-downloads = Downloads anzeigen
browser-effect-show-extensions = Erweiterungen anzeigen
browser-effect-pin-extension = An Symbolleiste anheften
browser-effect-unpin-extension = Von Symbolleiste lösen
browser-effect-use-extension = Erweiterung verwenden
browser-effect-send-to-extension = Text an Erweiterung senden
browser-effect-open-extension-settings = Erweiterungseinstellungen
browser-effect-open-extension-side-panel = Seitenleiste öffnen
browser-effect-revoke-extension-access = Zugriff der Erweiterung entziehen
browser-effect-remove-extension = Erweiterung entfernen
browser-effect-show-settings = Browsereinstellungen
browser-effect-set-library-filter = Bibliothek filtern
browser-effect-edit-address = Gespeicherte Adresse bearbeiten
browser-effect-delete-address = Gespeicherte Adresse löschen
browser-effect-forget-visit = Seite vergessen
browser-effect-start-download = Adresse herunterladen
browser-effect-show-download = Download anzeigen
browser-effect-cancel-download = Download abbrechen
browser-effect-retry-download = Download wiederholen
browser-effect-open-download = Datei öffnen
browser-effect-read-download = Datei lesen
browser-effect-save-download = Download behalten
browser-effect-discard-download = Download verwerfen
browser-effect-send-held-request = Zurückgehaltene Anfrage senden
browser-effect-read-page = Seite lesen
browser-effect-capture-page = Seite aufnehmen
browser-effect-read-page-changes = Änderungen der Seite
browser-effect-read-page-element-at = Element an einer Stelle
browser-effect-wait-page-settled = Warten, bis die Seite geladen ist
browser-effect-read-page-console = Konsolenmeldungen
browser-effect-read-page-network = Netzwerkanfragen
browser-effect-find-page-element = Seitenelement finden
browser-effect-click-page = Element anklicken
browser-effect-hover-page = Zeiger über Element bewegen
browser-effect-drag-page = Element ziehen
browser-effect-pointer-path = Zeigerweg zeichnen
browser-effect-fill-page = Feld ausfüllen
browser-effect-fill-page-secret = Passwortfeld ausfüllen
browser-effect-select-page = Optionen wählen
browser-effect-fill-page-files = Dateien zum Hochladen wählen
browser-effect-press-page = Taste drücken
browser-effect-submit-page = Formular absenden
browser-effect-accept-page-dialog = Dialog bestätigen
browser-effect-dismiss-page-dialog = Dialog schließen
browser-effect-answer-permission = Berechtigungsanfrage beantworten
browser-effect-pointer-page = Zeiger bewegen
browser-effect-scroll-page = Seite scrollen
browser-effect-import-personal-bookmarks = Lesezeichen importieren
browser-effect-remove-personal-bookmark = Lesezeichen entfernen
browser-effect-remove-personal-imported-node = Importiertes Lesezeichen entfernen
browser-effect-show-personal-bookmark-folder = Lesezeichenordner öffnen
browser-browsing-history = Browserverlauf
browser-browsing-history-visit = Besuch vermerken
browser-browsing-history-import-history-bundle = Verlauf importieren
browser-browsing-history-name-visit = Seite benennen
browser-browsing-history-forget = Seite vergessen
browser-browsing-history-clear-history = Alle Seiten vergessen
browser-personal-bookmarks = Persönliche Lesezeichen
browser-personal-bookmarks-add-bookmark = Seite als Lesezeichen merken
browser-personal-bookmarks-remove-bookmark = Lesezeichen entfernen
browser-personal-bookmarks-import-bookmarks = Lesezeichen importieren
browser-personal-bookmarks-remove-imported-node = Importierten Ordner entfernen
browser-application = Browser
browser-application-add-bookmark = Seite als Lesezeichen merken
browser-application-remove-bookmark = Lesezeichen entfernen
browser-application-set-site-protection = Tracker-Schutz
