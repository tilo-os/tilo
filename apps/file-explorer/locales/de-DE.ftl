# Dateien — deutsche Übersetzung (de-DE).
# Variablen und Auswahlzweige müssen strukturell mit en-US.ftl übereinstimmen.

files-time-unknown = unbekannter Zeitpunkt

## Tastaturkurzbefehle
files-shortcuts-title = Tastaturkurzbefehle
files-shortcuts-row = { $action }: { $keys }
files-shortcuts-preview = Vorschau
files-shortcuts-open = Öffnen
files-shortcuts-back-forward = Zurück, vor
files-shortcuts-up = Übergeordneter Ordner
files-shortcuts-location = Zu einem Ort gehen
files-shortcuts-rename = Umbenennen
files-shortcuts-new-folder = Neuer Ordner
files-shortcuts-trash = In den Papierkorb
files-shortcuts-copy-cut-paste = Kopieren, ausschneiden, einfügen
files-shortcuts-undo-redo = Rückgängig, wiederholen
files-shortcuts-select-all = Alles auswählen
files-shortcuts-hidden = Versteckte Dateien zeigen
files-shortcuts-size = Größere, kleinere Elemente
files-shortcuts-refresh = Aktualisieren

## Verfügbarkeit: Aktionen und Status
files-opening-document = Dokument wird geöffnet…
files-document-opening-cancelled = Das Öffnen des Dokuments wurde abgebrochen
files-keep-offline = Offline verfügbar halten
files-use-automatic-storage = Automatische Speicherverwaltung verwenden
files-pinned-by-parent = Durch übergeordneten Ordner offline gehalten
files-retry-download = Download wiederholen
files-free-local-space = Lokalen Speicher freigeben
files-no-pin-needed = Kein ausgewähltes Element muss offline gehalten werden
files-no-direct-pin = Die direkte Offline-Einstellung kann hier nicht geändert werden
files-keeping-offline = { $count } { $count ->
    [one] Element wird
   *[other] Elemente werden
} offline verfügbar gehalten
files-using-automatic = Automatische Speicherverwaltung wird für { $count } { $count ->
    [one] Element
   *[other] Elemente
} verwendet
files-offline-policy-failed = Offline-Einstellung fehlgeschlagen: { $errors }
files-no-retryable-transfer = Keine ausgewählte Übertragung kann wiederholt werden
files-retrying-transfers = { $count } { $count ->
    [one] Übertragung wird
   *[other] Übertragungen werden
} wiederholt…
files-retry-failed = Wiederholung fehlgeschlagen: { $errors }
files-no-local-space-releasable = Kein ausgewähltes Element kann lokalen Speicher freigeben
files-freeing-local-space = Lokaler Speicher für { $count } { $count ->
    [one] Element wird
   *[other] Elemente wird
} freigegeben…
files-free-local-space-failed = Freigeben des lokalen Speichers fehlgeschlagen: { $errors }
files-eviction-needs-confirmation = Die Sicherheit konnte nicht bestätigt werden; eine Bestätigung ist erforderlich
files-destructive-eviction-title = Eine nicht abgesicherte lokale Kopie entfernen?
files-destructive-eviction-summary = Für { $count } ausgewählte { $count ->
    [one] Datei konnte keine andere aktuelle, dauerhafte Kopie
   *[other] Dateien konnte keine andere aktuelle, dauerhafte Kopie
} nachgewiesen werden.
files-destructive-eviction-no-replica = Es konnte kein zulässiges aktuelles Replikat bestätigt werden.
files-destructive-eviction-loss = Wenn keine Quelle verbleibt, kann der aktuelle Dateiinhalt dauerhaft nicht mehr verfügbar sein.
files-destructive-eviction-protected = Aufbewahrte Versionen und von geschützten Dateien gemeinsam verwendete Daten werden nicht entfernt.
files-destructive-eviction-confirm = Lokale Kopie trotzdem entfernen
files-destructive-eviction-started = Erzwungenes lokales Entfernen für { $count } { $count ->
    [one] Element
   *[other] Elemente
} gestartet.
files-permanent-delete-title = { $count } { $count ->
    [one] Element
   *[other] Elemente
} endgültig löschen?
files-permanent-delete-summary = { $count ->
    [one] 1 ausgewähltes Element wird aus dem Papierkorb entfernt.
   *[other] Die ausgewählten { $count } Elemente werden aus dem Papierkorb entfernt.
}
files-permanent-delete-loss = Dies kann nicht rückgängig gemacht werden.
files-permanent-delete-confirm = Endgültig löschen
files-empty-trash-title = Papierkorb endgültig leeren?
files-empty-trash-summary = { $count ->
    [one] 1 Element wird aus dem Papierkorb entfernt.
   *[other] Alle { $count } Elemente werden aus dem Papierkorb entfernt.
}
files-empty-trash-confirm = Papierkorb leeren

