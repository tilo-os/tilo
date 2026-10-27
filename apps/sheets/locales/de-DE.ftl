# Verlauf: was jede Änderung an der Arbeitsmappe getan hat.
sheets-new-sheet = Tabelle hinzugefügt
sheets-duplicate-sheet = Tabelle dupliziert
sheets-delete-sheet = Tabelle gelöscht
sheets-set-cells = Zellen ab { $start } gesetzt
sheets-write-cells = Zellen geschrieben
sheets-format-cells = { $range } formatiert
sheets-sort-rows = { $range } nach Spalte { $by } sortiert
sheets-insert-rows = Zeilen über Zeile { $at } eingefügt
sheets-delete-rows = Zeilen gelöscht
sheets-insert-columns = Spalten links von Spalte { $at } eingefügt
sheets-delete-columns = Spalten gelöscht
sheets-import-sheets = Tabellen importiert
sheets-set-title = Arbeitsmappe { $title } genannt
sheets-set-records = Datensätze geschrieben
sheets-upsert-item = Zeile aktualisiert oder hinzugefügt
sheets-add-chart = Diagramm zu { $range } erstellt
sheets-edit-chart = Diagramm { $title } bearbeitet
sheets-edit-comment = Kommentar bearbeitet: { $body }
sheets-delete-chart = Diagramm gelöscht
sheets-comment = { $range } kommentiert: { $text }
sheets-reply = Geantwortet: { $text }
sheets-delete-comment = Kommentar gelöscht
sheets-cell-raw = Zelle bearbeitet
sheets-cell-format = Zelle formatiert
sheets-cell-bold = { $new ->
    [true] Zelle fett gesetzt
   *[false] Zelle normal gesetzt
}
sheets-sheet-name = { $item } in { $new } umbenannt
sheets-sheet-color = Registerfarbe von { $item } geändert
sheets-sheet-cells = Zellen von { $item } bearbeitet
sheets-workbook-sheets = Tabellen umgeordnet
sheets-workbook-title = Arbeitsmappe { $new } genannt
sheets-sheet-hidden = { $new ->
    [true] { $item } ausgeblendet
   *[false] { $item } eingeblendet
}
sheets-chart-title = Diagramm in { $new } umbenannt
sheets-chart-kind = Diagrammtyp geändert
sheets-chart-area = Diagramm zeigt nun andere Zellen
sheets-comment-body = Kommentar bearbeitet
sheets-comment-resolved = { $new ->
    [true] Kommentar erledigt
   *[false] Kommentar wieder geöffnet
}
sheets-reply-body = Antwort bearbeitet
sheets-doc-settings-currency = Währung der Arbeitsmappe festgelegt
sheets-doc-settings-date-format = Datumsformat der Arbeitsmappe festgelegt
sheets-doc-settings-time-format = Zeitformat der Arbeitsmappe festgelegt

# Notion-Export importieren.
sheets-notion-import-title = Notion-Export importieren
sheets-notion-files = Notion-Export
sheets-notion-json-files = Notion-Datenbankliste
sheets-notion-imported =
    { $tables ->
        [one] Eine Tabelle importiert
       *[other] { $tables } Tabellen importiert
    }
sheets-notion-imported-table = Importierte Tabelle
sheets-notion-no-tables = In diesem Export ist keine Tabelle zum Importieren
sheets-notion-too-big = Der Export ist zu groß zum Importieren
sheets-notion-too-many-tables = Der Export enthält mehr als { $limit } Tabellen
sheets-notion-archive-invalid = Das Export-Archiv lässt sich nicht lesen
sheets-notion-unreadable = Diese Datei ist kein Notion-Export
sheets-notion-yes = Ja
sheets-notion-no = Nein
