# Aktivität auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-activity = Aktivität

d-so-tilo-activity-keywords = verlauf, aktivität, änderungen, rückgängig, wiedergabe, arbeitssitzungen, wer geändert hat, history, changes

d-dgzobvxw = Fenster
    .doc = Die Aktivität des Arbeitsbereichs, wie dieses Fenster sie zeigt.
    .noun = { $count ->
        [one] Fenster
       *[other] Fenster
    }

d-ukfxyqw3 = Ort
    .doc = Die Ansicht, die das Fenster zuletzt zeigte: was neu ist, die Änderungen oder die Wiedergabe.

d-k5cpej2o = Aktivität
    .w-activity = Aktivität
    .w-what-s-new = Was neu ist
    .w-history = Verlauf
    .w-replay = Wiedergabe
    .w-loading-changes = Änderungen werden geladen
    .w-while-you-were-away = Während du weg warst
    .w-work-sessions = Arbeitssitzungen
    .w-no-changes-in-this-work-session = Keine Änderungen in dieser Arbeitssitzung
    .w-mark-as-viewed = Als gesehen markieren
    .w-on-device-summary = Zusammenfassung auf diesem Gerät
    .w-where-you-left-off = Wo du aufgehört hast
    .w-resume = Fortsetzen
    .w-you-re-caught-up = Du bist auf dem neuesten Stand
    .w-see-history = Verlauf ansehen
    .w-open-in-history = Im Verlauf öffnen
    .w-morning-sources = Morgenquellen
    .w-choose-a-resource-to-hear-about = Wähle in den Details einer Änderung eine Ressource, über die du jeden Morgen hören möchtest.
    .w-refresh-source = Quelle aktualisieren
    .w-remove-morning-source = Morgenquelle entfernen
    .w-this-workspace = Dieser Arbeitsbereich
    .w-all-workspaces = Alle Arbeitsbereiche
    .w-time = Zeit
    .w-the-changes-could-not-be-read = Die Änderungen konnten nicht gelesen werden
    .w-people = Personen
    .w-everyone = Alle
    .w-browse-by = Durchsuchen nach
    .w-everything = Alles
    .w-clear-filters = Filter zurücksetzen
    .w-this-device-cannot-change-the-history = Dieses Gerät kann den Verlauf hier nicht ändern.
    .w-not-here-could-not-be-read = Nicht hier: { $unread } konnten nicht gelesen werden
    .w-the-oldest-changes-are-left-out = Die ältesten Änderungen fehlen: { $capped } Datensätze werden nicht gelesen
    .w-no-changes-here = Keine Änderungen hier
    .w-no-changes-match-these-filters = Keine Änderungen passen zu diesen Filtern
    .w-selected = Ausgewählt
    .w-not-on = Nicht auf { $label }: { $reason }
    .w-select = Auswählen
    .w-select-through-here = Bis hierher auswählen
    .w-show-earlier-changes = Frühere Änderungen anzeigen
    .w-show-more = Mehr anzeigen
    .w-of-changes = { $kept } von { $held } Änderungen
    .w-details = Details
    .w-these-changes-are-no-longer-in = Diese Änderungen sind nicht mehr im Verlauf.
    .w-by = Von { $join }
    .w-work-session = Arbeitssitzung: { $session }
    .w-changed = Geändert
    .w-fields = Felder
    .w-what-changed = Was sich geändert hat
    .w-preview-unavailable = Vorschau nicht verfügbar
    .w-the-workspace-as-it-was-before = Der Arbeitsbereich vor und nach dieser Änderung konnte nicht gelesen werden.
    .w-no-content-differences-in-this-selection = Keine inhaltlichen Unterschiede in dieser Auswahl
    .w-name = Name
    .w-name-this-work-session = Diese Arbeitssitzung benennen
    .w-save-name = Namen speichern
    .w-agent-task = Agent-Aufgabe
    .w-why-this-work = Warum diese Arbeit
    .w-decision = Entscheidung
    .w-no-reason-was-recorded = Es wurde kein Grund aufgezeichnet.
    .w-changes-still-in-effect = Änderungen, die noch gelten
    .w-this-session-leaves-nothing-in-place = Diese Sitzung lässt nichts zurück: jede ihrer Änderungen wurde rückgängig gemacht.
    .w-select-a-work-session-or-a = Wähle eine Arbeitssitzung oder einen Bereich von Änderungen

d-qlpt7vlt = Ausgeführt von
    .w-started-by-you = Von dir gestartet
    .w-started-by = Gestartet von { $name }

d-peafm5lw = Wiedergabe
    .w-replay-is-waiting-for-current-edits = Die Wiedergabe wartet auf laufende Änderungen
    .w-no-replayable-workspace-checkpoints-yet = Noch keine wiedergabefähigen Prüfpunkte des Arbeitsbereichs
    .w-checkpoint-of = Prüfpunkt { $value } von { $total }
    .w-back = Zurück
    .w-pause = Pause
    .w-play = Abspielen
    .w-forward = Vor
    .w-workspace-replay-is-unavailable-for-these = Die Wiedergabe des Arbeitsbereichs ist für diese Prüfpunkte nicht verfügbar
    .w-the-workspace-did-not-exist-at = Der Arbeitsbereich existierte an diesem Prüfpunkt nicht
    .w-present = Anwesend: { $name }

d-tegrh6cu = Abschnitt
    .doc = Eine Folge von Änderungen unter einer Überschrift: die Arbeitssitzung, zu der sie gehören, oder der Tag, an dem sie gemacht wurden.
    .noun = { $count ->
        [one] Abschnitt
       *[other] Abschnitte
    }

d-ndg6kobd = Schlüssel

d-rb4x4ubn = Bezeichnung

d-e5ekpgc7 = Arbeitssitzung
    .doc = Ob sie von einer Arbeitssitzung stammen, deren Änderungen zusammen ausgewählt werden.

d-2evxsxd2 = Von

d-67qougjz = Bis

d-qkho4jyl = Einträge

d-tegrh6cu-filter-work_session = Arbeitssitzung

d-24ilmj72 = Vor-Zeile
    .w-nothing-now = Jetzt nichts
    .w-was = War { $before }

d-j7eah2gc = Vor
    .w-just-now = Gerade eben
    .w-1-minute-ago = Vor 1 Minute
    .w-minutes-ago = Vor { $minutes } Minuten
    .w-1-hour-ago = Vor 1 Stunde
    .w-hours-ago = Vor { $hours } Stunden
    .w-yesterday = Gestern
    .w-1-day-ago = Vor 1 Tag
    .w-days-ago = Vor { $days } Tagen

d-o6yuvcjn = Was dieses Gerät getan hat
    .w-what-this-device-did = Was dieses Gerät getan hat

d-iwboeiq3 = Fensterkarte
    .w-empty-window = Leeres Fenster
    .w-one-tab-of-the-width = Ein Tab, { $share } % der Breite
    .w-of-the-width = { $join }, { $share } % der Breite
