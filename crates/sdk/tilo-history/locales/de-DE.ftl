history-item-unnamed = ein Element

history-edit-set = { $field } von { $item } auf { $new } geändert
history-edit-set-unnamed = { $field } auf { $new } geändert
history-edit-on = { $field } für { $item } eingeschaltet
history-edit-on-unnamed = { $field } eingeschaltet
history-edit-off = { $field } für { $item } ausgeschaltet
history-edit-off-unnamed = { $field } ausgeschaltet
history-edit-cleared = { $field } von { $item } geleert
history-edit-cleared-unnamed = { $field } geleert
history-edit-text = { $field } von { $item } bearbeitet
history-edit-text-unnamed = { $field } bearbeitet
history-edit-add = { $field } von { $item } um { $new } geändert
history-edit-add-unnamed = { $field } um { $new } geändert
history-edit-insert = Zu { $field } von { $item } hinzugefügt
history-edit-insert-unnamed = Zu { $field } hinzugefügt
history-edit-insert-element = { $new } zu { $field } von { $item } hinzugefügt
history-edit-insert-element-unnamed = { $new } zu { $field } hinzugefügt
history-edit-remove-element = { $new } aus { $field } von { $item } entfernt
history-edit-remove-element-unnamed = { $new } aus { $field } entfernt
history-edit-remove = Aus { $field } von { $item } entfernt
history-edit-remove-unnamed = Aus { $field } entfernt
history-edit-move = { $field } von { $item } umsortiert
history-edit-move-unnamed = { $field } umsortiert
history-edit-format = { $field } von { $item } formatiert
history-edit-format-unnamed = { $field } formatiert
history-edit-settle = { $item } aus { $field } abgeschlossen
history-edit-settle-unnamed = Einen Eintrag aus { $field } abgeschlossen
history-edit-settle-failed = { $item } aus { $field } fehlgeschlagen: { $reason }
history-edit-settle-failed-unnamed = Ein Eintrag aus { $field } fehlgeschlagen: { $reason }
history-edit-forget = { $item } aus { $field } entfernt
history-edit-forget-unnamed = Einen Eintrag aus { $field } entfernt
history-edit-react = Mit { $new } auf { $item } reagiert
history-edit-react-unnamed = Mit { $new } reagiert
history-edit-unreact = Reaktion { $new } auf { $item } zurückgenommen
history-edit-unreact-unnamed = Reaktion { $new } zurückgenommen
history-edit = { $field } von { $item } geändert
history-edit-unnamed = { $field } geändert

## A verb an app has no message of its own for. The action's label is a name the
## app wrote ("Append to note"), not a sentence: quoting it keeps the row a
## past-tense sentence without making up words the app does not use.
history-verb = „{ $action }“ auf { $item } ausgeführt
history-verb-unnamed = „{ $action }“ ausgeführt
history-verb-in = „{ $action }“ in { $item } ausgeführt
history-verb-did = { $did }: „{ $item }“
history-verb-did-unnamed = { $did }
history-verb-did-in = { $did } in { $item }
history-verb-create = { $item } erstellt
history-verb-create-unnamed = Erstellt
history-verb-branch = { $item } aus einem anderen Arbeitsbereich übernommen
history-verb-branch-unnamed = Aus einem anderen Arbeitsbereich übernommen

history-group = { $count ->
    [one] Eine Änderung
   *[other] { $count } Änderungen
}
history-group-to = { $count ->
    [one] Eine Änderung an { $item }
   *[other] { $count } Änderungen an { $item }
}
history-group-same = { $what } ({ $count }×)

history-undo = „{ $what }“ rückgängig gemacht
history-undo-steps = { $count ->
    [one] Einen Schritt rückgängig gemacht
   *[other] { $count } Schritte rückgängig gemacht
}
history-redo = „{ $what }“ wiederhergestellt
history-redo-steps = { $count ->
    [one] Einen Schritt wiederhergestellt
   *[other] { $count } Schritte wiederhergestellt
}
history-undo-some = Frühere Schritte rückgängig gemacht
history-redo-some = Frühere Schritte wiederhergestellt
history-restore = Eine frühere Version wiederhergestellt
history-migrate = Auf Speicherversion { $to } aktualisiert
history-checkpoint = Zwischenstand „{ $name }“ gespeichert

history-changed = { $item } geändert
history-changed-unnamed = Dokument geändert

history-value-true = ein
history-value-false = aus
history-value-empty = nichts

history-actor-via-task = { $person } über Aufgabe „{ $task }“
history-actor-you = Du
history-actor-unknown = Jemand
