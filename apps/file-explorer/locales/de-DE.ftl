# Dateien — deutsche Übersetzung (de-DE).
# Variablen und Auswahlzweige müssen strukturell mit en-US.ftl übereinstimmen.

files-time-unknown = unbekannter Zeitpunkt

## Tastaturkurzbefehle
files-shortcuts-title = Tastaturkurzbefehle
files-shortcuts-summary = Dateien-Steuerung mit der Tastaturkonvention des aktuellen Desktops
files-shortcuts-primary = Primäre Modifikatortaste: Strg auf den meisten Desktops, Befehl unter macOS
files-shortcuts-navigate = Auswahl bewegen: ↑ und ↓
files-shortcuts-range = Bereich auswählen: Umschalt + ↑ und ↓
files-shortcuts-grid = Raster-Navigation: ← / → bewegen sich horizontal; ↑ / ↓ nach sichtbarer Zeile
files-shortcuts-open = Auswahl öffnen: Eingabe
files-shortcuts-rename = Umbenennen: F2
files-shortcuts-trash = In den Papierkorb verschieben: Entf
files-shortcuts-location = Ort bearbeiten: Primär + L
files-shortcuts-refresh = Aktualisieren: F5
files-shortcuts-new-folder = Neuer Ordner: Primär + Umschalt + N
files-shortcuts-edit = Kopieren, ausschneiden, einfügen, Papierkorb rückgängig, alles auswählen: Primär + C / X / V / Z / A
files-shortcuts-hidden = Versteckte Dateien zeigen oder ausblenden: Primär + H
files-shortcuts-size = Elementgröße ändern: Primär + + / −
files-shortcuts-size-reset = Größe zurücksetzen: Primär + 0
files-shortcuts-context = Kontextmenü: Menütaste oder Rechtsklick
files-shortcuts-close = Diese Hilfe öffnen: ? oder F1
files-shortcuts-dismiss = Blätter und Menüs schließen: Esc

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
files-properties-collab-summary = Dateisystem und verifizierte kollaborative Metadaten
files-availability-title = Verfügbarkeit — { $name }
files-availability-title-short = Verfügbarkeit…
files-not-stored = Nicht auf diesem Gerät gespeichert
files-partly-stored = Teilweise gespeichert ({ $bytes } Byte)
files-stored = Auf diesem Gerät gespeichert ({ $bytes } Byte)
files-kept-offline-parent = Durch einen übergeordneten Ordner offline gehalten
files-kept-offline = Offline verfügbar gehalten
files-automatic-storage = Automatische Speicherverwaltung
files-replica-unverified = Die Replikatsicherheit wurde nicht verifiziert
files-replica-counts = { $reachable } erreichbar, { $durable } dauerhafte { $durable ->
    [one] Replik
   *[other] Repliken
}
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
files-share-recipient-unreachable = Diese Person hat kein zertifiziertes Zustellgerät
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
files-sharing-title = Freigabe — { $name }
files-certified-devices = { $person }, zertifizierte Geräte: { $devices }
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
files-history-older = Ältere Versionen sind über den Verlaufsdienst verfügbar
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
files-conflict-unsupported-hint = Beide behalten und Zusammenführen sind nicht verfügbar, bis die signierten Abläufe unterstützt werden
files-history-title = Versionsverlauf — { $name }
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
files-merge-review-title = Zusammenführung prüfen — { $name }
files-merge-review-summary = Es wurden noch keine Änderungen vorgenommen
files-merge-review-checking = Die genauen unveränderlichen Versionen werden geprüft…
files-merge-review-checking-detail = Tilo vergleicht beide Änderungen mit ihrer gemeinsamen Basis. Währenddessen wird nichts geändert.
files-merge-review-safe = Sicher zusammenführbar
files-merge-review-needs-review = Prüfung nötig — es wird nichts zusammengeführt
files-merge-review-unavailable = Die Zusammenführungsprüfung ist nicht verfügbar. Aktualisieren Sie die Datei und vergleichen Sie die Versionen manuell.
files-merge-review-no-apply = Behalten Sie beide Versionen oder wählen Sie nach dem Prüfen des unveränderlichen Diffs eine Version aus.
files-merge-review-incoming = Eingehende Version von Gerät { $device }
files-merge-review-diff = In der Versionshistorie können Sie die beiden unveränderlichen Versionen vor der Zusammenführung vergleichen.
files-merge-review-guard = Tilo prüft die genaue Basis und beide Versionen unmittelbar vor dem Speichern erneut. Überlappen sich die Änderungen oder haben sie sich verändert, wird nichts zusammengeführt.
files-merge-review-undo = Die Zusammenführung erstellt einen signierten Verlaufseintrag. So können Sie eine frühere Version wiederherstellen, ohne spätere Arbeit rückgängig zu machen.
files-merge-apply-safe = Sichere Zusammenführung anwenden
files-merge-apply-safe-detail = Dies wird nur ausgeführt, wenn die deterministische Vorprüfung weiterhin beide Versionen sicher erhalten kann.
files-conflict-workflow-required = Beide behalten und Zusammenführen benötigen einen eigenen sicheren Ablauf
files-conflict-resolved-signed = Konflikt mit einem signierten Verlaufseintrag gelöst
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
files-history-timeline-detail = { $person }, { $time }
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
files-history-timeline-a11y = Versionszeitleiste
files-history-timeline-a11y-item = { $action } von { $person }, { $time }

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
files-a11y-replica-unknown = Replikatstatus unbekannt
files-a11y-durably-replicated = Dauerhaft repliziert
files-a11y-not-durably-replicated = Nicht dauerhaft repliziert
files-a11y-shared-recipients = Für { $count } { $count ->
    [one] Empfänger
   *[other] Empfänger
} freigegeben
files-a11y-version-conflict = Hat einen Versionskonflikt

