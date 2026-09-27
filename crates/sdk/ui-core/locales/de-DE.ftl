counter = Wert: { $count }
btn_1 = Klick Mich
tab-modified = Nicht gespeicherte Änderungen

standalone-history-restore = Diese Version wiederherstellen…
standalone-history-restore-title = Diese Version wiederherstellen?
standalone-history-restore-body = Das Dokument kehrt zum Stand nach „{ $version }“ zurück. Dabei entsteht eine neue Änderung: Spätere Versionen bleiben im Verlauf, und Rückgängig macht die Wiederherstellung rückgängig.
standalone-history-restore-confirm = Version wiederherstellen
standalone-history-restore-cancel = Abbrechen
standalone-history-restore-action = Version wiederherstellen
standalone-history-restore-done = Version wiederhergestellt. Rückgängig macht es rückgängig.
standalone-history-restore-stale = Das Dokument hat sich während der Prüfung geändert. Wähle die Version erneut aus, um sie wiederherzustellen.
standalone-history-restore-pending = Schließe die aktuelle Bearbeitung ab und stelle die Version dann wieder her.
standalone-history-restore-denied = Nur Bearbeitende können eine Version wiederherstellen.
standalone-history-restore-unavailable = Diese Version kann hier nicht wiederhergestellt werden.
standalone-history-preview-resource = Dokument
history-edited-field = { $more ->
    [0] { $path } bearbeitet
   *[other] { $path } und { $more } weitere bearbeitet
}
history-removed-field = { $more ->
    [0] { $path } entfernt
   *[other] { $path } und { $more } weitere entfernt
}
history-review-title = { $resource } wiederherstellen
history-review-confirm = Version wiederherstellen
history-review-refresh = Prüfung aktualisieren
history-review-cancel = Abbrechen
history-review-body = Version: { $version }

    Die Wiederherstellung bringt das Dokument als eine neue Änderung genau auf diese Version zurück. Spätere Änderungen bleiben im Verlauf, und Rückgängig macht die Wiederherstellung rückgängig.
history-review-selected-version = Ausgewählte Version
history-review-not-content = Diese Aktivität enthält keinen wiederherstellbaren Dokumentinhalt.
history-review-no-clock = Die aktuelle Uhrzeit ist nicht verfügbar. Die Wiederherstellung kann den Ablauf von Berechtigungen nicht prüfen.
history-restore-unsupported = Diese Version kann mit dem Inhaltsformat dieser App nicht sicher wiederhergestellt werden.
history-preview-description = { $resource } in dieser Version

close-title = Dokument schließen
close-unsaved = Änderungen vor dem Schließen speichern?
close-save-failed = Änderungen konnten nicht gespeichert werden
close-saving = Warten, bis das Speichern abgeschlossen ist…
close-save-cancelled = Speichern wurde abgebrochen. Das Dokument ist weiterhin geöffnet.
close-save = Speichern
close-discard = Änderungen verwerfen
close-retry = Erneut versuchen
close-keep-editing = Weiter bearbeiten
close-broker-disconnected = Die Verbindung zum Speicherdienst wurde getrennt. Das Dokument ist weiterhin geöffnet.
terminal-find-scrollback = Im Terminalverlauf suchen

notes-image-edit = Bild bearbeiten
notes-image-done = Fertig
notes-image-source = Bildbeschreibung und Ziel
table-edit-source = Quelltext
table-edit-visual = Tabelle
table-add-row = Zeile hinzufügen
table-add-column = Spalte hinzufügen

block-editor-drag-hint = Klicken für Blocktyp · Ziehen zum Verschieben · Alt+↑/↓
block-editor-find-placeholder = In Notiz suchen
block-editor-find-no-results = Keine Ergebnisse
block-editor-find-previous = Vorheriger Treffer
block-editor-find-next = Nächster Treffer
block-editor-find-close = Suche schließen
block-editor-change-type = Blocktyp ändern
block-editor-copy-code-hint = Code kopieren

block-editor-code-exit-hint = Ctrl+Enter: Code verlassen
block-editor-code-exit-compact = Ctrl+Enter ↵
block-editor-code-copied = Kopiert
block-editor-code-keyboard-hint = Ctrl+Enter: darunter schreiben · Ctrl+Shift+Enter: darüber schreiben · Tab / Shift+Tab: einrücken / ausrücken · Ctrl+Shift+C: Code kopieren
widget-block-indent = Block mit Unterelementen einrücken
widget-block-text = Blocktext
widget-block-outdent = Block mit Unterelementen ausrücken
tree-rename-field = Umbenennen
tree-move-up = Nach oben verschieben
tree-move-down = Nach unten verschieben
tree-move-indent = In das Element darüber verschieben
tree-move-outdent = Aus dem übergeordneten Element verschieben

# Authentication surfaces share the system theme and catalog.
greeter-welcome-back = Willkommen zurück bei Tilo
greeter-sign-in = Zum Fortfahren anmelden
greeter-passcode-intro = Gib deinen Code ein, um deinen Desktop zu öffnen.
lock-welcome-back = Willkommen zurück

# Now playing: Quick Settings and the lock screen.
now-playing-title = Wird abgespielt
now-playing-play = Abspielen
now-playing-pause = Pausieren
now-playing-previous = Vorheriger Titel
now-playing-next = Nächster Titel
now-playing-seek = Wiedergabeposition
now-playing-seek-help = Mit Pfeil links und rechts um fünf Sekunden springen
now-playing-choose-player = Player wählen
now-playing-choose-output = Wiedergabeort wählen
now-playing-output-local = Auf diesem Computer
now-playing-output-device = Eines deiner Geräte
now-playing-output-speaker-group = Eine deiner Lautsprechergruppen
now-playing-route-gone = Dieser Wiedergabeort ist nicht mehr verfügbar

# Die eingebettete Berechtigungsabfrage einer isolierten App.
permission-prompt-allow = Erlauben
permission-prompt-session = Diese Sitzung
permission-prompt-always = Immer erlauben
permission-prompt-deny = Ablehnen
permission-prompt-requested = { $capability } – angefragt von { $app }

# Relative Zeitangaben in Verlaufs- und Aktivitätszeilen.
relative-time-just-now = gerade eben
relative-time-minutes = { $count ->
        [one] vor { $count } Min.
       *[other] vor { $count } Min.
    }
relative-time-hours = { $count ->
        [one] vor { $count } Std.
       *[other] vor { $count } Std.
    }
relative-time-days = { $count ->
        [one] vor { $count } Tag
       *[other] vor { $count } Tagen
    }

# Grobe relative Zeitangaben („vor 3 Tagen“) in Listen und Karten.
relative-ago-just-now = gerade eben
relative-ago-minutes = vor { $count } Min.
relative-ago-hours = vor { $count } Std.
relative-ago-yesterday = gestern
relative-ago-days = { $count ->
        [one] vor { $count } Tag
       *[other] vor { $count } Tagen
    }
relative-ago-weeks = { $count ->
        [one] vor { $count } Woche
       *[other] vor { $count } Wochen
    }
relative-ago-months = { $count ->
        [one] vor { $count } Monat
       *[other] vor { $count } Monaten
    }
relative-ago-years = { $count ->
        [one] vor { $count } Jahr
       *[other] vor { $count } Jahren
    }

# Ein Software-Teilnehmer in Verlauf, Präsenz und Aktivität.
participant-agent = { $name } · Agent
participant-unknown-agent = Unbekannter Agent
