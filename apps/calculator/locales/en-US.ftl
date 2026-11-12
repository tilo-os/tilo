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
calculator-error = Error
calculator-graph-functions = Functions
calculator-graph-surfaces = Surfaces

## What the modes, the quantities and the keypad keys are called
calculator-mode-calculator = Calculator
calculator-mode-convert = Convert
calculator-mode-graph-2d = Graph 2D
calculator-mode-graph-3d = Graph 3D
calculator-quantity-length = Length
calculator-quantity-mass = Mass
calculator-quantity-volume = Volume
calculator-quantity-time = Time
calculator-quantity-speed = Speed
calculator-quantity-area = Area
calculator-quantity-data = Data
calculator-quantity-temperature = Temperature
calculator-key-equals = Evaluate expression
calculator-key-clear = Clear expression
calculator-key-backspace = Delete the last character
calculator-key-sign = Toggle sign
calculator-key-percent = Percent
calculator-key-answer = Insert previous answer
calculator-key-mem-add = Add expression to memory
calculator-key-mem-sub = Subtract expression from memory
calculator-key-mem-recall = Recall memory
calculator-key-mem-clear = Clear memory
calculator-key-square = Square expression
calculator-key-functions-page = Function keys, second page
calculator-key-numbers-page = Number keys, first page
calculator-key-sin = Sine
calculator-key-cos = Cosine
calculator-key-tan = Tangent
calculator-key-asin = Inverse sine
calculator-key-acos = Inverse cosine
calculator-key-atan = Inverse tangent
calculator-key-sqrt = Square root
calculator-key-integral = Definite integral
calculator-key-root = Nth root
calculator-key-abs = Absolute value
calculator-key-sinh = Hyperbolic sine
calculator-key-cosh = Hyperbolic cosine
calculator-key-tanh = Hyperbolic tangent
calculator-key-gamma = Gamma function
calculator-key-ln = Natural logarithm
calculator-key-log10 = Base 10 logarithm
calculator-key-exp = Exponential
calculator-key-reciprocal = Reciprocal
calculator-key-power = Power
calculator-key-multiply = Multiply
calculator-key-divide = Divide
calculator-key-add = Add
calculator-key-subtract = Subtract
calculator-key-pi = Pi
calculator-key-euler = Euler's number
calculator-key-other = Calculator key { $key }
