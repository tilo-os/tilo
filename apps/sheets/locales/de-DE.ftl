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
sheets-edit-comment = Kommentar bearbeiten
sheets-delete-chart = Diagramm löschen
sheets-comment = { $range } kommentiert: { $text }
sheets-reply = Geantwortet: { $text }
sheets-delete-comment = Kommentar löschen
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
sheets-start-formula = Formel beginnen
sheets-start-formula-detail = Eine Formel in der aktiven Zelle beginnen
sheets-name-box = Namensfeld
sheets-name-box-detail = Zu einer Zelle oder einem Bereich springen; Tastenkürzel Strg+G
sheets-formula-bar = Formelleiste
sheets-formula-bar-detail = Wert oder Formel der aktiven Zelle bearbeiten
sheets-add-sheet = Blatt hinzufügen
sheets-column-headers = Spaltenköpfe
sheets-comments-charts = Kommentare und Diagramme
sheets-manage-chart = Diagramm verwalten
sheets-open-chart-range = Diagrammbereich öffnen
sheets-rename-chart = Diagramm umbenennen
sheets-column-chart = Säulendiagramm
sheets-line-chart = Liniendiagramm
sheets-chart-cells = Diagrammzellen
sheets-reply-comment = Auf Kommentar antworten
sheets-open-comment-anchor = Kommentarstelle öffnen
sheets-resolve-comment = Kommentar als erledigt markieren
sheets-reopen-comment = Kommentar wieder öffnen
sheets-actions = Tabellenaktionen
sheets-window-name = Sheets
sheets-window-detail = Bearbeitbare Arbeitsmappe mit Formelleiste und Blattregistern

## Fenster- und Barrierefreiheits-Texte
sheets-numbers-none = Keine Zahlen in diesem Bereich

## Fakten, Eigenschaftsnamen und Bereichstitel
sheets-a11y-comment-by = Kommentar von { $author }
sheets-a11y-chart-extent = { $start }:{ $end }; im Diagramm-Kärtchen öffnen, umbenennen, Typ oder Zellen ändern oder löschen
sheets-a11y-worksheet = Arbeitsblatt; ausgewählter Bereich { $range }; { $columns } Spalten mal { $rows } Zeilen; nur das sichtbare Fenster des Rasters wird angesagt
sheets-a11y-row = Zeile { $row }
sheets-a11y-cell = Spalte { $column }, Zeile { $row }
sheets-a11y-column = Spalte { $letter }

## What the window shows
sheets-no-replies = Keine Antworten

sheets-comment-you = Du
sheets-comment-someone = Jemand

## Das Blatt mit den Dokumenteinstellungen
sheets-document-settings = Dokumenteinstellungen
sheets-done = Fertig

## Die Plätze der Fensterleiste
sheets-file-menu = Datei
sheets-format-menu = Format
sheets-bold = Fett
sheets-autosum = AutoSumme
sheets-show-formulas = Formeln anzeigen
sheets-show-results = Berechnete Ergebnisse anzeigen

## Die Farbmarke eines Registers
sheets-tab-colour = Registerfarbe

## Actions

sheets-effect-import-csv = CSV importieren
sheets-effect-import-xlsx = Excel-Arbeitsmappe importieren
sheets-effect-import-ods = OpenDocument-Tabelle importieren
sheets-effect-export-csv = CSV exportieren
sheets-effect-export-xlsx = Excel-Arbeitsmappe exportieren
sheets-effect-export-ods = OpenDocument-Tabelle exportieren
sheets-effect-import-xlsx-content = Arbeitsmappeninhalt importieren
sheets-effect-export-xlsx-content = Arbeitsmappeninhalt exportieren
sheets-effect-commands = Befehlspalette öffnen
sheets-effect-go-to-cell = Zu Zelle springen …
sheets-effect-toggle-formula-view = Formelansicht umschalten
sheets-effect-read-cells = Zellen lesen
sheets-effect-open = Öffnen
sheets-workbook = Arbeitsmappe
sheets-workbook-set-title = Arbeitsmappe benennen
sheets-workbook-set-records = Datensätze setzen
sheets-workbook-upsert-item = Zeile einfügen oder aktualisieren
sheets-workbook-duplicate-sheet = Blatt duplizieren
sheets-workbook-delete-sheet = Blatt löschen
sheets-workbook-set-cells = Zellen setzen
sheets-workbook-write-cells = Zellen schreiben
sheets-workbook-format-cells = Zellen formatieren
sheets-workbook-sort-rows = Zeilen sortieren
sheets-workbook-insert-rows = Zeilen einfügen
sheets-workbook-delete-rows = Zeilen löschen
sheets-workbook-insert-columns = Spalten einfügen
sheets-workbook-delete-columns = Spalten löschen
sheets-workbook-import-sheets = Blätter importieren
sheets-workbook-add-chart = Diagramm hinzufügen
sheets-workbook-edit-chart = Diagramm bearbeiten
sheets-workbook-rename-sheet = Blatt umbenennen
sheets-workbook-comment = Zellen kommentieren

## What the app is called in a window and in the launcher
sheets-app-title = Tabellen