## Eigenschaften und Verfügbarkeit
files-property-collaborative-record = Kollaborativer Datensatz: { $record }
files-property-content = Inhalt: { $hash }…
files-residency-metadata-only = nur Metadaten
files-residency-partly-local = teilweise lokal ({ $bytes } Byte)
files-residency-local = lokal ({ $bytes } Byte)
files-property-on-device = Auf diesem Gerät: { $state }
files-property-sharing = Freigabe: { $count } aktive { $count ->
    [one] Berechtigung
   *[other] Berechtigungen
}
files-property-history = Verlauf: { $versions } { $versions ->
    [one] Version
   *[other] Versionen
}, { $conflicts } offene { $conflicts ->
    [one] Konflikt
   *[other] Konflikte
}
files-properties-collab-summary = Datei- und Freigabedetails
files-availability-title = Verfügbarkeit von { $name }
files-availability-title-short = Verfügbarkeit…
files-not-stored = Nicht auf diesem Gerät gespeichert
files-partly-stored = Teilweise gespeichert ({ $bytes } Byte)
files-stored = Auf diesem Gerät gespeichert ({ $bytes } Byte)
files-kept-offline-parent = Durch einen übergeordneten Ordner offline gehalten
files-kept-offline = Offline verfügbar gehalten
files-automatic-storage = Automatische Speicherverwaltung
files-replica-unverified = Noch unbekannt, ob andere Geräte eine Kopie haben
files-replica-counts = Auf { $durable } { $durable ->
    [one] anderen Gerät
   *[other] anderen Geräten
} gespeichert
files-transfer-state = Übertragung: { $state }
files-transfer-idle = inaktiv
files-transfer-queued = eingereiht
files-transfer-downloading = wird heruntergeladen
files-transfer-uploading = wird hochgeladen
files-transfer-verifying = wird verifiziert

## Freigaben
files-search-collaborative-records = Kollaborative Dateien
files-search-record-provenance = { $path }
files-copy-one-time-link = Einmaligen Freigabelink kopieren
files-import-once = Einmalig in lokalen Ordner importieren…
files-cloud-title = Cloud-Dateien
files-account-import-only = Nur Import
files-cloud-enumerating = Ordner werden gelesen
files-cloud-exporting = Dokument wird exportiert
files-cloud-copying = Kopieren
files-cloud-done = Abgeschlossen
files-cloud-cancelled = Abgebrochen
files-cloud-failed = Fehlgeschlagen
files-cloud-preparing = Vorbereitung
files-cloud-independent = Dateien oder Ordner auswählen, dann einen lokalen Zielordner wählen. Importierte Kopien sind bearbeitbar und unabhängig; Cloud-Originale bleiben unverändert.
files-cloud-root = Alle Dateien
files-cloud-converted = Google-Dokument. Wird als .docx importiert; die Konvertierung kann die Formatierung verändern.
files-cloud-converted-sheet = Google-Tabelle. Wird als .xlsx importiert; die Konvertierung kann die Formatierung verändern.
files-cloud-converted-slides = Google-Präsentation. Wird als .pptx importiert; die Konvertierung kann die Formatierung verändern.
files-cloud-loading = Cloud-Ordner wird gelesen…
files-cloud-busy = Eine Cloud-Anfrage läuft bereits.
files-cloud-invalid-reply = Der Cloud-Dienst hat eine unerwartete Antwort gesendet.
files-cloud-selected = Ausgewählt: { $name }
files-cloud-select = Auswählen
files-cloud-more = Weitere laden
files-cloud-import = Einmal importieren…
files-cloud-destination = Lokalen Zielordner auswählen
files-cloud-progress = { $phase }: { $count } Dateien, { $bytes } Bytes
files-cloud-cancel = Import abbrechen
files-cloud-retry = Fehlgeschlagene wiederholen
files-cloud-reveal = Zielordner anzeigen
files-import-source-unavailable = Wähle einen Eintrag in einem eingehängten Dropbox- oder Google-Drive-Konto.
files-import-local-destination = Ein einmaliger Import benötigt einen lokalen Zielordner, keinen synchronisierten Cloud-Ordner.
files-share-composer-named = Für { $person } freigeben ({ $permission }, { $expiry })
files-share-composer-link = Link kopieren ({ $permission }, { $expiry }, { $uses })
files-share-choice-view = Ansehen
files-share-choice-edit = Bearbeiten
files-share-choice-day = läuft in 24 Stunden ab
files-share-choice-week = läuft in 7 Tagen ab
files-share-choice-month = läuft in 30 Tagen ab
files-share-choice-never = ohne Ablaufdatum
files-share-choice-one-use = einmal verwendbar
files-share-choice-bounded-uses = bis zu { $count } Verwendungen
files-share-creating = Freigabe wird erstellt…
files-share-invalid-options = Diese Freigabeoptionen können nicht kombiniert werden
files-share-link-needs-expiry = Freigabelinks benötigen ein Ablaufdatum
files-share-invalid-uses = Freigabelinks benötigen ein Verwendungslimit von 1 bis 1.000
files-share-recipient-unreachable = Keins der Geräte dieser Person kann gerade Dateien empfangen
files-sharing-access = Freigabe und Zugriff…
files-share-with = Für { $person } freigeben
files-revoke-subject = Zugriff für { $subject } widerrufen
files-shared-link = freigegebener Link
files-share-link-pending = Ein Freigabelink wird bereits erstellt
files-share-change-pending = Eine Freigabeänderung wird bereits ausgeführt
files-select-collaborative-file = Zuerst eine kollaborative Datei auswählen
files-select-shared-file = Zuerst die freigegebene Datei auswählen
files-sharing-unavailable = Freigaben sind für diesen Arbeitsbereich nicht verfügbar
files-creating-one-use-link = Einmaliger Link wird erstellt…
files-share-with-progress = Freigabe für { $person } wird erstellt…
files-revoking-access = Zugriff wird widerrufen…
files-not-shared = Für niemanden freigegeben
files-anyone-link = Jeder mit dem Link
files-can-view = kann ansehen
files-can-edit = kann bearbeiten
files-grant-active = aktiv
files-grant-expired = abgelaufen
files-grant-revoked = widerrufen
files-grant-exhausted = aufgebraucht
files-no-expiry = läuft nicht ab
files-expires = läuft am { $time } ab
files-share-grant-line = { $subject }: { $permission }, { $status }, { $expiry }
files-sharing-title = { $name } freigeben
files-certified-devices = { $person }, Geräte: { $devices }
files-certified-device-detail = { $device }, Schlüssel { $fingerprint }, { $capabilities }, ausgestellt { $issued }, { $expiry }
files-device-cap-own = verwaltet Arbeitsbereiche
files-device-cap-admit = lässt Geräte zu
files-device-cap-host = vertrauenswürdiger Host
files-device-cap-read-all = liest alle Arbeitsbereiche
files-device-cap-member = Arbeitsbereichsmitglied
files-sharing-summary = { $count } aktive { $count ->
    [one] Berechtigung
   *[other] Berechtigungen
}; kopierte Daten können nicht zurückgerufen werden
files-link-copied = Freigabelink in die Zwischenablage kopiert
files-share-created = Für { $person } freigegeben; heruntergeladene Kopien können nicht zurückgerufen werden
files-share-created-pending = Für { $person } freigegeben; Zustellung steht aus
files-share-created-delivered = Für { $person } freigegeben; Zustellung abgeschlossen
files-access-revoked = Zugriff widerrufen; bereits heruntergeladene Daten können nicht zurückgerufen werden
files-share-inactive = Diese Freigabe war bereits inaktiv

