# Diff & Merge: deutsche Sprache (de-DE).
# Anzahlen sind pluralbewusst; Variablen sind Daten, nie vorformatierter Fluent.

## Statusleiste
diff-status-empty = Keine Dateien geöffnet
diff-status-identical = Die Dateien sind identisch
diff-status-counts = { $added } hinzugefügt, { $removed } entfernt, { $modified } geändert
diff-status-position = Änderung { $current } von { $total }
merge-status-changes =
    { $count ->
        [one] 1 Änderung, { $resolved } aufgelöst
       *[other] { $count } Änderungen, { $resolved } aufgelöst
    }
diff-status-summary =
    { $added } hinzugefügt, { $removed } entfernt, { $modified } geändert, in { $blocks ->
        [one] 1 Änderung
       *[other] { $blocks } Änderungen
    }
diff-status-summary-at = { $added } hinzugefügt, { $removed } entfernt, { $modified } geändert. Änderung { $current } von { $total }
merge-status-summary =
    { $count ->
        [one] 1 Änderung
       *[other] { $count } Änderungen
    }, { $resolved } aufgelöst, { $conflicts ->
        [one] 1 Konflikt übrig
       *[other] { $conflicts } Konflikte übrig
    }

## Bezeichnungen der Ressource
merge-title = Zusammenführung von { $name }
merge-title-pair = Zusammenführung von { $ours } und { $theirs }
merge-title-empty = Zusammengeführtes Ergebnis

## Zusammengeführtes Ergebnis
merged-title = Zusammengeführtes Ergebnis
merged-conflicts =
    { $count ->
        [one] 1 Konflikt
       *[other] { $count } Konflikte
    }
merged-resolved = Alles aufgelöst

## Auflösen von Änderungen
hunk-conflict = Konflikt
hunk-auto-ours = Automatisch aufgelöst: unsere
hunk-auto-theirs = Automatisch aufgelöst: ihre
hunk-took-ours = Aufgelöst: unsere
hunk-took-theirs = Aufgelöst: ihre
hunk-took-both = Aufgelöst: beide
hunk-written = Aufgelöst: eigener Text
take-ours = Unsere übernehmen
take-theirs = Ihre übernehmen
take-both = Beide übernehmen
take-ours-short = Unsere
take-theirs-short = Ihre
take-both-short = Beide

## Quellen
source-base = Basis
source-ours = Unsere
source-theirs = Ihre
source-left = Links
source-right = Rechts

## Leere Zustände und Fehler
empty-title = Öffne zwei Dateien zum Vergleichen
empty-message = Wähle zwei Textdateien mit Dateien öffnen, oder lade ein Beispiel, um zu sehen, wie ein Vergleich aussieht.
merge-empty-title = Keine Zusammenführung geöffnet
merge-empty-message = Eine Drei-Wege-Zusammenführung beginnt bei einer gemeinsamen Basis und zwei bearbeiteten Fassungen davon. Lade ein Beispiel, um eine auszuprobieren.
error-title = Die Dateien konnten nicht geöffnet werden

## Das zusammengeführte Ergebnis speichern
save-title = Das zusammengeführte Ergebnis speichern
save-accept = Speichern
save-saved = Als { $name } gespeichert
save-failed = Speichern fehlgeschlagen: { $reason }
save-cancelled = Das zusammengeführte Ergebnis wurde nicht gespeichert

## Ordner vergleichen
folder-choose-left = Den linken Ordner wählen
folder-choose-right = Den rechten Ordner wählen
folder-choose-accept = Wählen
folder-cancelled = Das Wählen der Ordner wurde abgebrochen
folder-failed = Die Ordner konnten nicht verglichen werden: { $reason }
folder-summary = { $left } und { $right }: { $added ->
    [one] 1 Datei nur links
   *[other] { $added } Dateien nur links
}, { $removed ->
    [one] 1 Datei nur rechts
   *[other] { $removed } Dateien nur rechts
}, { $modified ->
    [one] 1 geänderte Datei
   *[other] { $modified } geänderte Dateien
}
folder-truncated = Nur der erste Teil der Ordner wurde verglichen
folder-status-counts = { $added } hinzugefügt, { $removed } entfernt, { $modified } geändert
folder-meta-only-here = nur hier, { $size }
folder-meta-only-other = nur dort, { $size }
folder-meta-size = { $left } wurde zu { $right }
folder-meta-contents = anderer Inhalt
source-absent = (keine solche Datei)

## Werkzeugleiste
toolbar-diff = Diff
toolbar-merge = Drei-Wege-Zusammenführung
toolbar-merge-short = Zusammenführen
toolbar-previous = Vorherige Änderung
toolbar-next = Nächste Änderung
toolbar-open = Dateien öffnen
toolbar-sample = Beispiel laden
toolbar-sample-short = Beispiel
toolbar-save = Zusammengeführtes Ergebnis speichern
toolbar-save-short = Speichern
toolbar-saved = Erneut speichern
toolbar-saved-short = Speichern
toolbar-save-as = Speichern unter…
toolbar-folders = Ordnerliste
toolbar-compare-folders = Ordner vergleichen…
toolbar-compare-folders-short = Ordner

