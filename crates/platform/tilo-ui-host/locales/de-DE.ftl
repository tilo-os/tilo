## Oberfläche des tilolang-Hosts. Was eine App sagt, stammt aus ihrem eigenen Quelltext.

tilolang-title = Apps
tilolang-home-heading = Apps
tilolang-home-empty = Es sind keine Apps installiert.
tilolang-open-failed = Diese App konnte nicht geöffnet werden.
tilolang-seen-failed = Was dieses Fenster gezeigt hat, konnte nicht als gesehen markiert werden.
tilolang-back = Alle Apps
tilolang-detail = Details
tilolang-ask-title = Diese Änderung erlauben?
tilolang-ask-by = Angefragt von { $invoker }
tilolang-ask-action = { $action }
tilolang-ask-confirm = Erlauben
tilolang-ask-decline = Ablehnen
tilolang-suggestion-accept = Übernehmen
tilolang-suggestion-dismiss = Verwerfen
tilolang-suggestion-says = { $name } schlägt vor:
tilolang-suggested-by = vorgeschlagen von { $name }
tilolang-ask-always = Immer erlauben
tilolang-ask-denied = Diese Änderung ist hier nicht erlaubt.
tilolang-ask-needs-approval = Diese Änderung muss zuerst bestätigt werden.
tilolang-embed-source = Block
tilolang-did-renamed = hat dies umbenannt in
tilolang-did-edited = hat { $field } bearbeitet
tilolang-did-changed = hat { $field } geändert
tilolang-did-completed = hat dies erledigt
tilolang-did-reopened = hat dies wieder geöffnet
tilolang-did-created = hat dies erstellt
tilolang-did-removed = hat dies entfernt
tilolang-did-moved = hat dies verschoben nach
tilolang-did-moved-under = hat dies untergeordnet
tilolang-did-moved-top = hat dies auf die oberste Ebene verschoben
tilolang-did-taken-out = hat dies entfernt aus
tilolang-did-assigned = hat dies zugewiesen an
tilolang-did-unassigned = hat die Zuweisung aufgehoben
tilolang-did-set = hat { $field } gesetzt auf
tilolang-did-cleared = hat { $field } geleert
tilolang-did-set-date = hat das Datum { $field } gesetzt auf
tilolang-did-cleared-date = hat das Datum { $field } geleert
tilolang-did-many = hat { $count } { $kind } geändert
tilolang-did-set-time = hat die Uhrzeit { $field } gesetzt auf
tilolang-did-cleared-time = hat die Uhrzeit { $field } geleert
tilolang-did-and = und

tilolang-invocation-pending = Wartet
tilolang-invocation-done = Erledigt
tilolang-invocation-suggested = Wartet auf Bestätigung
tilolang-invocation-declined = Abgelehnt
tilolang-invocation-failed = Fehlgeschlagen
tilolang-invocation-denied = Nicht erlaubt
tilolang-embed-error = Dieser Block konnte nicht ausgeführt werden
tilolang-all = Alle { $kind }
# Wer eine Zeile des Verlaufs geschrieben hat: die Person selbst.
tilolang-you = Du
tilolang-empty = Noch nichts hier
# What a list's entry at its end says while it is empty.
tilolang-add = Etwas hinzufügen

# The title over the items of a list that are done with, folded away after the rest.
tilolang-done = Erledigt
# The heading over the items of a list that name no group of their own.
tilolang-no-date = Kein Datum
# Said over the items of a list done with, where every one of them is: the list is not empty, it is finished.
tilolang-all-done = Alles erledigt
rich-text-reference-unreadable = Nicht mit dir geteilt
tilolang-none-selected = Nichts ausgewählt
tilolang-selected = { $count ->
    [one] 1 ausgewählt
   *[other] { $count } ausgewählt
}
tilolang-clear-selection = Auswahl aufheben
tilolang-sort = Sortieren
tilolang-undo = Rückgängig
tilolang-typing-unplaced = Keine Eingabe hat übernommen, was du getippt hast: „{ $typed }“
tilolang-did = { $action }: { $title }
tilolang-replying-to = Antwort an
tilolang-thread-replies = { $count ->
    [one] 1 Antwort
   *[other] { $count } Antworten
}
tilolang-make = Erstellen
tilolang-from = Aus
tilolang-linked = Daraus entstanden

app-activate-window = Dieses Fenster anzeigen
app-activate-window-description = Die Plattform bitten, das vorhandene Fenster anzuzeigen, ohne den Dokumentbesitzer zu wechseln.
tilolang-form-cancel = Abbrechen
tilolang-none-value = Keine Angabe
tilolang-empty-value = Leer
tilolang-yes = Ja
tilolang-no = Nein
tilolang-history = Verlauf
tilolang-history-changes = { $count ->
    [one] 1 Änderung
   *[other] { $count } Änderungen
}
tilolang-history-show = Änderungen zeigen
tilolang-history-hide = Änderungen ausblenden
tilolang-history-undo-run = Ausführung rückgängig machen
tilolang-history-made = Erstellt
tilolang-history-removed = Entfernt

# Wie ein wiederkehrendes Element wiederkehrt und wann daran erinnert wird. Die Anzahl wählt den
# Plural („alle 2 Wochen“, „3 Tage nach Erledigung“).
tilolang-repeat-every-days = { $count ->
    [one] Jeden Tag
   *[other] Alle { $count } Tage
}
tilolang-repeat-every-weeks = { $count ->
    [one] Jede Woche
   *[other] Alle { $count } Wochen
}
tilolang-repeat-every-months = { $count ->
    [one] Jeden Monat
   *[other] Alle { $count } Monate
}
tilolang-repeat-every-years = { $count ->
    [one] Jedes Jahr
   *[other] Alle { $count } Jahre
}
tilolang-repeat-weekdays = Jeden Werktag
tilolang-repeat-weekday-after-done = Der nächste Werktag nach Erledigung
tilolang-repeat-days-after-done = { $count ->
    [one] 1 Tag nach Erledigung
   *[other] { $count } Tage nach Erledigung
}
tilolang-repeat-weeks-after-done = { $count ->
    [one] 1 Woche nach Erledigung
   *[other] { $count } Wochen nach Erledigung
}
tilolang-repeat-months-after-done = { $count ->
    [one] 1 Monat nach Erledigung
   *[other] { $count } Monate nach Erledigung
}
tilolang-repeat-years-after-done = { $count ->
    [one] 1 Jahr nach Erledigung
   *[other] { $count } Jahre nach Erledigung
}
tilolang-remind-when-due = Wenn es fällig ist
tilolang-remind-minutes-before = { $count ->
    [one] 1 Minute vorher
   *[other] { $count } Minuten vorher
}
tilolang-remind-minutes-after = { $count ->
    [one] 1 Minute nachher
   *[other] { $count } Minuten nachher
}
tilolang-remind-hours-before = { $count ->
    [one] 1 Stunde vorher
   *[other] { $count } Stunden vorher
}
tilolang-remind-hours-after = { $count ->
    [one] 1 Stunde nachher
   *[other] { $count } Stunden nachher
}
tilolang-remind-days-before = { $count ->
    [one] 1 Tag vorher
   *[other] { $count } Tage vorher
}
tilolang-remind-days-after = { $count ->
    [one] 1 Tag nachher
   *[other] { $count } Tage nachher
}
tilolang-remind-weeks-before = { $count ->
    [one] 1 Woche vorher
   *[other] { $count } Wochen vorher
}
tilolang-remind-weeks-after = { $count ->
    [one] 1 Woche nachher
   *[other] { $count } Wochen nachher
}
