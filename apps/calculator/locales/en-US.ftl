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

## The keypad's two pages. A window too narrow for the full scientific pad
## folds the function keys onto the second page instead of dropping them.
calculator-page-numbers = Numbers
calculator-page-functions = { $count ->
    [one] 1 function
   *[other] { $count } functions
}

## Every value the display shows can be copied; these name the controls.
calculator-copy-expression = Copy the expression
calculator-copy-result = Copy the result
calculator-copy-answer = Copy the previous answer
calculator-copy-memory = Copy the memory value
