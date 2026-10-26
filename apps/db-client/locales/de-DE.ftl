## Fensterleiste

db-run-query = Abfrage ausführen
db-cancel-query = Abfrage abbrechen
db-copy-row = Zeile kopieren
db-copy-result = Ergebnis kopieren
db-view-switcher = Ansicht

## Tabs

db-tab-query = Abfrage
db-tab-data = Daten
db-tab-structure = Aufbau

## Abfragebereich

db-ready = Bereit
db-query-no-columns = Die Abfrage lieferte keine Spalten.
db-statement-ran = Die Anweisung wurde ausgeführt.
db-starter-queries = Beispielabfragen

## Ergebnis

db-result-rows = { $count ->
    [one] 1 Zeile
   *[other] { $count } Zeilen
} in { $ms } ms
db-result-rows-cut = Die ersten { $count } Zeilen in { $ms } ms
db-result-cut-rows = Das Ergebnis wurde bei { $limit } Zeilen abgeschnitten. Grenze die Abfrage ein oder blättere die Tabelle durch.
db-result-cut-payload = Das Ergebnis wurde bei { $limit } abgeschnitten, damit das Fenster flüssig bleibt. Grenze die Abfrage ein, um den Rest zu sehen.
db-result-cut-payload-limit = 8 MB
db-result-affected = { $count ->
    [one] 1 Zeile geändert
   *[other] { $count } Zeilen geändert
} in { $ms } ms
db-copied-row = Zeile kopiert
db-copied-result = Ergebnis kopiert
db-copy-nothing = Es ist nichts ausgewählt, das kopiert werden könnte

## Status

db-connected = Verbunden mit { $name }
db-connecting = Verbindung zu { $name } wird aufgebaut …
db-running = Läuft …
db-cancelling = Abfrage wird abgebrochen …
db-cancel-finished = Die Abfrage ist bereits beendet
db-copy-failed = Konnte nicht in die Zwischenablage kopieren
db-cancelled = Abfrage abgebrochen
db-no-connection = Keine offene Verbindung
db-query-running = Es läuft bereits eine Abfrage
db-identity-exhausted = Der Abfragezähler ist erschöpft; Database Client neu starten

## Seitenleiste

db-catalog = Katalog
db-tables = Tabellen
db-saved-queries = Gespeicherte Abfragen
db-connections = Verbindungen


## Fenstertitel und Tabs

db-tab-query-hint = SQL schreiben und gegen die offene Verbindung ausführen.
db-tab-data-hint = Die Zeilen der ausgewählten Tabelle.
db-tab-structure-hint = Spalten, Schlüssel und Indizes der ausgewählten Tabelle.

## Abfragebereich

db-sql-placeholder = SQL-Abfrage schreiben
db-sql-editor = SQL

## Status

db-dismiss-status = Schließen

## Seitenleiste

db-navigator = Verbindungen und Tabellen
db-new-connection = Neue Verbindung
db-remove-connection = Verbindung entfernen
db-tables = Keine Tabellen
db-saved-queries = Gespeicherte Abfragen

## Daten

db-no-table = Keine Tabelle ausgewählt
db-choose-table-rows = Wähle eine Tabelle, um ihre Zeilen zu sehen.
db-choose-table-structure = Wähle eine Tabelle, um ihren Aufbau zu sehen.
db-no-connections = Noch keine Verbindungen
db-no-connections-hint = Füge eine PostgreSQL-, MySQL- oder SQLite-Datenbank hinzu, um ihre Tabellen zu durchsuchen und Abfragen auszuführen.
db-no-rows = Keine Zeilen
db-rows-range = Zeilen { $from } bis { $to } von { $total }
db-next-rows = Nächste Zeilen
db-previous-rows = Vorherige Zeilen

## Aufbau

db-column-name = Name
db-column-type = Typ
db-column-nullable = Nullbar
db-column-key = Schlüssel
db-column-default = Standard
db-column-constraint = Bedingung
db-column-column = Spalte
db-column-references = Verweist auf
db-column-index = Index
db-column-unique = Eindeutig
db-column-columns = Spalten
db-keys = Schlüssel
db-indexes = Indizes
db-yes = Ja
db-no = Nein
db-none = Keine
db-primary = Primär
db-primary-key = Primärschlüssel
db-foreign-key = Fremdschlüssel

## Verbindungsdialog

db-new-connection-title = Neue Verbindung
db-connection-name = Name der Verbindung
db-host = Host
db-port = Port
db-database = Datenbank
db-user = Benutzer
db-password = Passwort
db-password-stays = Das Passwort bleibt auf diesem Gerät.
db-file-path = Pfad der SQLite-Datei
db-add-connection = Verbindung hinzufügen
db-cancel = Abbrechen
db-untitled = Ohne Namen
db-no-connection-open = Es ist keine Verbindung offen.
db-cannot-undo = Database Client kann das nicht rückgängig machen.
db-run-statement-title = Diese Anweisung ausführen?
db-run-statement = Anweisung ausführen

## Verlauf: was jede Änderung tat

db-client-workspace-sql = Die SQL bearbeitet
db-client-workspace-connections = Die gespeicherten Verbindungen geändert
db-client-workspace-saved-queries = Die gespeicherten Abfragen geändert
db-client-saved-connection-name = Die Verbindung { $item } umbenannt
db-client-saved-connection-engine = Die Engine von { $item } auf { $new ->
    [sqlite] SQLite
    [postgres] PostgreSQL
    [mysql] MySQL
   *[other] { $new }
} gesetzt
db-client-saved-connection-host = Den Host von { $item } auf { $new } gesetzt
db-client-saved-connection-port = Den Port von { $item } auf { $new } gesetzt
db-client-saved-connection-database = Die Datenbank von { $item } auf { $new } gesetzt
db-client-saved-connection-user = Den Benutzer von { $item } auf { $new } gesetzt
db-client-saved-connection-file-path = Die Datei von { $item } auf { $new } gesetzt
db-client-saved-query-name = Die Abfrage { $item } umbenannt
db-client-saved-query-sql = Die Abfrage { $item } bearbeitet
db-client-add-connection = Die Verbindung { $name } hinzugefügt
db-client-remove-connection = Die Verbindung { $item } entfernt
db-client-save-query = Die Abfrage { $name } gespeichert
db-client-remove-query = Die Abfrage { $item } entfernt