## Verlauf und Konfliktlösung
files-version-history = Versionsverlauf…
files-restore-previous = Vorherige Version wiederherstellen
files-resolve-keep-current = { $conflict } lösen — Aktuelle behalten
files-resolve-use-other = { $conflict } lösen — Andere verwenden (Gerät { $device })
files-history-version-line = { $action } { $time } von { $person } auf Gerät { $device }
files-history-older = Ältere Versionen sind noch nicht auf diesem Gerät
files-conflict-needs-review = Überprüfung erforderlich
files-conflict-resolved-strategy = Gelöst: { $strategy }
files-conflict-resolved = Gelöst
files-strategy-keep-current = Aktuelle behalten
files-strategy-use-other = Andere verwenden
files-strategy-keep-both = Beide behalten
files-strategy-merge = Zusammenführen
files-conflict-line = Konflikt { $conflict }: { $count } { $count ->
    [one] Version
   *[other] Versionen
}, { $state }
files-conflict-resolve-hint = Über das Kontextmenü lösen: Aktuelle behalten oder Andere verwenden
files-conflict-unsupported-hint = „Beide behalten“ und „Zusammenführen“ sind für diese Datei noch nicht verfügbar
files-history-title = Versionsverlauf von { $name }
files-history-summary = { $versions } { $versions ->
    [one] Version
   *[other] Versionen
}, { $heads } aktuelle { $heads ->
    [one] Spitze
   *[other] Spitzen
}
files-history-change-pending = Eine Verlaufsänderung wird bereits ausgeführt
files-select-file-with-history = Zuerst eine kollaborative Datei mit Verlauf auswählen
files-version-restored = Version als neuer Verlaufseintrag wiederhergestellt
files-restoring-version = Diese Version wird wiederhergestellt…
files-keeping-current = Die aktuelle Version wird beibehalten…
files-applying-other = Die andere Version wird angewendet…
files-merging-versions = Nicht überlappende Änderungen werden sicher zusammengeführt…
files-merge-review-menu = Zusammenführung prüfen
files-merge-review-title = Zusammenführung von { $name } prüfen
files-merge-review-summary = Es wurden noch keine Änderungen vorgenommen
files-merge-review-checking = Die beiden Versionen werden verglichen …
files-merge-review-checking-detail = Währenddessen ändert sich nichts.
files-merge-review-safe = Sicher zusammenführbar
files-merge-review-needs-review = Prüfung nötig. Es wird nichts zusammengeführt.
files-merge-review-unavailable = Die Versionen lassen sich gerade nicht vergleichen. Aktualisiere die Datei und vergleiche sie im Versionsverlauf.
files-merge-review-no-apply = Behalte beide oder wähle eine, nachdem du sie verglichen hast.
files-merge-review-incoming = Eingehende Version von Gerät { $device }
files-merge-review-diff = Der Versionsverlauf zeigt beide Versionen nebeneinander.
files-merge-review-guard = Ändert sich eine der Versionen vor dem Anwenden, wird nichts zusammengeführt.
files-merge-review-undo = Du kannst danach jede frühere Version wiederherstellen, ohne spätere Arbeit zu verlieren.
files-merge-apply-safe = Sichere Zusammenführung anwenden
files-merge-apply-safe-detail = Wird nur angewendet, wenn beide Versionen noch vollständig erhalten bleiben.
files-conflict-resolved-signed = Konflikt gelöst
files-history-change-saved = Verlaufsänderung gespeichert
files-history-action-created = Erstellt
files-history-action-edited = Bearbeitet
files-history-action-renamed = Umbenannt
files-history-action-moved = Verschoben
files-history-action-deleted = Gelöscht
files-history-action-conflict-resolved = Konflikt gelöst
files-history-action-restored = Wiederhergestellt
files-history-action-changed = Geändert
files-history-current-version = { $action } (aktuelle Version)
files-history-preview-title = Ausgewählte Version
files-history-compare-title = Mit aktueller Version vergleichen
files-history-selected-action = Aktion: { $action }
files-history-selected-by = Von: { $person }
files-history-selected-at = Zeitpunkt: { $time }
files-history-version-id = Version: { $id }
files-history-content-identity = Inhaltsidentität: { $hash }
files-history-metadata-identity = Metadatenidentität: { $hash }
files-history-no-content = kein Dateiinhalt
files-history-compare-content = Inhalt: { $state }
files-history-compare-metadata = Metadaten: { $state }
files-history-same = unverändert
files-history-different = geändert
files-history-show-activity = In Workspace-Aktivität anzeigen
files-history-compare = Mit aktueller Version vergleichen
files-history-hide-compare = Versionsdetails anzeigen
files-history-restore-selected = Diese Version wiederherstellen
files-history-current-version-label = Aktuelle Version

