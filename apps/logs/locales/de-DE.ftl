## Verlauf

logs-logs-severity = { $new ->
    [debug] Einträge jeder Priorität angezeigt
    [info] Informationen und schwerwiegendere Einträge angezeigt
    [notice] Hinweise und schwerwiegendere Einträge angezeigt
    [warning] Warnungen und schwerwiegendere Einträge angezeigt
    [err] Fehler und schwerwiegendere Einträge angezeigt
    [crit] Kritische und schwerwiegendere Einträge angezeigt
    [alert] Alarmmeldungen und Notfälle angezeigt
   *[emerg] Nur Notfälle angezeigt
}
logs-logs-unit = Die Einträge der Unit { $new } angezeigt
logs-logs-boot = Die Einträge eines Systemstarts angezeigt
logs-logs-search = Im Journal gesucht
logs-clear-filters = Die Journal-Filter zurückgesetzt

## Fenster- und Barrierefreiheits-Texte
logs-severity = Schweregrad
logs-a11y-entries = Journaleinträge

## Fakten, Eigenschaftsnamen und Bereichstitel
logs-boot-id = Start-ID
logs-transport = Transport
logs-host = Rechner
logs-process-id = Prozess-ID
logs-program = Programm

## What the window shows
logs-facet-unit = Einheit
logs-facet-boot = Start
logs-copy-message = Meldung kopieren
logs-copy-entry = Eintrag kopieren

## Actions

logs-effects-refresh = Protokoll aktualisieren
logs-effects-set-live-tail = Live mitlesen
logs-effects-stop-live-tail = Nicht mehr mitlesen
logs-effects-export-filtered-jsonl = Gefilterte Protokolle exportieren
logs-effects-select-entry = Eintragsdetails anzeigen
logs-journal = Systemprotokoll
logs-journal-clear-filters = Filter zurücksetzen