## Dienst- und Empfangsfehler/status
files-open-terminal-failed = Terminal konnte nicht geöffnet werden: { $error }
files-conflict-strategy-unsupported = Diese Konfliktstrategie wird von Dateien noch nicht unterstützt
files-conflict-encode-failed = Konfliktanfrage konnte nicht kodiert werden
files-conflict-request-too-large = Konfliktanfrage überschreitet ihre Übertragungsgrenze
files-conflict-resolve-failed = Konflikt konnte nicht gelöst werden: { $error }
files-share-wrong-response = Freigabedienst hat die falsche Antwort zurückgegeben
files-share-unavailable-error = Freigabedienst nicht verfügbar: { $error }
files-share-encode-failed = Freigabeanfrage konnte nicht kodiert werden: { $error }
files-share-request-too-large = Freigabeanfrage überschreitet ihre Übertragungsgrenze
files-share-request-failed = Link konnte nicht angefordert werden: { $error }
files-share-flush-failed = Freigabeanfrage konnte nicht abgeschlossen werden: { $error }
files-share-no-response = Freigabedienst hat nicht geantwortet: { $error }
files-share-response-too-large = Freigabedienst hat eine zu große Antwort zurückgegeben
files-share-response-truncated = Antwort des Freigabedienstes wurde abgeschnitten: { $error }
files-share-response-invalid = Ungültige Antwort des Freigabedienstes: { $error }
files-share-refused = Freigabedienst hat die Anfrage abgelehnt ({ $code })
files-received-refused = Empfangsdienst hat den Link abgelehnt ({ $code })
files-received-retry-refused = Empfangsdienst konnte die Wiederholung nicht ausführen ({ $code })
files-received-wrong-response = Empfangsdienst hat die falsche Antwort zurückgegeben
files-received-encode-failed = Empfangsanfrage konnte nicht kodiert werden
files-received-request-too-large = Empfangsanfrage überschreitet ihre Grenze
files-received-unavailable = Empfangsdienst ist nicht verfügbar
files-received-no-response = Empfangsdienst hat nicht geantwortet
files-received-response-too-large = Empfangsdienst hat eine zu große Antwort zurückgegeben
files-received-response-truncated = Antwort des Empfangsdienstes wurde abgeschnitten
files-received-response-invalid = Empfangsdienst hat eine ungültige Antwort zurückgegeben
files-received-not-available = Empfangsdienst ist nicht verfügbar
files-received-fd-clone-failed = Dateideskriptor des Empfangsdienstes konnte nicht dupliziert werden
files-received-fd-invalid = Dateideskriptor des Empfangsdienstes ist ungültig
files-received-file = Freigegebene Datei empfangen
files-received-mount-starting = Freigegebene Datei angenommen; Einbindung wird gestartet
files-received-admitted = Freigegebene Datei angenommen
files-received-link-read-failed = Link zur freigegebenen Datei konnte nicht gelesen werden
files-received-link-too-large = Link zur freigegebenen Datei überschreitet seine Grenze
files-received-link-invalid-text = Link zur freigegebenen Datei ist kein gültiger Text
files-received-link-empty = Link zur freigegebenen Datei ist leer