## Barrierefreier kollaborativer Status
files-a11y-transfer-failed = Übertragung fehlgeschlagen
files-a11y-transfer-queued = Übertragung eingereiht
files-a11y-downloading = Wird heruntergeladen
files-a11y-uploading = Wird hochgeladen
files-a11y-verifying = Wird verifiziert
files-a11y-online-only = Nur online
files-a11y-partly-offline = Teilweise offline verfügbar
files-a11y-available-offline = Offline verfügbar
files-a11y-kept-offline-parent = Durch übergeordneten Ordner offline gehalten
files-a11y-kept-offline = Offline verfügbar gehalten
files-a11y-replica-unknown = Kopien auf anderen Geräten unbekannt
files-a11y-durably-replicated = Auf anderen Geräten gespeichert
files-a11y-not-durably-replicated = Noch nicht auf anderen Geräten gespeichert
files-a11y-shared-recipients = Für { $count } { $count ->
    [one] Empfänger
   *[other] Empfänger
} freigegeben
files-a11y-version-conflict = Hat einen Versionskonflikt

## Dienst- und Empfangsfehler/status
files-open-terminal-failed = Terminal konnte nicht geöffnet werden: { $error }
files-conflict-strategy-unsupported = Diese Konfliktstrategie wird von Dateien noch nicht unterstützt
files-conflict-encode-failed = Konfliktanfrage konnte nicht kodiert werden
files-conflict-request-too-large = Dieser Konflikt lässt sich hier nicht lösen. Versuch es im Versionsverlauf.
files-conflict-resolve-failed = Konflikt konnte nicht gelöst werden: { $error }
files-share-wrong-response = Freigabedienst hat die falsche Antwort zurückgegeben
files-share-unavailable-error = Freigabedienst nicht verfügbar: { $error }
files-share-encode-failed = Freigabeanfrage konnte nicht kodiert werden: { $error }
files-share-request-too-large = Zu viele Personen auf einmal. Teile mit weniger gleichzeitig.
files-share-request-failed = Link konnte nicht angefordert werden: { $error }
files-share-flush-failed = Freigabeanfrage konnte nicht abgeschlossen werden: { $error }
files-share-no-response = Freigabedienst hat nicht geantwortet: { $error }
files-share-response-too-large = Freigabedienst hat eine zu große Antwort zurückgegeben
files-share-response-truncated = Antwort des Freigabedienstes wurde abgeschnitten: { $error }
files-share-response-invalid = Ungültige Antwort des Freigabedienstes: { $error }
files-share-refused = Freigabedienst hat die Anfrage abgelehnt ({ $code })
files-received-refused = Empfangsdienst hat den Link abgelehnt ({ $code })
files-received-wrong-response = Empfangsdienst hat die falsche Antwort zurückgegeben
files-received-encode-failed = Empfangsanfrage konnte nicht kodiert werden
files-received-request-too-large = Diese geteilte Datei kann nicht empfangen werden.
files-received-unavailable = Empfangsdienst ist nicht verfügbar
files-received-no-response = Empfangsdienst hat nicht geantwortet
files-received-response-too-large = Empfangsdienst hat eine zu große Antwort zurückgegeben
files-received-response-truncated = Antwort des Empfangsdienstes wurde abgeschnitten
files-received-response-invalid = Empfangsdienst hat eine ungültige Antwort zurückgegeben
files-received-not-available = Empfangsdienst ist nicht verfügbar
files-received-fd-clone-failed = Die geteilte Datei konnte nicht empfangen werden. Versuch es erneut.
files-received-fd-invalid = Die geteilte Datei konnte nicht empfangen werden. Versuch es erneut.
files-received-file = Freigegebene Datei empfangen
files-received-mount-starting = Freigegebene Datei angenommen; Einbindung wird gestartet
files-received-admitted = Freigegebene Datei angenommen
files-received-link-read-failed = Link zur freigegebenen Datei konnte nicht gelesen werden
files-received-link-too-large = Dieser Link ist zu lang zum Öffnen.
files-received-link-invalid-text = Link zur freigegebenen Datei ist kein gültiger Text
files-received-link-empty = Link zur freigegebenen Datei ist leer

