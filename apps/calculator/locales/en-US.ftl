## History of the calculations and graphs

calculator-commit-history = Calculated { $expr } = { $result }
# What a band of the History holds, said in place of a character count.
calculator-history-calculations = { $count ->
    [one] 1 calculation
   *[other] { $count } calculations
}
calculator-history-functions = { $count ->
    [one] 1 function
   *[other] { $count } functions
}
calculator-history-other-changes = { $count ->
    [one] 1 other change
   *[other] { $count } other changes
}
calculator-delete-history-entry = Removed { $item } from the history
calculator-clear-history = Cleared the history
calculator-add-function = Added the function y = { $formula }
calculator-add-surface = Added the surface z = { $formula }
calculator-delete-function = Removed the function y = { $item }
calculator-delete-surface = Removed the surface z = { $item }
calculator-plot-fn-expr = Changed the formula { $item }
calculator-plot-fn-visible = { $new ->
    [true] Showed { $item }
   *[false] Hid { $item }
}
calculator-calculator-funcs = { $op ->
    [insert] Added a function
    [remove] Removed a function
   *[other] Rearranged the functions
}
calculator-calculator-surfaces = { $op ->
    [insert] Added a surface
    [remove] Removed a surface
   *[other] Rearranged the surfaces
}

## What the window and its controls are called for assistive technology

calculator-title = Calculator
calculator-a11y-expression = Expression
calculator-a11y-expression-hint = Type an expression, then activate Evaluate or press Enter.
calculator-a11y-controls = Calculator controls
calculator-a11y-keypad = Scientific keypad
calculator-a11y-keypad-hint = Use the calculator keys, or type an expression directly.
calculator-a11y-workspace = Calculator and graphing workspace
calculator-a11y-remove-formula = Permanently removes this saved formula.
calculator-amount = Amount
calculator-amount-to-convert = Amount to convert
calculator-currency = Currency
calculator-quantity = Quantity
calculator-unit-conversion = Unit conversion
calculator-unit-conversion-description = Convert an amount from one unit to another, offline.

## Facts, property labels and section titles
calculator-graph-add = Add { $kind }
calculator-graph-delete = Delete { $kind } { $number }
calculator-graph-show = Show { $kind } { $number }
calculator-graph-field = { $kind } { $number } formula
calculator-next-unit = Change { $unit } to the next unit

## What the window shows
calculator-result = Result { $value }
calculator-invalid-expression = The expression is not valid: { $detail }
calculator-graph-functions = Functions
calculator-graph-surfaces = Surfaces