## Anwendungsrahmen und Navigation
files-app-title = Dateien
files-selection-empty = Wählen Sie zuerst Dateien aus.
files-clipboard-source-unavailable = Eine ausgewählte Datei wurde geändert oder ist nicht mehr verfügbar. Wählen Sie sie vor dem Kopieren erneut aus.
files-clipboard-source-changed = Das angezeigte Objekt unterscheidet sich von der gespeicherten Auswahl. Wählen Sie es vor dem Kopieren erneut aus.
files-clipboard-unavailable = Die Datei-Zwischenablage des Arbeitsbereichs ist nicht verfügbar.
files-clipboard-empty = Die Datei-Zwischenablage des Arbeitsbereichs ist leer.
files-clipboard-undone = Einfügen rückgängig gemacht.
files-clipboard-partial = { $count } Elemente eingefügt. Einige Elemente konnten nicht eingefügt werden: { $error }
files-selection-unavailable = Ein Eintrag ist nicht mehr im angezeigten Ordner. Die Auswahl wurde nicht geändert.
files-selection-too-large = Wählen Sie höchstens 256 Einträge pro Anfrage aus oder verwenden Sie „Alles auswählen“.
files-back = Zurück
files-forward = Vor
files-up = Einen Ordner nach oben
files-view-list = Liste
files-view-grid = Raster
files-sort-by = Sortieren nach
files-view-options = Ansichtsoptionen
files-path-placeholder = Pfad eingeben, z. B. ~/Dokumente
files-place-home = Persönlicher Ordner
files-place-favorites = Favoriten
files-place-system = Orte
files-place-root = Wurzelverzeichnis
files-place-trash = Papierkorb
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
files-load-error-title = Dieser Ordner konnte nicht geöffnet werden
files-load-error-detail = { $path }: { $error }
files-a11y-load-error = Ordner konnte nicht geladen werden: { $error }. Erneut versuchen ist verfügbar
files-a11y-panel-details = { $title }. { $summary }. { $details }. Zum Schließen Escape drücken
files-a11y-dialog-close = { $summary }. Zum Schließen Escape drücken
files-a11y-activity = Dateiaktivität
files-a11y-current-folder = Aktueller Ordner: { $path }
files-a11y-places = Orte
files-a11y-favourite = Favorit
files-a11y-system-location = Systemort
files-a11y-place-description = { $section }, { $path }
files-a11y-breadcrumbs = Brotkrümelnavigation
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
files-properties-title = Eigenschaften — { $name }
files-properties-summary = Dateisystemmetadaten