## Anwendungsrahmen und Navigation
files-app-title = Dateien
files-selection-empty = Wähle zuerst Dateien aus.
files-clipboard-source-unavailable = Eine ausgewählte Datei wurde geändert oder ist nicht mehr verfügbar. Wähle sie vor dem Kopieren erneut aus.
files-clipboard-source-changed = Das angezeigte Objekt unterscheidet sich von der gespeicherten Auswahl. Wähle es vor dem Kopieren erneut aus.
files-clipboard-unavailable = Kopieren und Einfügen ist gerade nicht verfügbar.
files-clipboard-empty = Die Datei-Zwischenablage des Arbeitsbereichs ist leer.
files-clipboard-undone = Einfügen rückgängig gemacht.
files-selection-unavailable = Dieses Element ist nicht mehr in diesem Ordner.
files-selection-too-large = Wähle höchstens 256 Elemente pro Anfrage aus.
files-copy-too-large = Kopiere bei mehr als 256 Elementen aus einem Ordner nach dem anderen.
files-back = Zurück
files-forward = Vor
files-up = Einen Ordner nach oben
files-view-list = Liste
files-view-grid = Raster
files-sort-by = Sortieren nach
files-show-as-grid = Als Raster anzeigen
files-show-as-list = Als Liste anzeigen
files-location = Ort
files-view-options = Ansichtsoptionen
files-path-placeholder = Pfad eingeben, z. B. ~/Dokumente
files-place-home = Persönlicher Ordner
files-place-favorites = Favoriten
files-place-system = Orte
files-place-root = Wurzelverzeichnis
files-place-trash = Papierkorb
files-place-drives = Laufwerke
files-drive-unnamed-internal = Laufwerk in diesem Computer
files-drive-unnamed-external = Externes Laufwerk
files-drive-internal = In diesem Computer
files-drive-external = Externes Laufwerk
files-drive-open = Geöffnet
files-drive-open-action = Laufwerk öffnen
files-drive-gone = Dieses Laufwerk ist nicht mehr verbunden.
files-drive-open-failed = Das Laufwerk konnte nicht geöffnet werden: { $error }
files-trash-unavailable = Der Papierkorb ist auf diesem Gerät nicht verfügbar.
files-place-workspace = Arbeitsbereich
files-place-documents = Dokumente
files-place-downloads = Downloads
files-place-pictures = Bilder
files-place-music = Musik
files-place-videos = Videos

## Liste, Leer-/Fehlerstatus und Barrierefreiheit
files-column-name = Name
files-column-size = Größe
files-column-modified = Geändert
files-try-again = Erneut versuchen
files-loading = Wird geladen…
files-loading-folder = { $path } wird geladen
files-loading-title = Ordner wird geöffnet
files-loading-detail = { $path } wird geladen
files-directory-read-failed = Dieser Ordner konnte nicht geöffnet werden: { $error }
files-load-error-title = „{ $name }“ konnte nicht geöffnet werden
files-folder-error-permission = Dir fehlt die Berechtigung, den Inhalt zu sehen.
files-folder-error-missing = Er wurde verschoben oder gelöscht.
files-open-enclosing = Übergeordneten Ordner öffnen
files-a11y-load-error = Dieser Ordner konnte nicht geöffnet werden: { $error }
files-a11y-panel-details = { $title }. { $summary }. { $details }. Zum Schließen Escape drücken
files-a11y-dialog-close = { $summary }. Zum Schließen Escape drücken
files-a11y-activity = Dateiaktivität
files-a11y-current-folder = Aktueller Ordner: { $path }
files-a11y-places = Orte
files-a11y-favourite = Favorit
files-a11y-system-location = Systemort
files-a11y-place-description = { $section }, { $path }
files-a11y-current-breadcrumb = Aktueller Ordner, { $path }. Zum Bearbeiten des Pfads Eingabe drücken
files-a11y-open-folder = { $path } öffnen
files-empty-trash = Der Papierkorb ist leer
files-empty-folder = Dieser Ordner ist leer
files-item-count = { $count } { $count ->
    [one] Element
   *[other] Elemente
}
files-selected-count = { $base }, { $count } ausgewählt
files-a11y-list = Dateien
files-a11y-folder = Ordner
files-a11y-file = Datei, { $size }
files-new-name-placeholder = Neuer Name

