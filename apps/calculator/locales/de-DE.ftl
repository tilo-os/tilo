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

## What the modes, the quantities and the keypad keys are called
calculator-mode-calculator = Rechner
calculator-mode-convert = Umrechnen
calculator-mode-graph-2d = Graph 2D
calculator-mode-graph-3d = Graph 3D
calculator-quantity-length = Länge
calculator-quantity-mass = Masse
calculator-quantity-volume = Volumen
calculator-quantity-time = Zeit
calculator-quantity-speed = Geschwindigkeit
calculator-quantity-area = Fläche
calculator-quantity-data = Datenmenge
calculator-quantity-temperature = Temperatur
calculator-key-equals = Ausdruck auswerten
calculator-key-clear = Ausdruck löschen
calculator-key-backspace = Letztes Zeichen löschen
calculator-key-sign = Vorzeichen wechseln
calculator-key-percent = Prozent
calculator-key-answer = Letzte Antwort einsetzen
calculator-key-mem-add = Ausdruck zum Speicher addieren
calculator-key-mem-sub = Ausdruck vom Speicher abziehen
calculator-key-mem-recall = Speicher abrufen
calculator-key-mem-clear = Speicher löschen
calculator-key-square = Ausdruck quadrieren
calculator-key-functions-page = Funktionstasten, zweite Seite
calculator-key-numbers-page = Zahlentasten, erste Seite
calculator-key-sin = Sinus
calculator-key-cos = Kosinus
calculator-key-tan = Tangens
calculator-key-asin = Arkussinus
calculator-key-acos = Arkuskosinus
calculator-key-atan = Arkustangens
calculator-key-sqrt = Quadratwurzel
calculator-key-integral = Bestimmtes Integral
calculator-key-root = N-te Wurzel
calculator-key-abs = Betrag
calculator-key-sinh = Sinus hyperbolicus
calculator-key-cosh = Kosinus hyperbolicus
calculator-key-tanh = Tangens hyperbolicus
calculator-key-gamma = Gammafunktion
calculator-key-ln = Natürlicher Logarithmus
calculator-key-log10 = Zehnerlogarithmus
calculator-key-exp = Exponentialfunktion
calculator-key-reciprocal = Kehrwert
calculator-key-power = Potenz
calculator-key-multiply = Mal
calculator-key-divide = Geteilt
calculator-key-add = Plus
calculator-key-subtract = Minus
calculator-key-pi = Pi
calculator-key-euler = Eulersche Zahl
calculator-key-other = Rechnertaste { $key }