## Ergebnisse gewöhnlicher Vorgänge
files-new-folder-default-name = Neuer Ordner
files-created-folder = Ordner erstellt
files-new-folder-failed = Neuer Ordner fehlgeschlagen: { $error }
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
files-copied = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} kopiert
files-cut = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} ausgeschnitten
files-created-file = Datei erstellt
files-new-file-failed = Neue Datei fehlgeschlagen: { $error }
files-added-favourite = Zu Favoriten hinzugefügt
files-removed-favourite = Aus Favoriten entfernt
files-moved-with-failures = { $moved } nach „{ $destination }“ verschoben ({ $failed } fehlgeschlagen)
files-moved-to-folder = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} nach „{ $destination }“ verschoben
files-nothing-to-move = Nichts zu verschieben
files-pasted = { $count } { $count ->
    [one] Element wurde
   *[other] Elemente wurden
} eingefügt
files-pasted-with-failures = { $count } eingefügt ({ $failed } fehlgeschlagen)
files-renamed = Umbenannt
files-rename-failed = Umbenennen fehlgeschlagen: { $error }

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
files-archive-select-compress = Wählen Sie Dateien oder Ordner zum Komprimieren aus
files-archive-read-only = Archive können erst nach dem Entpacken geändert werden
files-archive-open-after-extract = Entpacken Sie dieses Archiv, um enthaltene Dateien zu öffnen oder zu bearbeiten
files-archive-select = Wählen Sie zum Entpacken ein ZIP-, TAR- oder TAR.GZ-Archiv aus
files-archive-extracted =
    { $count ->
        [one] 1 Datei entpackt
       *[other] { $count } Dateien entpackt
}
files-undo-move-trash = In Papierkorb verschieben rückgängig
files-undo-trash-restored = { $count } { $count ->
    [one] 1 Element wiederhergestellt
   *[other] { $count } Elemente wiederhergestellt
}
files-undo-trash-unavailable = Rückgängig ist nicht mehr verfügbar, weil sich der Papierkorb geändert hat
files-undo-trash-failed = Papierkorbelemente konnten nicht wiederhergestellt werden: { $error }
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
files-nearby-select-one = Wählen Sie eine Datei oder einen Ordner zum Senden aus.
files-nearby-loading-devices = Zertifizierte Geräte werden gesucht…
files-nearby-no-devices = Keine zertifizierten Geräte sind online.
files-nearby-choose-device = Wählen Sie ein zertifiziertes Gerät.
files-nearby-queueing = Sicheres Senden wird vorbereitet…
files-nearby-accepted = Senden angenommen.
files-nearby-declined = Senden abgelehnt.
files-nearby-cancelled = Senden abgebrochen.
files-nearby-retrying = Senden wird erneut versucht…
files-nearby-accept = Annehmen
files-nearby-retry = Erneut versuchen
files-nearby-consent-detail = Nichts wird gespeichert, bevor Sie genau diese Übertragung annehmen.
files-nearby-retry-detail = Die Quelle bleibt verfügbar, daher kann die fortsetzbare Übertragung sicher erneut versucht werden.
files-nearby-offer-title = Senden von { $device }
files-nearby-offer-summary = { $name } empfangen?
files-nearby-offer-size = Übertragungsgröße: { $bytes } Bytes
files-nearby-failed-title = Senden fehlgeschlagen
files-nearby-failed-device = Zertifiziertes Gerät: { $device }
files-nearby-choose-destination = Speicherort wählen
files-nearby-no-destinations = Kein freigegebener beschreibbarer Arbeitsbereichsordner ist verfügbar.
files-nearby-destination-count = Wählen Sie aus { $count } freigegebenen Speicherorten.
files-nearby-save-to-destination = In { $destination } speichern
files-nearby-save-and-remember = In { $destination } speichern und für diese Sitzung merken

files-share-not-available = Freigabedienst ist auf diesem Gerät nicht verfügbar

files-name-invalid = Gib einen gültigen Datei- oder Ordnernamen ein

## Öffnen mit und Zuletzt verwendet
files-menu-open-with = Öffnen mit
files-open-with-other = Andere Anwendung…
files-open-with-none = Keine Anwendung öffnet diese Datei
files-open-with-default = { $app } (Standard)
files-open-with-default-badge = Standard
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
files-empty-recent = Dateien, die Sie öffnen oder erstellen, erscheinen hier
files-recent-reveal = Enthaltenden Ordner öffnen
files-recent-forget = Aus „Zuletzt verwendet“ entfernen
files-recent-action-unavailable = Dieser zuletzt verwendete Eintrag ist nicht mehr verfügbar. Öffnen Sie „Zuletzt verwendet“ und wählen Sie einen aktuellen Eintrag.
files-recent-not-a-folder = „Zuletzt verwendet“ ist kein Ordner; öffnen Sie einen, um Dateien hinzuzufügen

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
files-airdrop-action-detail = Den gewählten Laborempfänger auf dem Bestätigungsdialog des Systems prüfen, bevor diese Datei einmal gesendet wird.

files-nearby-status = Übertragungsstatus
files-nearby-transfer-unavailable = Diese Übertragung ist in diesem Dateienfenster nicht mehr verfügbar.
files-nearby-status-detail = Phase und bestätigte Byteanzahl dieser Übertragung anzeigen.

files-clipboard-recovered = { $count } unterbrochene Übertragungen wiederhergestellt. Wiederherstellbare Kopien sind im Papierkorb.
files-menu-recover-transfers = Unterbrochene Übertragungen wiederherstellen
files-clipboard-redone = Einfügen wiederhergestellt.

files-clipboard-cancelled = Dateiübertragung abgebrochen. Unterbrochene Übertragungen vor einem erneuten Versuch wiederherstellen.

files-invalid-destination-name = Das Ziel muss ein einzelner Dateiname sein.

files-native-restore-select-one = Wähle einen Eintrag im persönlichen Papierkorb zum Wiederherstellen. Andere Einträge bleiben unverändert.

files-history-paste-files = Dateien einfügen
files-history-restore-file = Datei wiederherstellen

files-native-trash-delete-unavailable = Diese Objekte können wiederhergestellt werden. Dauerhaftes Löschen ist hier nicht verfügbar.
files-user-trash-unavailable = Dein Papierkorb ist in diesem Arbeitsbereich nicht verfügbar.