## Eigenschaften
files-property-type = Typ: { $type }
files-property-location = Ort: { $location }
files-property-size = Größe: { $size }
files-property-modified = Geändert: { $time }
files-property-dimensions = Abmessungen: { $width } × { $height } px
files-property-duration = Dauer: { $duration }
files-property-pages = Seiten: { $count }
files-property-codec = Codec: { $codec }
files-property-container = Container: { $container }
files-property-unknown = Unbekannt
files-properties-title = Eigenschaften von { $name }
files-properties-summary = Dateisystemmetadaten

## Ergebnisse gewöhnlicher Vorgänge
files-new-folder-default-name = Neuer Ordner
files-permanently-deleted = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} endgültig gelöscht
files-delete-failed = Löschen fehlgeschlagen: { $error }
files-moved-to-trash = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} in den Papierkorb verschoben
files-restored = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} wiederhergestellt
files-restore-failed = Wiederherstellung fehlgeschlagen: { $error }
files-trash-already-empty = Der Papierkorb ist bereits leer
files-emptied-trash = Papierkorb geleert ({ $count })
files-empty-trash-failed = Leeren des Papierkorbs fehlgeschlagen: { $error }
files-created-file = Datei erstellt
files-new-file-failed = Neue Datei fehlgeschlagen: { $error }
files-added-favourite = Zu Favoriten hinzugefügt
files-removed-favourite = Aus Favoriten entfernt
files-moved-to-folder = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} nach „{ $destination }“ verschoben
files-nothing-to-move = Nichts zu verschieben

## Kontext-, Ansichts- und Sortiermenüs
files-menu-open = Öffnen
files-action-reveal = In Dateien anzeigen
files-menu-restore = Wiederherstellen
files-menu-delete-permanently = Endgültig löschen
files-menu-empty-trash = Papierkorb leeren
files-menu-rename = Umbenennen
files-menu-cut = Ausschneiden
files-menu-copy = Kopieren
files-menu-properties = Eigenschaften…
files-menu-paste = Einfügen
files-menu-new-folder = Neuer Ordner
files-menu-new-file = Neue Datei  ›
files-menu-move-trash = In den Papierkorb verschieben
files-menu-add-favourite = Zu Favoriten hinzufügen
files-menu-remove-favourite = Aus Favoriten entfernen
files-menu-hide-hidden = Verborgene Dateien ausblenden
files-menu-show-hidden = Verborgene Dateien anzeigen
files-menu-sort-name = Nach Name sortieren
files-menu-sort-size = Nach Größe sortieren
files-menu-sort-type = Nach Typ sortieren
files-menu-sort-date = Nach Datum sortieren
files-menu-open-terminal = Terminal hier öffnen
files-sort-name = Name
files-sort-size = Größe
files-sort-type = Typ
files-sort-date = Datum
files-sort-ascending = Aufsteigend
files-sort-descending = Absteigend
files-new-from-template = Neu: { $name }
files-drag-items = { $count } Elemente

## Archive
files-archive-extract = Archiv entpacken…
files-archive-extract-here = Hier entpacken
files-archive-compress = Als ZIP komprimieren
files-archive-select-compress = Wähle Dateien oder Ordner zum Komprimieren aus
files-archive-read-only = Archive können erst nach dem Entpacken geändert werden
files-archive-select = Wähle zum Entpacken ein ZIP-, TAR- oder TAR.GZ-Archiv aus
files-archive-extracted =
    { $count ->
        [one] 1 Datei entpackt
       *[other] { $count } Dateien entpackt
}
files-archive-extracted-partial = { $count } Dateien entpackt; { $failures }
files-archive-extract-failed = Archiv konnte nicht entpackt werden: { $error }
files-archive-created = { $name } erstellt
files-archive-create-failed = Archiv konnte nicht erstellt werden: { $error }

