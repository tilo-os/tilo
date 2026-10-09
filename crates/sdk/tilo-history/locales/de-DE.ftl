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
history-edit-remove = Aus { $field } von { $item } entfernt
history-edit-remove-unnamed = Aus { $field } entfernt
history-edit-move = { $field } von { $item } umsortiert
history-edit-move-unnamed = { $field } umsortiert
history-edit-format = { $field } von { $item } formatiert
history-edit-format-unnamed = { $field } formatiert
history-edit-settle = { $item } aus { $field } ausgeführt
history-edit-settle-unnamed = Einen Eintrag aus { $field } ausgeführt
history-edit-settle-failed = { $item } aus { $field } fehlgeschlagen: { $reason }
history-edit-settle-failed-unnamed = Ein Eintrag aus { $field } fehlgeschlagen: { $reason }
history-edit-forget = { $item } aus { $field } entfernt
history-edit-forget-unnamed = Einen Eintrag aus { $field } entfernt
history-edit = { $field } von { $item } geändert
history-edit-unnamed = { $field } geändert

history-verb = { $action }: { $item }
history-verb-unnamed = { $action }
history-verb-in = { $action } in { $item }

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

history-actor-for = { $actor }, für { $for }
history-actor-you = Du
history-actor-you-for = Du, für { $for }
history-actor-unknown = Jemand
history-actor-agent = { $actor } · Agent
history-actor-shortcut = { $actor } · Kurzbefehl
history-actor-app = { $actor } · App
history-actor-for-you = dich