## Verlauf
history-title = Zusammengeführtes Ergebnis
history-originals = Die ursprünglichen Eingaben fehlen: Basis, unsere und ihre Fassung wurden nicht aufgezeichnet. Aufgezeichnete Entscheidungen sind nur Beiwerk; die Identität der Änderung lässt sich nachträglich nicht prüfen.
history-session = Bezeichnung der Sitzung: { $label }
history-choice = Aufgezeichnete Änderung { $change }: { $choice }
history-output = Zusammengeführtes Ergebnis
history-none = In dieser Fassung wurde kein zusammengeführtes Ergebnis aufgezeichnet
history-page-output = Zusammengeführtes Ergebnis
history-page-originals = Ursprüngliche Eingaben fehlen und aufgezeichnete Entscheidungen

## Ausgerichtete Quellzeilen, die nur auf einer Seite vorkommen

diff-gap-added-right = { $count ->
    [one] 1 Zeile rechts hinzugefügt
   *[other] { $count } Zeilen rechts hinzugefügt
}
diff-gap-removed-left = { $count ->
    [one] 1 Zeile links entfernt
   *[other] { $count } Zeilen links entfernt
}

## Verlauf: was jede Änderung getan hat, nach Feld und Verb
diffmerge-session-merged = Das zusammengeführte Ergebnis bearbeitet
diffmerge-open-comparison = { $left_label } mit { $right_label } verglichen
diffmerge-open-merge = Eine Zusammenführung von { $ours_label } und { $theirs_label } geöffnet
diffmerge-take-ours = Für eine Änderung die eigene Fassung genommen
diffmerge-take-theirs = Für eine Änderung die andere Fassung genommen
diffmerge-take-both = Für eine Änderung beide Seiten genommen
diffmerge-resolve = Eine Änderung von Hand aufgelöst

## Barrierefreiheit
diff-a11y-open-two-files = Diese zwei Dateien als Vergleich öffnen
diff-a11y-no-files = Keine Dateien geladen
diff-a11y-start-comparison = Mit Dateien öffnen einen lokalen Vergleich starten.
diff-a11y-status = Vergleichsstatus
diff-a11y-window = Diff und Merge
diff-a11y-window-description = Lokale Dateien vergleichen oder eine geteilte Drei-Wege-Zusammenführung auflösen
diff-a11y-switch-diff-source = Die sichtbare lokale Vergleichsquelle wechseln
diff-a11y-show-change = Diesen lokalen Änderungsblock zeigen
diff-a11y-switch-merge-source = Die sichtbare Quelle der Zusammenführung wechseln
diff-a11y-resolve-hunk = Diesen geteilten Konflikt der Zusammenführung auflösen
diff-a11y-merge-group = Drei-Wege-Zusammenführung
diff-a11y-one-source = Eine Quelle ist jeweils sichtbar. Mit der linken und rechten Pfeiltaste die Quelle wechseln.
diff-a11y-side-by-side = Basis, unsere und ihre Fassung stehen nebeneinander.

diff-a11y-folders-truncated = Nur der erste Teil der Ordner wurde verglichen
diff-a11y-choose-file = Eine Datei wählen, um sie zu vergleichen

## Fakten, Eigenschaftsnamen und Bereichstitel
diffmerge-a11y-change-lines = Änderung { $change }: Zeilen { $first } bis { $last }
diffmerge-a11y-show-file = Datei { $source } anzeigen
diffmerge-a11y-changes = { $changes ->
    [one] Eine Änderung. Scrolle durch die ausgerichteten Quellzeilen.
   *[other] { $changes } Änderungen. Scrolle durch die ausgerichteten Quellzeilen.
}
diffmerge-a11y-between = Unterschied zwischen { $left } und { $right }
diffmerge-a11y-for-change = { $choice } für Änderung { $change }
diffmerge-a11y-show-source = Quelle { $source } anzeigen

## Actions

diffmerge-effects-open-files = Dateien öffnen
diffmerge-effects-open-one = Eine Datei öffnen
diffmerge-effects-save-merged = Zusammengeführtes Ergebnis sichern
diffmerge-effects-save-merged-as = Zusammengeführtes Ergebnis sichern unter…
diffmerge-effects-compare-folders = Ordner vergleichen
diffmerge-effects-load-sample = Beispiel laden
diffmerge-effects-show-diff = Zwei-Wege-Vergleich
diffmerge-effects-show-merge = Drei-Wege-Zusammenführung
diffmerge-effects-previous-change = Vorherige Änderung
diffmerge-effects-next-change = Nächste Änderung
diffmerge-effects-show = Änderung anzeigen
diffmerge-session = Diff & Merge
diffmerge-session-open-comparison = Zwei Texte vergleichen
diffmerge-session-open-merge = Drei Texte zusammenführen
diffmerge-session-take-ours = Unsere Seite übernehmen
diffmerge-session-take-theirs = Ihre Seite übernehmen
diffmerge-session-take-both = Beide Seiten übernehmen
diffmerge-session-resolve = Mit eigenem Text auflösen