## Validierung von Dateivorgängen
files-folder-name-empty = Ordnername darf nicht leer sein
files-folder-name-slash = Ordnername darf kein „/“ enthalten
files-name-empty = Name darf nicht leer sein
files-name-slash = Name darf kein „/“ enthalten
files-no-parent = Kein übergeordneter Ordner
files-name-exists = „{ $name }“ ist bereits vorhanden
files-destination-inspect-failed = „{ $path }“ konnte nicht geprüft werden: { $error }
files-destination-names-exhausted = Zu viele Dateien mit dem Namen „{ $name }“ in „{ $path }“. Wähle einen anderen Namen oder Ordner.
files-source-no-name = Quelle hat keinen Dateinamen
files-menu-send-nearby = An Gerät in der Nähe senden…
files-menu-run-shortcut = Kurzbefehl ausführen
files-shortcut-running = „{ $shortcut }“ wird gestartet …
files-shortcut-started = „{ $shortcut }“ läuft
files-shortcut-failed = „{ $shortcut }“ wurde nicht ausgeführt: { $reason }
files-action-send-nearby-device = An { $device } senden
files-nearby-send-to-device = An { $device } senden
files-nearby-select-one = Wähle eine Datei oder einen Ordner zum Senden aus.
files-nearby-loading-devices = Deine Geräte werden gesucht …
files-nearby-no-devices = Keins deiner Geräte ist in der Nähe.
files-nearby-choose-device = Wähle ein Gerät.
files-nearby-queueing = Senden wird vorbereitet …
files-nearby-accepted = Angenommen.
files-nearby-declined = Abgelehnt.
files-nearby-cancelled = Senden abgebrochen.
files-nearby-retrying = Wird erneut gesendet …
files-nearby-accept = Annehmen
files-nearby-retry = Erneut versuchen
files-nearby-consent-detail = Nichts wird gespeichert, bevor du annimmst.
files-nearby-retry-detail = Die Datei ist noch da, das Senden kann dort weitermachen, wo es aufgehört hat.
files-nearby-offer-title = Senden von { $device }
files-nearby-offer-summary = { $name } empfangen?
files-nearby-offer-size = Übertragungsgröße: { $bytes } Bytes
files-nearby-failed-title = Senden fehlgeschlagen
files-nearby-failed-device = Gerät: { $device }
files-nearby-choose-destination = Speicherort wählen
files-nearby-no-destinations = Es gibt keinen Ordner, in dem das gespeichert werden kann.
files-nearby-destination-count = Wähle aus { $count } Ordnern.
files-nearby-save-to-destination = In { $destination } speichern
files-nearby-save-and-remember = In { $destination } speichern und für diese Sitzung merken

files-share-not-available = Teilen ist auf diesem Gerät nicht verfügbar

files-name-invalid = Gib einen gültigen Datei- oder Ordnernamen ein

## Öffnen mit und Zuletzt verwendet
files-menu-open-with = Öffnen mit
files-open-with-other = Andere Anwendung…
files-open-with-none = Keine Anwendung öffnet diese Datei
files-open-with-default = { $app } (Standard)
files-open-with-default-badge = Standard
files-open-with-access-param = Zugriff auf die Originaldatei
files-open-with-launch-unavailable = Diese Anwendung kann die Originaldatei gerade nicht öffnen.
files-open-with-busy = Warte, bis die aktuelle Datei geöffnet ist, bevor du eine weitere öffnest.
files-open-with-action-unavailable = Wähle „Öffnen mit“ erneut für die aktuelle Datei.
files-open-with-write-required = Du brauchst die Berechtigung, die Originaldatei zu bearbeiten.
files-open-with-default-unavailable = Öffne diese Originaldatei einmal, ohne ihre Standardanwendung zu ändern.
files-open-with-access-unavailable = Diese Anwendung kann den gewählten Zugriff auf die Originaldatei nicht nutzen.
files-open-with-selection-changed = Die ausgewählte Datei ist nicht mehr verfügbar. Wähle sie erneut aus.
files-open-with-timeout = Das Öffnen der Originaldatei hat zu lange gedauert. Versuche es erneut.
files-open-with-once-group = Einmal öffnen
files-open-with-always-group = Immer verwenden für { $type }
files-open-with-search = Anwendung für { $name } wählen
files-open-with-app = Mit { $app } öffnen
files-open-with-always-app = { $type } immer mit { $app } öffnen
files-open-with-app-param = Anwendung
files-open-with-always-param = Außerdem als Standard für { $type } festlegen
files-open-with-now-default = { $app } öffnet jetzt { $type }
files-open-with-unavailable = Die Anwendungswahl ist hier nicht verfügbar ({ $name })
files-open-with-not-a-candidate = { $app } öffnet diese Datei nicht
files-place-recent = Zuletzt verwendet
files-empty-recent = Noch keine zuletzt verwendeten Dateien
files-recent-reveal = Enthaltenden Ordner öffnen
files-recent-forget = Aus „Zuletzt verwendet“ entfernen
files-recent-action-unavailable = Dieser zuletzt verwendete Eintrag ist nicht mehr verfügbar. Öffne „Zuletzt verwendet“ und wähle einen aktuellen Eintrag.
files-recent-not-a-folder = „Zuletzt verwendet“ ist kein Ordner; öffne einen, um Dateien hinzuzufügen

## Cloud-Konten
files-account-sign-in-title = Bei { $account } anmelden
files-account-sign-in-needed = { $service } unter { $host } braucht eine neue Anmeldung, bevor dieser Ordner synchronisiert werden kann.
files-account-sign-in-where = Das geht in den Einstellungen unter „Externe Konten“.
files-account-service-at-host = { $service } unter { $host }
files-account-folder = Ordner: { $path }
files-account-check-again = Erneut prüfen
files-account-check-again-detail = Liest den Zustand des Kontos neu und öffnet seinen Ordner, sobald er verfügbar ist.
files-action-send-airdrop = An überprüften AirDrop-Laborempfänger senden…
files-airdrop-unavailable = Eine überprüfte AirDrop-Laborübertragung ist auf diesem Gerät nicht verfügbar.
files-airdrop-endpoint-invalid = Die IP-Adresse des Empfängers und einen TLS-Port von 1 bis 65535 eingeben.
files-airdrop-source-unavailable = Diese Datei ist nicht mehr verfügbar.
files-airdrop-file-only = Eine reguläre Datei für die AirDrop-Laborübertragung auswählen.
files-airdrop-verifying = Empfängerüberprüfung und Erlaubnis zum Senden dieser Datei werden erwartet.
files-airdrop-address = IP-Adresse des Empfängers
files-airdrop-port = TLS-Port des Empfängers
files-airdrop-action-detail = Du bestätigst den Empfänger, bevor die Datei gesendet wird.

