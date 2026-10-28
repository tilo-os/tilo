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
