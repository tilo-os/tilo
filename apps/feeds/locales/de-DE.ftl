feeds-open-link-invalid = Dieser Artikellink kann nicht geöffnet werden. Nur Web-, Gemini- und E-Mail-Links werden unterstützt.
feeds-open-link-started = Link wird in der Standard-App geöffnet…
feeds-open-link-failed = Der Link konnte nicht geöffnet werden

## Verlauf: was jede Änderung an der Bibliothek getan hat, nach Feld und Verb

feeds-article-read = { $new ->
    [true] { $item } als gelesen markiert
   *[false] { $item } als ungelesen markiert
}
feeds-article-starred = { $new ->
    [true] { $item } mit Stern markiert
   *[false] Stern von { $item } entfernt
}
feeds-article-tags = Schlagwörter von { $item } geändert
feeds-feed-title = Abonnement { $item } umbenannt
feeds-feed-folder = { $item } in einen anderen Ordner verschoben
feeds-folder-name = Ordner { $item } umbenannt
feeds-folder-collapsed = { $new ->
    [true] Ordner { $item } eingeklappt
   *[false] Ordner { $item } ausgeklappt
}
feeds-library-folders = Ordner geändert
feeds-library-feeds = Abonnements geändert
feeds-library-articles = Artikel geändert
feeds-add-subscription = { $url } abonniert
feeds-unsubscribe = Abonnement { $item } beendet
feeds-mark-feed-read = Alles in { $item } als gelesen markiert
feeds-mark-everything-read = Alle Artikel als gelesen markiert
feeds-new-folder = Ordner { $name } angelegt
feeds-delete-folder = Ordner { $item } gelöscht
feeds-import-subscriptions = Abonnements aus einer OPML-Datei übernommen
feeds-refreshed = Neue Artikel abgerufen

## Ein Abonnement, dessen Aktualisierung fehlgeschlagen ist

feeds-refresh-unreachable = Die Website war nicht erreichbar
feeds-refresh-refused = Die Website hat die Anfrage abgelehnt
feeds-refresh-refused-status = Die Website hat die Anfrage abgelehnt (HTTP { $status })
feeds-refresh-not-a-feed = Die Antwort war kein Feed
feeds-refresh-untrusted = Das Zertifikat der Website hat sich geändert, jemand gibt sich womöglich als sie aus
feeds-refresh-unavailable = Abruf konnte nicht gestartet werden
feeds-window-name = Feeds
feeds-reading-settings = Leseeinstellungen
feeds-refreshing = Feeds werden aktualisiert
feeds-no-article = Kein Artikel ausgewählt
feeds-articles-with-tag = Artikel mit diesem Schlagwort zeigen
feeds-feed-actions = Aktionen für diesen Feed

## Fenster- und Barrierefreiheits-Texte
feeds-a11y-text-size = Textgröße
feeds-a11y-subscriptions = Abos

## Fakten, Eigenschaftsnamen und Bereichstitel
feeds-a11y-star = { $article } markieren
feeds-a11y-tag = Schlagwort { $tag }

## What the window shows
feeds-add-caption = Füge die Adresse eines RSS-, Atom- oder Gemini-Feeds ein, oder die einer Website, die einen anbietet.
feeds-add-label = Feed-Adresse
feeds-folder-caption = Ordner fassen verwandte Abos zusammen.
feeds-folder-label = Ordnername
feeds-folder-placeholder = Newsletter
feeds-rename-caption = Wähle einen eigenen Namen für dieses Abo.
feeds-rename-label = Name des Abos
feeds-tags-caption = Trenne Schlagwörter durch Kommas.
feeds-tags-label = Schlagwörter

## What the overview, a refresh and the reading settings say
feeds-status-refreshing = Aktualisiere { $done } von { $total } Feeds
feeds-status-new = { $count ->
    [one] 1 neuer Artikel
   *[other] { $count } neue Artikel
}
feeds-status-failed = { $count ->
    [one] 1 Feed konnte nicht aktualisiert werden
   *[other] { $count } Feeds konnten nicht aktualisiert werden
}
feeds-status-up-to-date = Alle Feeds sind aktuell
feeds-filter-all = Alle Artikel
feeds-filter-unread = Ungelesen
feeds-filter-starred = Markiert
feeds-filter-folder = Ordner
feeds-filter-feed = Feed
feeds-filter-tag = Schlagwort
feeds-theme-system = System
feeds-theme-light = Hell
feeds-theme-sepia = Sepia
feeds-theme-dark = Dunkel
feeds-font-sans = Ohne Serifen
feeds-font-serif = Mit Serifen
feeds-font-mono = Dicktengleich

## Actions

feeds-effects-refresh-all = Alle Abonnements aktualisieren
feeds-effects-import-opml = OPML importieren…
feeds-effects-new-folder-form = Neuer Ordner…
feeds-effects-export-opml = OPML exportieren…
feeds-effects-reveal-tag = Markierte Artikel anzeigen
feeds-effects-reader-text-larger = Lesetext vergrößern
feeds-effects-reader-text-smaller = Lesetext verkleinern
feeds-effects-reader-font = Leseschrift verwenden
feeds-effects-reader-paper = Lesepapier verwenden
feeds-library = Feeds
feeds-library-add-subscription = Abonnement hinzufügen
feeds-library-unsubscribe = Abonnement beenden
feeds-library-mark-feed-read = Abonnement als gelesen markieren
feeds-library-mark-everything-read = Alle Artikel als gelesen markieren
feeds-library-new-folder = Neuer Ordner
feeds-library-delete-folder = Ordner löschen
feeds-library-import-subscriptions = Abonnements importieren
feeds-library-refreshed = Abgerufene Artikel erfassen