files-nearby-status = Übertragungsstatus
files-nearby-transfer-unavailable = Diese Übertragung ist in diesem Dateienfenster nicht mehr verfügbar.
files-nearby-status-detail = Phase und bestätigte Byteanzahl dieser Übertragung anzeigen.

files-clipboard-recovered = { $count } unterbrochene Übertragungen wiederhergestellt. Wiederherstellbare Kopien sind im Papierkorb.
files-menu-recover-transfers = Unterbrochene Übertragungen wiederherstellen
files-clipboard-redone = Einfügen wiederhergestellt.

files-clipboard-cancelled = Dateiübertragung abgebrochen. Unterbrochene Übertragungen vor einem erneuten Versuch wiederherstellen.

files-invalid-destination-name = Das Ziel muss ein einzelner Dateiname sein.

files-native-restore-select-one = Wähle einen Eintrag im persönlichen Papierkorb zum Wiederherstellen. Andere Einträge bleiben unverändert.

## History: what each change to Files did

file-explorer-add-bookmark = { $place } zu den Favoriten hinzugefügt
file-explorer-remove-bookmark = { $place } aus den Favoriten entfernt
file-explorer-new-folder = Ordner { $name } erstellt
file-explorer-rename = { $entry } in { $name } umbenannt
file-explorer-trash = Dateien in den Papierkorb verschoben
file-explorer-move = Dateien nach { $to } verschoben
file-explorer-copy = Dateien nach { $to } kopiert
file-explorer-revert = Eine Dateiänderung zurückgenommen
file-explorer-write-file = Datei { $name } geschrieben
file-explorer-compress = Dateien in ein Archiv gepackt

files-native-trash-delete-unavailable = Diese Objekte können wiederhergestellt werden. Dauerhaftes Löschen ist hier nicht verfügbar.
files-user-trash-unavailable = Dein Papierkorb ist in diesem Arbeitsbereich nicht verfügbar.

files-mounted-converted = Schreibgeschütztes konvertiertes Cloud-Dokument
files-mounted-source = { $source }, eine schreibgeschützte konvertierte Ansicht
files-mounted-readonly = Schreibgeschützt konvertiert
files-mounted-open = Schreibgeschützte Konvertierung öffnen
files-mounted-copy = Unabhängige bearbeitbare Kopie erstellen…
files-mounted-independent = Änderungen an einer unabhängigen lokalen Kopie ändern das Cloud-Original nicht.
file-explorer-import-mounted = Unabhängige bearbeitbare Kopien in { $folder } erstellt

## Hinweise
files-notice-undo = Rückgängig
files-notice-open-with = Öffnen mit …
files-notice-dismiss = Schließen
files-menu-undo = Rückgängig
files-open-failed = „{ $name }“ konnte nicht geöffnet werden
files-open-unavailable = Dateien kann in diesem Fenster keine Dokumente öffnen.
files-open-no-answer = Die Arbeitsumgebung hat nicht geantwortet.
files-new-folder-failed-title = Der Ordner konnte nicht erstellt werden
files-rename-failed-title = Umbenennen nicht möglich
files-trash-failed = Verschieben in den Papierkorb nicht möglich
files-move-failed = Nicht alles konnte nach „{ $destination }“ verschoben werden
files-paste-failed = { $count } von { $total } { $total ->
    [one] Element konnte
   *[other] Elementen konnten
} nicht eingefügt werden
files-clipboard-partial = Nicht alles konnte eingefügt werden
files-error-permission = Dafür fehlt die Berechtigung.
files-error-not-found = Es existiert nicht mehr.
files-error-busy = Es wird gerade verwendet.
files-error-exists = Es gibt bereits etwas mit diesem Namen.
files-error-no-space = Es ist nicht genug Speicherplatz frei.
files-error-read-only = Dieser Ort ist schreibgeschützt.
files-error-not-empty = Der Ordner ist nicht leer.
files-notice-cancel = Abbrechen
files-copying = { $count } { $count ->
    [one] Element wird
   *[other] Elemente werden
} nach „{ $folder }“ kopiert
files-moving = { $count } { $count ->
    [one] Element wird
   *[other] Elemente werden
} nach „{ $folder }“ verschoben
files-transfer-progress = { $done } von { $total }
files-transfer-stopped = Angehalten
files-clash-title-one = „{ $name }“ gibt es in „{ $folder }“ schon
files-clash-title-many = { $count } Elemente gibt es in „{ $folder }“ schon
files-clash-detail = Beim Ersetzen kommen die vorhandenen Elemente in den Papierkorb.
files-clash-skip = Überspringen
files-clash-keep-both = Beide behalten
files-clash-replace = Ersetzen
files-preview-detail = { $what }, geändert { $modified }
files-drop-nowhere = Öffne einen Ordner, um Dateien hineinzulegen.
