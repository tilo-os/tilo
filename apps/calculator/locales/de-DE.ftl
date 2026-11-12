## Verlauf der Rechnungen und Graphen

calculator-commit-history = { $expr } = { $result } berechnet
# Was ein Abschnitt des Verlaufs hält, statt einer Zeichenzahl gesagt.
calculator-history-calculations = { $count ->
    [one] 1 Rechnung
   *[other] { $count } Rechnungen
}
calculator-history-functions = { $count ->
    [one] 1 Funktion
   *[other] { $count } Funktionen
}
calculator-history-other-changes = { $count ->
    [one] 1 weitere Änderung
   *[other] { $count } weitere Änderungen
}
calculator-delete-history-entry = { $item } aus dem Verlauf entfernt
calculator-clear-history = Den Verlauf geleert
calculator-add-function = Die Funktion y = { $formula } hinzugefügt
calculator-add-surface = Die Fläche z = { $formula } hinzugefügt
calculator-delete-function = Die Funktion y = { $item } entfernt
calculator-delete-surface = Die Fläche z = { $item } entfernt
calculator-plot-fn-expr = Die Formel { $item } geändert
calculator-plot-fn-visible = { $new ->
    [true] { $item } eingeblendet
   *[false] { $item } ausgeblendet
}
calculator-calculator-funcs = { $op ->
    [insert] Eine Funktion hinzugefügt
    [remove] Eine Funktion entfernt
   *[other] Die Funktionen umsortiert
}
calculator-calculator-surfaces = { $op ->
    [insert] Eine Fläche hinzugefügt
    [remove] Eine Fläche entfernt
   *[other] Die Flächen umsortiert
}

## Wie das Fenster und seine Bedienelemente für assistive Technik heißen

calculator-title = Rechner
calculator-a11y-expression = Ausdruck
calculator-a11y-expression-hint = Tippe einen Ausdruck ein und wähle Auswerten oder drücke die Eingabetaste.
calculator-a11y-controls = Bedienelemente des Rechners
calculator-a11y-keypad = Wissenschaftliches Tastenfeld
calculator-a11y-keypad-hint = Nutze die Tasten des Rechners oder tippe direkt einen Ausdruck ein.
calculator-a11y-workspace = Arbeitsfläche zum Rechnen und für Graphen
calculator-a11y-remove-formula = Entfernt diese gespeicherte Formel endgültig.
calculator-amount = Betrag
calculator-amount-to-convert = Umzurechnender Betrag
calculator-currency = Währung
calculator-quantity = Menge
calculator-unit-conversion = Einheiten umrechnen
calculator-unit-conversion-description = Rechne einen Betrag offline von einer Einheit in eine andere um.

## Fakten, Eigenschaftsnamen und Bereichstitel
calculator-graph-add = { $kind } hinzufügen
calculator-graph-delete = { $kind } { $number } löschen
calculator-graph-show = { $kind } { $number } anzeigen
calculator-graph-field = Formel { $kind } { $number }
calculator-next-unit = { $unit } zur nächsten Einheit wechseln

## What the window shows
calculator-result = Ergebnis { $value }
calculator-invalid-expression = Der Ausdruck ist nicht gültig: { $detail }
calculator-graph-functions = Funktionen
calculator-graph-surfaces = Flächen
