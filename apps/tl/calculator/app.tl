app so.tilo.calculator "Calculator" (
    icon: .hash,
    category: .utility,
    tagline: "Arithmetic and scientific calculations",
    keywords: ["calc", "math", "maths", "arithmetic", "scientific calculator", "numbers", "gnome calculator", "kcalc", "qalculate"],
)

/// One calculation worked out: what was typed, and what it came to. It stands in the calculator's
/// own record, so search finds it and opening it puts its expression back on the display.
type Calculation (untitled: "Calculation") {
    expr: Text
    result: Text
}

/// A curve graphed over x, in the 2D graph.
type Curve (untitled: "y =") {
    expr: Text
    visible: Bool = true
}

/// A surface graphed over x and y, in the 3D graph.
type Surface (untitled: "z =") {
    expr: Text
    visible: Bool = true
}

/// A quantity unit conversion works within.
enum Quantity {
    length
    mass
    volume
    temperature
    time
    speed
    area
    data
    /// Currency
    currency
}

/// The workspace's calculator: what it worked out, oldest first, and the curves and surfaces its
/// graphs draw. What is being typed, the memory and the graphs' view are the window's own, so two
/// people at one calculator never overwrite each other.
type Calculator {
    calculations: Collection<Calculation>
    curves: Collection<Curve>
    surfaces: Collection<Surface>
}

root calculator: Calculator

default view calculator for Calculator

default view calculation for Calculation

/// Works out an expression and keeps the result in the record. The result is checked here, where
/// the log records it with the expression that made it, and the display says the same number.
primary action calculate(
    c: Calculator,
    expr: Text,
    in_degrees: Bool = false,
    answer: Float = 0.0,
    memory: Float = 0.0,
) -> Calculation (icon: .hash, label: "Calculate") {
    require outcome(expr, in_degrees, answer, memory) != "" "That expression is not one this calculator can work out"
    let result = outcome(expr, in_degrees, answer, memory)
    let kept = c.calculations.insert(Calculation(expr: expr, result: result))
    // A calculator's record is its recent work: the oldest goes as a new one arrives.
    if c.calculations.len() > 100 {
        if let oldest = c.calculations.first() { c.calculations.remove(oldest) }
    }
    kept
}

/// What `text` comes to as the record says it, or empty where it is no number.
fn outcome(text: Text, degrees: Bool, answer: Float, memory: Float) -> Text {
    let value = eval(text, degrees, answer, memory)
    if value.is_nan() {
        return ""
    }
    said(value)
}

/// Puts `text` on the person's clipboard, as the system's copy does.
action copy_text(text: Text) uses clipboard (icon: .copy, label: "Copy") {
    clipboard.copy(text)
}

/// Adds a curve to the 2D graph.
action add_curve(c: Calculator, formula: Text = "") -> Curve (icon: .plus, label: "Add curve") {
    c.curves.insert(Curve(expr: formula))
}

/// Adds a surface to the 3D graph.
action add_surface(c: Calculator, formula: Text = "") -> Surface (
    icon: .plus,
    label: "Add surface",
) {
    c.surfaces.insert(Surface(expr: formula))
}

/// Takes a curve off the 2D graph.
destructive action delete_curve(x: Curve) (icon: .trash, label: "Delete") {
    calculator.curves.remove(x)
}

/// Takes a surface off the 3D graph.
destructive action delete_surface(x: Surface) (icon: .trash, label: "Delete") {
    calculator.surfaces.remove(x)
}

/// Takes one calculation out of the record.
destructive action forget(x: Calculation) (icon: .trash, label: "Delete") {
    calculator.calculations.remove(x)
}

/// Clears the record of everything worked out so far.
destructive action clear(c: Calculator) (icon: .trash, label: "Clear calculations") {
    c.calculations.remove_where(x -> true)
}

/// The calculator's window: the mode chosen over the window's own state, which is kept across a
/// mode switch so a person finds what they typed where they left it.
view calculator(c: Calculator) {
    state expr: Text = ""
    state answer: Float = 0.0
    state memory: Float = 0.0
    state deg: Bool = false
    state fresh: Bool = false
    state mode: Int = 0
    state amount: Text = "1"
    state quantity: Quantity = .length
    state from: Text = "m"
    state to: Text = "ft"
    state width: Float = 10.0
    state centre: Float = 0.0
    state yaw: Float = 0.6
    state pitch: Float = 0.5
    state zoom: Float = 1.0
    state traced: Option<Int> = .none
    state grabbed: Option<Float> = .none
    state grabbed_y: Float = 0.0
    state held_centre: Float = 0.0
    state held_yaw: Float = 0.0
    state held_pitch: Float = 0.0
    window(
        c,
        expr,
        answer,
        memory,
        deg,
        fresh,
        mode,
        amount,
        quantity,
        from,
        to,
        width,
        centre,
        yaw,
        pitch,
        zoom,
        traced,
        grabbed,
        grabbed_y,
        held_centre,
        held_yaw,
        held_pitch,
    )
}

/// A calculation opened: the calculator, with its own expression on the display.
view calculation(x: Calculation) {
    state expr: Text = x.expr
    state answer: Float = 0.0
    state memory: Float = 0.0
    state deg: Bool = false
    state fresh: Bool = false
    state mode: Int = 0
    state amount: Text = "1"
    state quantity: Quantity = .length
    state from: Text = "m"
    state to: Text = "ft"
    state width: Float = 10.0
    state centre: Float = 0.0
    state yaw: Float = 0.6
    state pitch: Float = 0.5
    state zoom: Float = 1.0
    state traced: Option<Int> = .none
    state grabbed: Option<Float> = .none
    state grabbed_y: Float = 0.0
    state held_centre: Float = 0.0
    state held_yaw: Float = 0.0
    state held_pitch: Float = 0.0
    window(
        calculator,
        expr,
        answer,
        memory,
        deg,
        fresh,
        mode,
        amount,
        quantity,
        from,
        to,
        width,
        centre,
        yaw,
        pitch,
        zoom,
        traced,
        grabbed,
        grabbed_y,
        held_centre,
        held_yaw,
        held_pitch,
    )
}

/// What every window shows: the modes as the window's own switcher, the unit over it, and the
/// chosen mode's face over the window's state.
view window(
    c: Calculator,
    expr: State<Text>,
    answer: State<Float>,
    memory: State<Float>,
    deg: State<Bool>,
    fresh: State<Bool>,
    mode: State<Int>,
    amount: State<Text>,
    quantity: State<Quantity>,
    from: State<Text>,
    to: State<Text>,
    width: State<Float>,
    centre: State<Float>,
    yaw: State<Float>,
    pitch: State<Float>,
    zoom: State<Float>,
    traced: State<Option<Int>>,
    grabbed: State<Option<Float>>,
    grabbed_y: State<Float>,
    held_centre: State<Float>,
    held_yaw: State<Float>,
    held_pitch: State<Float>,
) {
    std.page(c, canvas: true) {
        header: {
            unit(deg)
        }
        ui.ViewTabs(selected: mode, label: "Mode") {
            ui.Segment("Calculator")
            ui.Segment("Convert")
            ui.Segment("2D graph")
            ui.Segment("3D graph")
            on change(chosen): set(mode, chosen)
        }
        if mode == 0 {
            calculator_face(c, expr, answer, memory, deg, fresh)
        } else if mode == 1 {
            convert_face(amount, quantity, from, to)
        } else if mode == 2 {
            graph2d_face(c, deg, answer, memory, width, centre, traced, grabbed, held_centre)
        } else {
            graph3d_face(
                c,
                deg,
                answer,
                memory,
                yaw,
                pitch,
                zoom,
                grabbed,
                grabbed_y,
                held_yaw,
                held_pitch,
            )
        }
    }
}

/// The angle unit the trigonometric functions work in, named by the unit they work in now.
view unit(deg: State<Bool>) {
    ui.Button(if deg { "Degrees" } else { "Radians" }, role: .plain, toggled: deg) {
        on press: set(deg, not deg)
    }
}

/// The calculator: what is typed and its result over the keypad, and the calculations before this
/// one beside them, each opening the calculator on its expression.
view calculator_face(
    c: Calculator,
    expr: State<Text>,
    answer: State<Float>,
    memory: State<Float>,
    deg: State<Bool>,
    fresh: State<Bool>,
) {
    let typed = "{expr}"
    let value = eval(typed, deg, answer, memory)
    let out = said(value)
    let valid = not value.is_nan()
    let commit = if valid {
        calculate(c, typed, in_degrees: deg, answer: answer, memory: memory)
            .then(set(expr, out))
            .then(set(answer, value))
            .then(set(fresh, true))
    } else { set(fresh, fresh) }
    split(side: .trailing, collapse: .stack) {
        primary(min: 18rem) {
            stack(gap: .section) {
                display(expr, out, typed, valid, commit, answer)
                keys(expr, memory, fresh, value, valid, commit)
            }
        }
        secondary(min: 16rem) {
            if not c.calculations.is_empty() {
                past(c, expr, fresh)
            }
        }
    }
}

/// What is typed, and what it comes to. The result is a field a person can select and copy, as
/// every number in Tilo is.
view display(
    expr: State<Text>,
    out: Text,
    typed: Text,
    valid: Bool,
    commit: Action,
    answer: Float,
) {
    ui.TextInput(bind: expr, placeholder: "Type a calculation", autofocus: true, lines: 2) {
        on submit: commit
    }
    ui.TextInput(text: if valid { out } else { "" }, read_only: true, placeholder: "Result")
    row(gap: .tight) {
        ui.Button("Copy expression", icon: .copy, icon_only: true, role: .secondary, size: .small) {
            on press: copy_text(typed)
        }
        if valid {
            ui.Button("Copy result", icon: .copy, icon_only: true, role: .secondary, size: .small) {
                on press: copy_text(out)
            }
        }
        ui.Button("Copy answer", icon: .history, icon_only: true, role: .secondary, size: .small) {
            on press: copy_text(said(answer))
        }
    }
    if not valid and typed != "" {
        ui.Text("Not a complete expression yet", role: .caption, secondary: true)
    }
}

/// The memory keys, and the number the calculator remembers.
view keys(
    expr: State<Text>,
    memory: State<Float>,
    fresh: State<Bool>,
    value: Float,
    valid: Bool,
    commit: Action,
) {
    row(gap: .tight) {
        ui.Button(
            "M+",
            role: .secondary,
            size: .small,
            enabled: valid,
            description: "The expression is no number yet",
        ) {
            on press: set(memory, memory + value)
        }
        ui.Button(
            "M−",
            role: .secondary,
            size: .small,
            enabled: valid,
            description: "The expression is no number yet",
        ) {
            on press: set(memory, memory - value)
        }
        ui.Button("MR", role: .secondary, size: .small) {
            on press: set(expr, begun(expr, said(memory), fresh)).then(set(fresh, false))
        }
        ui.Button("MC", role: .secondary, size: .small) {
            on press: set(memory, 0.0)
        }
        spacer
        if memory != 0.0 {
            ui.Text("M {said(memory)}", role: .caption, secondary: true, numeric: true)
        }
    }
    grid(columns: 6, gap: .tight) {
        ui.Button("AC", role: .secondary) {
            on press: set(expr, "").then(set(fresh, false))
        }
        ui.Button("Delete", icon: .delete, icon_only: true, role: .secondary) {
            on press: set(expr, backspace(expr)).then(set(fresh, false))
        }
        ui.Button("(", role: .key) {
            on press: set(expr, begun(expr, "(", fresh)).then(set(fresh, false))
        }
        ui.Button(")", role: .key) {
            on press: set(expr, expr + ")").then(set(fresh, false))
        }
        ui.Button("π", role: .key) {
            on press: set(expr, begun(expr, "pi", fresh)).then(set(fresh, false))
        }
        ui.Button("e", role: .key) {
            on press: set(expr, begun(expr, "e", fresh)).then(set(fresh, false))
        }
        ui.Button("sin", role: .key) {
            on press: set(expr, begun(expr, "sin(", fresh)).then(set(fresh, false))
        }
        ui.Button("cos", role: .key) {
            on press: set(expr, begun(expr, "cos(", fresh)).then(set(fresh, false))
        }
        ui.Button("tan", role: .key) {
            on press: set(expr, begun(expr, "tan(", fresh)).then(set(fresh, false))
        }
        ui.Button("ln", role: .key) {
            on press: set(expr, begun(expr, "ln(", fresh)).then(set(fresh, false))
        }
        ui.Button("log", role: .key) {
            on press: set(expr, begun(expr, "log10(", fresh)).then(set(fresh, false))
        }
        ui.Button("√", role: .key) {
            on press: set(expr, begun(expr, "sqrt(", fresh)).then(set(fresh, false))
        }
        ui.Button("asin", role: .key) {
            on press: set(expr, begun(expr, "asin(", fresh)).then(set(fresh, false))
        }
        ui.Button("acos", role: .key) {
            on press: set(expr, begun(expr, "acos(", fresh)).then(set(fresh, false))
        }
        ui.Button("atan", role: .key) {
            on press: set(expr, begun(expr, "atan(", fresh)).then(set(fresh, false))
        }
        ui.Button("^", role: .key) {
            on press: set(expr, expr + "^").then(set(fresh, false))
        }
        ui.Button("x²", role: .key) {
            on press: set(expr, squared(expr)).then(set(fresh, false))
        }
        ui.Button("1/x", role: .key) {
            on press: set(expr, reciprocal(expr)).then(set(fresh, false))
        }
        ui.Button("7", role: .key) {
            on press: set(expr, begun(expr, "7", fresh)).then(set(fresh, false))
        }
        ui.Button("8", role: .key) {
            on press: set(expr, begun(expr, "8", fresh)).then(set(fresh, false))
        }
        ui.Button("9", role: .key) {
            on press: set(expr, begun(expr, "9", fresh)).then(set(fresh, false))
        }
        ui.Button("÷", role: .key) {
            on press: set(expr, expr + "/").then(set(fresh, false))
        }
        ui.Button("×", role: .key) {
            on press: set(expr, expr + "*").then(set(fresh, false))
        }
        ui.Button("−", role: .key) {
            on press: set(expr, expr + "-").then(set(fresh, false))
        }
        ui.Button("4", role: .key) {
            on press: set(expr, begun(expr, "4", fresh)).then(set(fresh, false))
        }
        ui.Button("5", role: .key) {
            on press: set(expr, begun(expr, "5", fresh)).then(set(fresh, false))
        }
        ui.Button("6", role: .key) {
            on press: set(expr, begun(expr, "6", fresh)).then(set(fresh, false))
        }
        ui.Button("+", role: .key) {
            on press: set(expr, expr + "+").then(set(fresh, false))
        }
        ui.Button("%", role: .key) {
            on press: set(expr, expr + "%").then(set(fresh, false))
        }
        ui.Button("!", role: .key) {
            on press: set(expr, expr + "!").then(set(fresh, false))
        }
        ui.Button("1", role: .key) {
            on press: set(expr, begun(expr, "1", fresh)).then(set(fresh, false))
        }
        ui.Button("2", role: .key) {
            on press: set(expr, begun(expr, "2", fresh)).then(set(fresh, false))
        }
        ui.Button("3", role: .key) {
            on press: set(expr, begun(expr, "3", fresh)).then(set(fresh, false))
        }
        ui.Button("±", role: .key) {
            on press: set(expr, negated(expr)).then(set(fresh, false))
        }
        ui.Button(",", role: .key) {
            on press: set(expr, expr + ",").then(set(fresh, false))
        }
        ui.Button("ans", role: .key) {
            on press: set(expr, begun(expr, "ans", fresh)).then(set(fresh, false))
        }
        ui.Button("0", role: .key) {
            on press: set(expr, begun(expr, "0", fresh)).then(set(fresh, false))
        }
        ui.Button(".", role: .key) {
            on press: set(expr, begun(expr, ".", fresh)).then(set(fresh, false))
        }
        ui.Button("00", role: .key) {
            on press: set(expr, begun(expr, "00", fresh)).then(set(fresh, false))
        }
        ui.Button("|x|", role: .key) {
            on press: set(expr, begun(expr, "abs(", fresh)).then(set(fresh, false))
        }
        ui.Button("ⁿ√", role: .key) {
            on press: set(expr, begun(expr, "root(", fresh)).then(set(fresh, false))
        }
        ui.Button("eˣ", role: .key) {
            on press: set(expr, begun(expr, "exp(", fresh)).then(set(fresh, false))
        }
        ui.Button("sinh", role: .key) {
            on press: set(expr, begun(expr, "sinh(", fresh)).then(set(fresh, false))
        }
        ui.Button("cosh", role: .key) {
            on press: set(expr, begun(expr, "cosh(", fresh)).then(set(fresh, false))
        }
        ui.Button("tanh", role: .key) {
            on press: set(expr, begun(expr, "tanh(", fresh)).then(set(fresh, false))
        }
        ui.Button("Γ", role: .key) {
            on press: set(expr, begun(expr, "gamma(", fresh)).then(set(fresh, false))
        }
        ui.Button("∫", role: .key) {
            on press: set(expr, begun(expr, "int(", fresh)).then(set(fresh, false))
        }
        ui.Button("mod", role: .key) {
            on press: set(expr, begun(expr, "mod(", fresh)).then(set(fresh, false))
        }
        ui.Button(
            "=",
            role: .primary,
            enabled: valid,
            description: "The expression is no number yet",
        ) {
            on press: commit
        }
    }
}

/// The calculations before this one, newest first; pressing one puts its expression back on the
/// display to work on again.
view past(c: Calculator, expr: State<Text>, fresh: State<Bool>) {
    ui.Section("Earlier") {
        row(distribute: .end) {
            button(clear(c)) as clear_record
        }
        ui.VirtualList(in_column: false) {
            // A past calculation goes back on the display, to work on again here.
            on activate(x: Calculation): set(expr, x.expr).then(set(fresh, false))
            for x in c.calculations.sort_by(x -> created(x)?.at, desc) {
                ui.ListItem(item: x) {
                    ui.ListRow(x.expr, value: x.result)
                }
            }
        }
    }
}

/// Unit conversion, from the fixed table: a quantity, an amount, and two units of it. Nothing is
/// fetched, so it works with the network off; money is not here, since a rate is a fact about the
/// moment and a stale one would lie.
view convert_face(
    amount: State<Text>,
    quantity: State<Quantity>,
    from: State<Text>,
    to: State<Text>,
) {
    let units = symbols(quantity)
    let src = if units.contains(from) { from } else { default_from(quantity) }
    let dst = if units.contains(to) { to } else { default_to(quantity) }
    let value = amount.trim().to_float() ?? bad()
    let converted = convert_value(quantity, value, src, dst)
    stack(gap: .section) {
        ui.ToggleGroup(caption: "Quantity", fill: true) {
            for q in cases(Quantity) {
                ui.Segment(q.label(), chosen: quantity == q) {
                    on press: set(quantity, q)
                        .then(set(from, default_from(q)))
                        .then(set(to, default_to(q)))
                }
            }
        }
        if quantity == .currency {
            ui.Text(
                "A rate has to come from somewhere: Tilo has no exchange-rate source here, so this page converts units and not money.",
                role: .body,
                secondary: true,
            )
        } else {
            stack(gap: .related) {
                ui.TextInput(bind: amount, placeholder: "Amount", autofocus: true)
                row(gap: .related) {
                    ui.Dropdown(options: units, bind: from, caption: "From", fill)
                    ui.Button("Swap", icon: .arrow_up_down, icon_only: true) {
                        on press: set(from, dst).then(set(to, src))
                    }
                    ui.Dropdown(options: units, bind: to, caption: "To", fill)
                }
            }
            ui.TextInput(
                text: if converted.is_nan() { "" } else { said(converted) },
                read_only: true,
                placeholder: "Result",
            )
            if not converted.is_nan() {
                ui.Text(
                    "{said(value)} {src} = {said(converted)} {dst}",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    }
}

/// The 2D graph: the visible curves over x, the grid, and the zoom and pan of what is shown.
view graph2d_face(
    c: Calculator,
    deg: State<Bool>,
    answer: State<Float>,
    memory: State<Float>,
    width: State<Float>,
    centre: State<Float>,
    traced: State<Option<Int>>,
    grabbed: State<Option<Float>>,
    held_centre: State<Float>,
) {
    let shown = c.curves.filter(x -> x.visible)
    let xmin = centre - width
    let xmax = centre + width
    let ymin = 0.0 - width
    let ymax = 0.0 + width
    split(side: .trailing, collapse: .stack) {
        primary(min: 18rem) {
            stack(gap: .related) {
                ui.Drawing(
                    plot2d(shown, deg, answer, memory, xmin, xmax, ymin, ymax, traced),
                    meaning: .chart,
                    label: .some("The curves over x"),
                    height: .some(320.0),
                    motion: .brisk,
                ) {
                    // The pointer reads the curve where it stands, a drag pans what is shown, the
                    // wheel and a pinch zoom it.
                    on pointer(x): set(traced, sample_near(x))
                    on drag_start(x): set(grabbed, x).then(set(held_centre, centre))
                    on drag(x): set(centre, panned(held_centre, grabbed ?? x, x, width))
                    on drag_end: set(grabbed, .none)
                    on wheel(dx, dy): set(width, zoomed_by(width, dy))
                        .then(set(centre, centre + dx / 200.0 * width))
                    on pinch(scale): set(width, scaled_by(width, scale))
                }
                row(gap: .tight) {
                    ui.Button("Zoom out", icon: .minus, icon_only: true) {
                        on press: set(width, zoomed_out(width))
                    }
                    ui.Button("Zoom in", icon: .plus, icon_only: true) {
                        on press: set(width, zoomed_in(width))
                    }
                    ui.Button("Reset", icon: .rotate_ccw, icon_only: true) {
                        on press: set(width, 10.0).then(set(centre, 0.0)).then(set(traced, .none))
                    }
                    ui.Button("Left", icon: .chevron_left, icon_only: true) {
                        on press: set(centre, centre - width / 2.0)
                    }
                    ui.Button("Right", icon: .chevron_right, icon_only: true) {
                        on press: set(centre, centre + width / 2.0)
                    }
                    if let at = traced {
                        ui.Text(
                            trace_said(shown, deg, answer, memory, xmin, xmax, at),
                            role: .caption,
                            numeric: true,
                        )
                    } else {
                        ui.Text(
                            "Point at the graph to read a value; drag to pan, scroll to zoom",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            }
        }
        secondary(min: 14rem) {
            curves_panel(c, "Functions")
        }
    }
}

/// The 3D graph: the visible surfaces over x and y, orbited and zoomed by the buttons under it.
view graph3d_face(
    c: Calculator,
    deg: State<Bool>,
    answer: State<Float>,
    memory: State<Float>,
    yaw: State<Float>,
    pitch: State<Float>,
    zoom: State<Float>,
    grabbed: State<Option<Float>>,
    grabbed_y: State<Float>,
    held_yaw: State<Float>,
    held_pitch: State<Float>,
) {
    let shown = c.surfaces.filter(x -> x.visible)
    // A graph with nothing stored yet draws the surface most people start from; the first
    // surface added replaces it.
    let starter = c.surfaces.is_empty()
    split(side: .trailing, collapse: .stack) {
        primary(min: 18rem) {
            stack(gap: .related) {
                ui.Drawing(
                    plot3d(shown, starter, deg, answer, memory, yaw, pitch, zoom),
                    meaning: .chart,
                    label: .some("The surfaces over x and y"),
                    height: .some(360.0),
                    motion: .brisk,
                ) {
                    // A drag turns the surface about: across it turns it, up and down tilts it.
                    on drag_start(x, y): set(grabbed, x)
                        .then(set(grabbed_y, y))
                        .then(set(held_yaw, yaw))
                        .then(set(held_pitch, pitch))
                    on drag(x, y): set(yaw, held_yaw + (x - (grabbed ?? x)) * 3.0)
                        .then(
                            set(pitch, tilted(held_pitch + (y - grabbed_y) * 2.0)),
                        )
                    on drag_end: set(grabbed, .none)
                    on wheel(dy): set(zoom, zoomed_3d(zoom, dy))
                    on pinch(scale): set(zoom, (zoom * scale).clamp(0.2, 5.0))
                }
                row(gap: .tight) {
                    ui.Button("Orbit left", icon: .chevron_left, icon_only: true) {
                        on press: set(yaw, yaw - 0.2)
                    }
                    ui.Button("Orbit right", icon: .chevron_right, icon_only: true) {
                        on press: set(yaw, yaw + 0.2)
                    }
                    ui.Button("Tilt up", icon: .chevron_up, icon_only: true) {
                        on press: set(pitch, tilted_up(pitch))
                    }
                    ui.Button("Tilt down", icon: .chevron_down, icon_only: true) {
                        on press: set(pitch, tilted_down(pitch))
                    }
                    ui.Button("Zoom out", icon: .minus, icon_only: true) {
                        on press: set(zoom, closer_in(zoom))
                    }
                    ui.Button("Zoom in", icon: .plus, icon_only: true) {
                        on press: set(zoom, closer_out(zoom))
                    }
                    ui.Button("Reset", icon: .rotate_ccw, icon_only: true) {
                        on press: set(yaw, 0.6).then(set(pitch, 0.5)).then(set(zoom, 1.0))
                    }
                }
            }
        }
        secondary(min: 14rem) {
            surfaces_panel(c, "Surfaces")
        }
    }
}

/// The graph's view, moved by the buttons under it: bounded, so a plot never vanishes or turns
/// inside out.
fn zoomed_out(w: Float) -> Float {
    (w * 1.5).min(1000.0)
}

fn zoomed_in(w: Float) -> Float {
    (w / 1.5).max(0.01)
}

fn tilted_up(p: Float) -> Float {
    (p + 0.1).min(1.45)
}

fn tilted_down(p: Float) -> Float {
    (p - 0.1).max(-1.45)
}

fn closer_in(z: Float) -> Float {
    (z / 1.25).max(0.2)
}

fn closer_out(z: Float) -> Float {
    (z * 1.25).min(5.0)
}

/// The sample nearest a point across the drawing, of the 64 a curve is drawn through.
fn sample_near(x: Float) -> Int {
    (x * 63.0).round().clamp(0, 63)
}

/// Where the graph stands after a drag from `from` to `to` across it: the world moves the other way.
fn panned(held: Float, from: Float, to: Float, width: Float) -> Float {
    held - (to - from) * 2.0 * width
}

/// The graph's width after the wheel turned `dy` pixels: a notch up narrows it.
fn zoomed_by(w: Float, dy: Float) -> Float {
    (w * (1.0 - dy / 400.0)).clamp(0.01, 1000.0)
}

/// The graph's width after a pinch asked for `scale`.
fn scaled_by(w: Float, scale: Float) -> Float {
    if scale <= 0.0 {
        return w
    }
    (w / scale).clamp(0.01, 1000.0)
}

/// The 3D orbit's tilt, kept inside where the surface still reads.
fn tilted(p: Float) -> Float {
    p.clamp(-1.45, 1.45)
}

/// The 3D view's zoom after the wheel turned `dy` pixels.
fn zoomed_3d(z: Float, dy: Float) -> Float {
    (z * (1.0 + dy / 400.0)).clamp(0.2, 5.0)
}

/// The 2D graph's formulas: each curve's expression, editable where it stands, a box that shows
/// it or hides it, and its own menu, which deletes it.
view curves_panel(c: Calculator, title: Text) {
    ui.Section(title) {
        for x in c.curves {
            row(gap: .related, align: .center) {
                ui.Checkbox(bind: x.visible)
                ui.TextInput(bind: x.expr, placeholder: "y = f(x)", fill)
            }
        }
        row(gap: .related) {
            button(add_curve(c)) as new_curve
        }
        if c.curves.is_empty() {
            ui.Text("Nothing graphed yet", role: .caption, secondary: true)
        }
    }
}

/// The 3D graph's formulas, as the 2D graph's are.
view surfaces_panel(c: Calculator, title: Text) {
    ui.Section(title) {
        for x in c.surfaces {
            row(gap: .related, align: .center) {
                ui.Checkbox(bind: x.visible)
                ui.TextInput(bind: x.expr, placeholder: "z = f(x, y)", fill)
            }
        }
        row(gap: .related) {
            button(add_surface(c)) as new_surface
        }
        if c.surfaces.is_empty() {
            ui.Text("Nothing graphed yet", role: .caption, secondary: true)
        }
    }
}

/// A key that starts a value: after a worked-out result it starts the next expression afresh,
/// where an operator continues the result as it stands.
// ---------------------------------------------------------------------------
// What a keypress does to the expression
// ---------------------------------------------------------------------------
fn begun(x: Text, s: Text, fresh: Bool) -> Text {
    if fresh {
        return s
    }
    x + s
}

/// Takes the last character off, and the name of a function whole: `sin(` goes as a piece.
fn backspace(x: Text) -> Text {
    if x == "" {
        return ""
    }
    let cut = x.slice(0, x.len() - 1)
    if x.ends_with("(") {
        return trailing_letters_cut(cut)
    }
    cut
}

/// `x` without the run of letters it ends in.
fn trailing_letters_cut(x: Text) -> Text {
    if x == "" {
        return ""
    }
    let last = x.slice(x.len() - 1, x.len())
    if is_letter(last) {
        return trailing_letters_cut(x.slice(0, x.len() - 1))
    }
    x
}

/// A negative of the whole expression, or the leading minus the field had.
fn negated(x: Text) -> Text {
    if x.trim() == "" {
        return "-"
    }
    if x.starts_with("-") {
        return x.slice(1, x.len())
    }
    "-(" + x + ")"
}

/// The whole expression squared.
fn squared(x: Text) -> Text {
    if x.trim() == "" {
        return ""
    }
    "(" + x + ")^2"
}

/// One over the whole expression.
fn reciprocal(x: Text) -> Text {
    if x.trim() == "" {
        return "1/("
    }
    "1/(" + x + ")"
}

/// The result of `text`, or not-a-number when it does not parse. `degrees` works the
/// trigonometric functions in degrees; `answer` and `memory` are the values named `ans` and
/// `mem`.
// ---------------------------------------------------------------------------
// The expression engine
// ---------------------------------------------------------------------------
//
// A recursive-descent parse over the characters of the expression, its place kept in a map the
// parse carries with it. `+` and `-` bind loosest, then `*` and `/` (implicit multiplication
// included), then a leading minus, then `^` (right to left), then `!` and `%` on the value just
// read. A parse that goes wrong answers not-a-number, which the display reads as no result.
//
// The typesetting the display shows is plain text: what a person types is what the parser reads.
fn eval(text: Text, degrees: Bool, answer: Float, memory: Float) -> Float {
    eval_at(text, degrees, answer, memory, 0.0, 0.0)
}

/// The same, with `x` and `y` bound: what a plotted function is sampled at.
fn eval_at(text: Text, degrees: Bool, answer: Float, memory: Float, x: Float, y: Float) -> Float {
    eval_with(text, degrees, answer, memory, x, y, "x")
}

/// The same, sampling `var` instead: what an integral reads at each point.
fn eval_in(
    text: Text,
    degrees: Bool,
    answer: Float,
    memory: Float,
    var: Text,
    value: Float,
) -> Float {
    eval_with(text, degrees, answer, memory, value, 0.0, var)
}

fn eval_with(
    text: Text,
    degrees: Bool,
    answer: Float,
    memory: Float,
    x: Float,
    y: Float,
    var: Text,
) -> Float {
    let cs = normalize(text).split("")
    let st = Map<Float>()
    st.put("i", 0.0)
    st.put("ans", answer)
    st.put("mem", memory)
    st.put("deg", if degrees { 1.0 } else { 0.0 })
    st.put("x", x)
    st.put("y", y)
    st.put(var, x)
    let value = sum(cs, st)
    if peek(cs, st) != "" {
        return bad()
    }
    value
}

/// What a person can type of the operators the parser knows, as the parser reads it.
fn normalize(text: Text) -> Text {
    text
        .replace("×", "*")
        .replace("÷", "/")
        .replace("−", "-")
        .replace("π", "pi")
        .replace("√", "sqrt")
}

/// No number at all: what a parse that went wrong answers.
fn bad() -> Float {
    0.0 / 0.0
}

fn index(st: Map<Float>) -> Float {
    st.get("i") ?? 0.0
}

/// Over the spaces from the place, answering the character the parse then stands at.
fn skip(cs: List<Text>, st: Map<Float>) -> Text {
    if peek_raw(cs, st) == " " {
        st.put("i", index(st) + 1.0)
        skip(cs, st)
    } else { peek_raw(cs, st) }
}

fn peek_raw(cs: List<Text>, st: Map<Float>) -> Text {
    cs.at(index(st).to_int()) ?? ""
}

/// The character the parse is at, spaces skipped; empty at the end.
fn peek(cs: List<Text>, st: Map<Float>) -> Text {
    skip(cs, st)
}

fn take(cs: List<Text>, st: Map<Float>) -> Text {
    let c = peek(cs, st)
    if c != "" { st.put("i", index(st) + 1.0) }
    c
}

fn is_letter(c: Text) -> Bool {
    if c == "" {
        return false
    }
    "abcdefghijklmnopqrstuvwxyz".contains(c.lower())
}

/// The digit `c` names, or -1 when it names none.
fn digit(c: Text) -> Float {
    if c == "" {
        return -1.0
    }
    return match c.to_int() {
        .some(n) => n.to_float(),
        .none => -1.0,
    }
}

fn starts_value(c: Text) -> Bool {
    digit(c) >= 0.0 or c == "." or c == "(" or is_letter(c)
}

// -- the grammar, by binding power ------------------------------------------------------------
fn sum(cs: List<Text>, st: Map<Float>) -> Float {
    sum_rest(cs, st, product(cs, st))
}

fn sum_rest(cs: List<Text>, st: Map<Float>, left: Float) -> Float {
    let c = peek(cs, st)
    if c == "+" {
        take(cs, st)
        return sum_rest(cs, st, left + product(cs, st))
    }
    if c == "-" {
        take(cs, st)
        return sum_rest(cs, st, left - product(cs, st))
    }
    left
}

fn product(cs: List<Text>, st: Map<Float>) -> Float {
    product_rest(cs, st, unary(cs, st))
}

fn product_rest(cs: List<Text>, st: Map<Float>, left: Float) -> Float {
    let c = peek(cs, st)
    if c == "*" {
        take(cs, st)
        return product_rest(cs, st, left * unary(cs, st))
    }
    if c == "/" {
        take(cs, st)
        return product_rest(cs, st, left / unary(cs, st))
    }
    // Implicit multiplication: a value written after a value multiplies it.
    if starts_value(c) {
        return product_rest(cs, st, left * unary(cs, st))
    }
    left
}

fn unary(cs: List<Text>, st: Map<Float>) -> Float {
    let c = peek(cs, st)
    if c == "-" {
        take(cs, st)
        return -unary(cs, st)
    }
    if c == "+" {
        take(cs, st)
        return unary(cs, st)
    }
    power(cs, st)
}

fn power(cs: List<Text>, st: Map<Float>) -> Float {
    let base = postfix(cs, st)
    if peek(cs, st) == "^" {
        take(cs, st)
        return base.pow(unary(cs, st))
    }
    base
}

fn postfix(cs: List<Text>, st: Map<Float>) -> Float {
    postfix_rest(cs, st, atom(cs, st))
}

fn postfix_rest(cs: List<Text>, st: Map<Float>, v: Float) -> Float {
    let c = peek(cs, st)
    if c == "!" {
        take(cs, st)
        return postfix_rest(cs, st, factorial(v))
    }
    if c == "%" {
        take(cs, st)
        return postfix_rest(cs, st, v / 100.0)
    }
    v
}

fn atom(cs: List<Text>, st: Map<Float>) -> Float {
    let c = peek(cs, st)
    if c == "" {
        return bad()
    }
    if digit(c) >= 0.0 or c == "." {
        return number(cs, st)
    }
    if c == "(" {
        take(cs, st)
        let inner = sum(cs, st)
        if take(cs, st) != ")" {
            return bad()
        }
        return inner
    }
    if is_letter(c) {
        return named(cs, st)
    }
    bad()
}

fn named(cs: List<Text>, st: Map<Float>) -> Float {
    let name = read_name(cs, st)
    if peek(cs, st) != "(" {
        return constant(name, st)
    }
    take(cs, st)
    // An integral binds the expression it integrates rather than working it out where it stands:
    // its first argument is kept as text and read again at every point.
    if name == "int" {
        let from = index(st)
        sum(cs, st)
        let to = index(st)
        let integrand = cs
            .map((c, i) -> if i >= from.to_int() and i < to.to_int() { c } else { "" })
            .join("")
        if peek(cs, st) != "," {
            return bad()
        }
        take(cs, st)
        // `int(f, x, a, b)` names the variable it binds; `int(f, a, b)` binds `x`.
        let bounds = index(st)
        let maybe = read_name(cs, st)
        let named = (maybe == "x" or maybe == "y") and peek(cs, st) == ","
        if named { take(cs, st) } else { st.put("i", bounds) }
        let var = if named { maybe } else { "x" }
        let first = sum(cs, st)
        if take(cs, st) != "," {
            return bad()
        }
        let second = sum(cs, st)
        if take(cs, st) != ")" {
            return bad()
        }
        return integrate(integrand, first, second, var, st)
    }
    // A minimum or maximum takes as many numbers as it is given.
    if name == "min" or name == "max" {
        let values = arguments(cs, st)
        if values.is_empty() or take(cs, st) != ")" {
            return bad()
        }
        return if name == "min" {
            least(values, 1, values.first() ?? 0.0)
        } else {
            greatest(values, 1, values.first() ?? 0.0)
        }
    }
    let first = sum(cs, st)
    let second_given = peek(cs, st) == ","
    if second_given { take(cs, st) }
    let second = if second_given { sum(cs, st) } else { 0.0 }
    if take(cs, st) != ")" {
        return bad()
    }
    apply(name, first, second, second_given, st)
}

/// Every argument of a call, in order.
fn arguments(cs: List<Text>, st: Map<Float>) -> List<Float> {
    arguments_rest(cs, st, [sum(cs, st)])
}

fn arguments_rest(cs: List<Text>, st: Map<Float>, so_far: List<Float>) -> List<Float> {
    if peek(cs, st) != "," {
        return so_far
    }
    take(cs, st)
    arguments_rest(cs, st, so_far + [sum(cs, st)])
}

fn least(values: List<Float>, at: Int, low: Float) -> Float {
    let value = values.at(at) ?? low
    let low = if value < low { value } else { low }
    if at + 1 >= values.len() {
        return low
    }
    least(values, at + 1, low)
}

fn greatest(values: List<Float>, at: Int, high: Float) -> Float {
    let value = values.at(at) ?? high
    let high = if value > high { value } else { high }
    if at + 1 >= values.len() {
        return high
    }
    greatest(values, at + 1, high)
}

/// The definite integral of `text` over `[a, b]` in its variable, Simpson's rule on 512
/// intervals: enough for a plotted or typed function, and bounded work however wrong the bounds
/// are.
fn integrate(text: Text, a: Float, b: Float, var: Text, st: Map<Float>) -> Float {
    if a.is_nan() or b.is_nan() or not a.is_finite() or not b.is_finite() {
        return bad()
    }
    let degrees = (st.get("deg") ?? 0.0) > 0.0
    let answer = st.get("ans") ?? 0.0
    let memory = st.get("mem") ?? 0.0
    let h = (b - a) / 512.0
    // The ends count once, the odd points four times and the even ones twice.
    let ends = eval_in(text, degrees, answer, memory, var, a)
        + eval_in(text, degrees, answer, memory, var, b)
    let odds = odd_points(text, degrees, answer, memory, a, h, 1.0, var)
    let evens = even_points(text, degrees, answer, memory, a, h, 2.0, var)
    h / 3.0 * (ends + 4.0 * odds + 2.0 * evens)
}

fn odd_points(
    text: Text,
    degrees: Bool,
    answer: Float,
    memory: Float,
    a: Float,
    h: Float,
    i: Float,
    var: Text,
) -> Float {
    if i >= 512.0 {
        return 0.0
    }
    eval_in(text, degrees, answer, memory, var, a + h * i)
        + odd_points(text, degrees, answer, memory, a, h, i + 2.0, var)
}

fn even_points(
    text: Text,
    degrees: Bool,
    answer: Float,
    memory: Float,
    a: Float,
    h: Float,
    i: Float,
    var: Text,
) -> Float {
    if i >= 512.0 {
        return 0.0
    }
    eval_in(text, degrees, answer, memory, var, a + h * i)
        + even_points(text, degrees, answer, memory, a, h, i + 2.0, var)
}

fn read_name(cs: List<Text>, st: Map<Float>) -> Text {
    let c = peek(cs, st)
    if not is_letter(c) {
        return ""
    }
    take(cs, st)
    c.lower() + read_name_tail(cs, st)
}

/// The rest of a name: letters, digits and underscores, so `log10` and `log2` read as the
/// functions they are, as a calculator's own names do.
fn read_name_tail(cs: List<Text>, st: Map<Float>) -> Text {
    let c = peek(cs, st)
    if not (is_letter(c) or digit(c) >= 0.0 or c == "_") {
        return ""
    }
    take(cs, st)
    c.lower() + read_name_tail(cs, st)
}

fn constant(name: Text, st: Map<Float>) -> Float {
    if name == "pi" {
        return 3.141592653589793
    }
    if name == "tau" {
        return 6.283185307179586
    }
    if name == "e" {
        return 2.718281828459045
    }
    if name == "ans" {
        return st.get("ans") ?? 0.0
    }
    if name == "mem" or name == "m" {
        return st.get("mem") ?? 0.0
    }
    if name == "x" {
        return st.get("x") ?? 0.0
    }
    if name == "y" {
        return st.get("y") ?? 0.0
    }
    bad()
}

/// A function of one value, or of two where `second_given`.
fn apply(name: Text, a: Float, b: Float, second_given: Bool, st: Map<Float>) -> Float {
    let degrees = (st.get("deg") ?? 0.0) > 0.0
    let rad = if degrees { a.to_radians() } else { a }
    // Radians to degrees, where an inverse function answers in degrees.
    let back = if degrees { 57.29577951308232 } else { 1.0 }
    if name == "sin" {
        return rad.sin()
    }
    if name == "cos" {
        return rad.cos()
    }
    if name == "tan" {
        return rad.tan()
    }
    if name == "asin" {
        return a.asin() * back
    }
    if name == "acos" {
        return a.acos() * back
    }
    if name == "atan" {
        return a.atan() * back
    }
    if name == "sinh" {
        return a.sinh()
    }
    if name == "cosh" {
        return a.cosh()
    }
    if name == "tanh" {
        return a.tanh()
    }
    if name == "asinh" {
        return asinh(a)
    }
    if name == "acosh" {
        return acosh(a)
    }
    if name == "atanh" {
        return atanh(a)
    }
    if name == "rad" {
        return a.to_radians()
    }
    if name == "deg" {
        return a.to_degrees()
    }
    if name == "atan2" and second_given {
        return atan2(a, b) * back
    }
    if name == "ln" {
        return a.ln()
    }
    if name == "log10" or name == "log" and not second_given {
        return a.log10()
    }
    if name == "log2" {
        return a.log2()
    }
    if name == "log" {
        // `log(base, value)`, as a calculator writes it.
        return b.ln() / a.ln()
    }
    if name == "exp" {
        return a.exp()
    }
    if name == "sqrt" {
        return a.sqrt()
    }
    if name == "cbrt" {
        return a.cbrt()
    }
    if name == "abs" {
        return a.abs()
    }
    if name == "sign" or name == "sgn" {
        return a.sign()
    }
    if name == "floor" {
        return a.floor().to_float()
    }
    if name == "ceil" {
        return a.ceil().to_float()
    }
    if name == "round" {
        return a.round().to_float()
    }
    if name == "trunc" {
        return a.trunc()
    }
    if name == "fract" {
        return a.fract()
    }
    if name == "gamma" {
        return a.gamma()
    }
    if name == "fact" {
        return factorial(a)
    }
    if name == "pow" and second_given {
        return a.pow(b)
    }
    if name == "root" and second_given {
        return nth_root(a, b)
    }
    if name == "mod" and second_given {
        // The remainder mathematics takes: never negative, as `rem_euclid` gives it.
        if b == 0.0 {
            return bad()
        }
        let m = a - (a / b).trunc() * b
        return if m < 0.0 { m + b.abs() } else { m }
    }
    if name == "hypot" and second_given {
        return (a * a + b * b).sqrt()
    }
    if name == "min" and second_given {
        return a.min(b)
    }
    if name == "max" and second_given {
        return a.max(b)
    }
    bad()
}

/// The `degree`-th root of `x`, which has a real answer below zero when the degree is odd.
fn nth_root(degree: Float, x: Float) -> Float {
    if x < 0.0 and degree.fract() == 0.0 and (degree / 2.0).fract() != 0.0 {
        return -(-x).pow(1.0 / degree)
    }
    x.pow(1.0 / degree)
}

/// The factorial of a whole number at or above zero; of any other number, gamma of one more.
fn factorial(n: Float) -> Float {
    if n.is_nan() {
        return n
    }
    if n.fract() != 0.0 {
        return (n + 1.0).gamma()
    }
    if n < 0.0 {
        return bad()
    }
    factorial_up(n, 1.0)
}

fn factorial_up(n: Float, acc: Float) -> Float {
    if n <= 1.0 {
        return acc
    }
    factorial_up(n - 1.0, acc * n)
}

/// The inverse hyperbolic sine: any number.
fn asinh(x: Float) -> Float {
    (x + (x * x + 1.0).sqrt()).ln()
}

/// The inverse hyperbolic cosine: only one and above.
fn acosh(x: Float) -> Float {
    if x < 1.0 {
        return bad()
    }
    (x + (x * x - 1.0).sqrt()).ln()
}

/// The inverse hyperbolic tangent: between -1 and 1, and infinite at either end.
fn atanh(x: Float) -> Float {
    if x == 1.0 {
        return 1.0 / 0.0
    }
    if x == -1.0 {
        return -1.0 / 0.0
    }
    if x.abs() > 1.0 {
        return bad()
    }
    0.5 * ((1.0 + x) / (1.0 - x)).ln()
}

/// The angle of the point `(x, y)` from the positive x axis, in radians: `atan2(y, x)`.
fn atan2(y: Float, x: Float) -> Float {
    let pi = 3.141592653589793
    if x > 0.0 {
        return (y / x).atan()
    }
    if x < 0.0 {
        return if y >= 0.0 { (y / x).atan() + pi } else { (y / x).atan() - pi }
    }
    if y > 0.0 {
        return pi / 2.0
    }
    if y < 0.0 {
        return -pi / 2.0
    }
    0.0
}

/// A number as the display reads it: about ten places, and none past that. A number too large
/// or too small to keep ten places reads as it is, since rounding it would have to overflow.
fn said(v: Float) -> Text {
    if v.is_nan() {
        return ""
    }
    if not v.is_finite() {
        if v > 0.0 {
            return "∞"
        }
        return "-∞"
    }
    let size = v.abs()
    if size >= 1000000000000000.0 or size != 0.0 and size < 1e-9 {
        return v.to_text()
    }
    v.round(10).to_text()
}

// -- the number reader ------------------------------------------------------------------------
fn digits(cs: List<Text>, st: Map<Float>, acc: Float) -> Float {
    let d = digit(peek(cs, st))
    if d < 0.0 {
        return acc
    }
    take(cs, st)
    digits(cs, st, acc * 10.0 + d)
}

fn fractional(cs: List<Text>, st: Map<Float>, scale: Float, acc: Float) -> Float {
    let d = digit(peek(cs, st))
    if d < 0.0 {
        return acc
    }
    take(cs, st)
    fractional(cs, st, scale / 10.0, acc + d * scale)
}

fn number(cs: List<Text>, st: Map<Float>) -> Float {
    let start = index(st)
    let whole = digits(cs, st, 0.0)
    let dot = peek(cs, st) == "."
    if dot { take(cs, st) }
    let fraction = if dot { fractional(cs, st, 0.1, 0.0) } else { 0.0 }
    let value = whole + fraction
    if index(st) == start {
        return bad()
    }
    let marker = peek(cs, st)
    if marker == "e" or marker == "E" {
        let save = index(st)
        take(cs, st)
        let negative = peek(cs, st) == "-"
        if negative or peek(cs, st) == "+" { take(cs, st) }
        if digit(peek(cs, st)) >= 0.0 {
            let sign = if negative { -1.0 } else { 1.0 }
            return value * 10.0.pow(sign * digits(cs, st, 0.0))
        }
        st.put("i", save)
    }
    value
}

// ---------------------------------------------------------------------------
// Unit conversion
// ---------------------------------------------------------------------------
//
// A fixed table, so a conversion works with the network off. Each unit reaches its quantity's
// base unit by a factor and an offset (temperature's units do not share a zero), and comes back
// the same way. Money is not here: a rate is a fact about the moment, and a stale one would lie.
fn symbols(q: Quantity) -> List<Text> {
    if q == .length {
        return ["mm", "cm", "m", "km", "in", "ft", "yd", "mi", "nmi"]
    }
    if q == .mass {
        return ["mg", "g", "kg", "lb", "oz", "t"]
    }
    if q == .volume {
        return ["ml", "l", "tsp", "tbsp", "fl oz", "gal"]
    }
    if q == .temperature {
        return ["°C", "°F", "K"]
    }
    if q == .time {
        return ["s", "min", "h", "d", "wk", "yr"]
    }
    if q == .speed {
        return ["m/s", "km/h", "mph", "kn"]
    }
    if q == .area {
        return ["m²", "ha", "ft²", "ac", "km²"]
    }
    ["B", "kB", "MB", "GB", "KiB", "GiB"]
}

/// The pair a quantity starts at: metres to feet, kilograms to pounds, and so on.
fn default_from(q: Quantity) -> Text {
    symbols(q).at(default_from_at(q)) ?? ""
}

fn default_to(q: Quantity) -> Text {
    symbols(q).at(default_to_at(q)) ?? ""
}

fn default_from_at(q: Quantity) -> Int {
    if q == .length {
        2
    } else if q == .mass { 2 } else if q == .volume { 0 } else if q == .time { 1 } else { 0 }
}

fn default_to_at(q: Quantity) -> Int {
    if q == .length {
        5
    } else if q == .mass {
        3
    } else if q == .volume { 3 } else if q == .time { 0 } else if q == .area { 2 } else { 1 }
}

/// How many base units one of `unit` is; not-a-number where the table has no such unit, so a
/// stale choice reads as no result rather than silently as the wrong number.
fn unit_factor(q: Quantity, unit: Text) -> Float {
    if q == .length {
        return length_factor(unit)
    }
    if q == .mass {
        return mass_factor(unit)
    }
    if q == .volume {
        return volume_factor(unit)
    }
    if q == .temperature {
        return temperature_factor(unit)
    }
    if q == .time {
        return time_factor(unit)
    }
    if q == .speed {
        return speed_factor(unit)
    }
    if q == .area {
        return area_factor(unit)
    }
    data_factor(unit)
}

fn length_factor(u: Text) -> Float {
    if u == "mm" {
        0.001
    } else if u == "cm" {
        0.01
    } else if u == "m" {
        1.0
    } else if u == "km" {
        1000.0
    } else if u == "in" {
        0.0254
    } else if u == "ft" {
        0.3048
    } else if u == "yd" {
        0.9144
    } else if u == "mi" { 1609.344 } else if u == "nmi" { 1852.0 } else { bad() }
}

fn mass_factor(u: Text) -> Float {
    if u == "mg" {
        1e-6
    } else if u == "g" {
        0.001
    } else if u == "kg" {
        1.0
    } else if u == "lb" {
        0.45359237
    } else if u == "oz" { 0.028349523 } else if u == "t" { 1000.0 } else { bad() }
}

fn volume_factor(u: Text) -> Float {
    if u == "ml" {
        0.001
    } else if u == "l" {
        1.0
    } else if u == "tsp" {
        0.004928922
    } else if u == "tbsp" {
        0.014786765
    } else if u == "fl oz" { 0.028413063 } else if u == "gal" { 3.785411784 } else { bad() }
}

fn temperature_factor(u: Text) -> Float {
    if u == "°C" {
        1.0
    } else if u == "°F" { 0.5555555555555556 } else if u == "K" { 1.0 } else { bad() }
}

fn time_factor(u: Text) -> Float {
    if u == "s" {
        1.0
    } else if u == "min" {
        60.0
    } else if u == "h" {
        3600.0
    } else if u == "d" {
        86400.0
    } else if u == "wk" { 604800.0 } else if u == "yr" { 31557600.0 } else { bad() }
}

fn speed_factor(u: Text) -> Float {
    if u == "m/s" {
        1.0
    } else if u == "km/h" {
        0.277777778
    } else if u == "mph" { 0.44704 } else if u == "kn" { 0.514444444 } else { bad() }
}

fn area_factor(u: Text) -> Float {
    if u == "m²" {
        1.0
    } else if u == "ha" {
        10000.0
    } else if u == "ft²" {
        0.09290304
    } else if u == "ac" { 4046.856422 } else if u == "km²" { 1000000.0 } else { bad() }
}

fn data_factor(u: Text) -> Float {
    if u == "B" {
        1.0
    } else if u == "kB" {
        1000.0
    } else if u == "MB" {
        1000000.0
    } else if u == "GB" {
        1000000000.0
    } else if u == "KiB" { 1024.0 } else if u == "GiB" { 1073741824.0 } else { bad() }
}

/// The offset a unit reaches its base with: only temperature has one.
fn unit_offset(q: Quantity, unit: Text) -> Float {
    if q == .temperature {
        if unit == "°C" {
            273.15
        } else if unit == "°F" { 255.3722222222222 } else if unit == "K" { 0.0 } else { bad() }
    } else { 0.0 }
}

fn convert_value(q: Quantity, v: Float, from: Text, to: Text) -> Float {
    if v.is_nan() {
        return bad()
    }
    let base = v * unit_factor(q, from) + unit_offset(q, from)
    (base - unit_offset(q, to)) / unit_factor(q, to)
}

/// The samples a curve is drawn through, 0 to 63.
// ---------------------------------------------------------------------------
// The graphs
// ---------------------------------------------------------------------------
//
// A graph is sampled in the view and drawn as shapes of the design system (`ui.Drawing`): points
// are fractions of the drawing's box, so the same curve scales with the window and the theme.
fn steps() -> List<Float> {
    [
        0.0,
        1.0,
        2.0,
        3.0,
        4.0,
        5.0,
        6.0,
        7.0,
        8.0,
        9.0,
        10.0,
        11.0,
        12.0,
        13.0,
        14.0,
        15.0,
        16.0,
        17.0,
        18.0,
        19.0,
        20.0,
        21.0,
        22.0,
        23.0,
        24.0,
        25.0,
        26.0,
        27.0,
        28.0,
        29.0,
        30.0,
        31.0,
        32.0,
        33.0,
        34.0,
        35.0,
        36.0,
        37.0,
        38.0,
        39.0,
        40.0,
        41.0,
        42.0,
        43.0,
        44.0,
        45.0,
        46.0,
        47.0,
        48.0,
        49.0,
        50.0,
        51.0,
        52.0,
        53.0,
        54.0,
        55.0,
        56.0,
        57.0,
        58.0,
        59.0,
        60.0,
        61.0,
        62.0,
        63.0,
    ]
}

/// The cells a surface is drawn on, 0 to 12.
fn lattice() -> List<Float> {
    [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0]
}

/// The grid a 3D floor is drawn in, 0 to 6.
fn floor_cells() -> List<Float> {
    [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
}

/// The sample's x, and where it stands across the drawing.
fn sample_x(xmin: Float, xmax: Float, i: Int) -> Float {
    xmin + (xmax - xmin) * i.to_float() / 63.0
}

fn across(xmin: Float, xmax: Float, x: Float) -> Float {
    (x - xmin) / (xmax - xmin)
}

fn down(ymin: Float, ymax: Float, y: Float) -> Float {
    (ymax - y) / (ymax - ymin)
}

/// A curve's points, x and y in turn, with the samples off the top or bottom of the view left
/// out so the line is clipped to what a person can see.
fn curve_points(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    xmin: Float,
    xmax: Float,
    ymin: Float,
    ymax: Float,
) -> List<Float> {
    steps().flat_map((v, i) -> curve_point(expr, deg, answer, memory, xmin, xmax, ymin, ymax, i))
}

/// One sample's place in the box, or none where it is off the top or bottom of the view.
fn curve_point(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    xmin: Float,
    xmax: Float,
    ymin: Float,
    ymax: Float,
    i: Int,
) -> List<Float> {
    let x = sample_x(xmin, xmax, i)
    let y = eval_at(expr, deg, answer, memory, x, 0.0)
    if y.is_nan() or y < ymin or y > ymax {
        return []
    }
    [across(xmin, xmax, x), down(ymin, ymax, y)]
}

/// The grid the 2D graph is read against: its lines and the numbers at its corners.
fn grid2d(xmin: Float, xmax: Float, ymin: Float, ymax: Float) -> List<ui.Shape> {
    let verticals = quarters()
        .map(
            v -> ui.Shape.line(x1: v, y1: 0.0, x2: v, y2: 1.0, stroke: .track, weight: .hairline),
        )
    let horizontals = quarters()
        .map(
            v -> ui.Shape.line(x1: 0.0, y1: v, x2: 1.0, y2: v, stroke: .track, weight: .hairline),
        )
    let corners = [
        ui.Shape.text(
            x: 0.005,
            y: 0.985,
            text: said(xmin),
            role: .caption,
            ink: .muted,
            anchor: .start,
        ),
        ui.Shape.text(
            x: 0.995,
            y: 0.985,
            text: said(xmax),
            role: .caption,
            ink: .muted,
            anchor: .end,
        ),
        ui.Shape.text(
            x: 0.005,
            y: 0.03,
            text: said(ymin),
            role: .caption,
            ink: .muted,
            anchor: .start,
        ),
        ui.Shape.text(
            x: 0.005,
            y: 0.06,
            text: said(ymax),
            role: .caption,
            ink: .muted,
            anchor: .start,
        ),
    ]
    [verticals, horizontals, corners].flat_map(s -> s)
}

/// Where the grid's lines stand across the box.
fn quarters() -> List<Float> {
    [0.0, 0.25, 0.5, 0.75, 1.0]
}

/// The 2D graph's shapes: the grid under the curves, the curves, and the sample dots the pointer
/// reads a value at, the one it is nearest lifted.
fn plot2d(
    shown: List<Curve>,
    deg: Bool,
    answer: Float,
    memory: Float,
    xmin: Float,
    xmax: Float,
    ymin: Float,
    ymax: Float,
    traced: Option<Int>,
) -> List<ui.Shape> {
    let grid = grid2d(xmin, xmax, ymin, ymax)
    let curves = shown.map(
        (x, i) -> ui.Shape.path(
            points: curve_points(x.expr, deg, answer, memory, xmin, xmax, ymin, ymax),
            smooth: true,
            stroke: .some(ink_at(i)),
            weight: .strong,
        ),
    )
    let dots = shown.flat_map(
        (x, i) -> if i == 0 {
            dots_for(x.expr, deg, answer, memory, xmin, xmax, ymin, ymax, traced ?? -1)
        } else { [] },
    )
    [grid, curves, dots].flat_map(s -> s)
}

/// The dots on a curve: where each sample stands, the one under the pointer lifted.
fn dots_for(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    xmin: Float,
    xmax: Float,
    ymin: Float,
    ymax: Float,
    picked: Int,
) -> List<ui.Shape> {
    steps()
        .map(
            (v, i) -> ui.Shape.ellipse(
                cx: sample_x(xmin, xmax, i),
                cy: dot_y(expr, deg, answer, memory, sample_x(xmin, xmax, i), ymin, ymax),
                rx: if i == picked { 0.014 } else { 0.007 },
                ry: if i == picked { 0.014 } else { 0.007 },
                stroke: .some(.accent),
                weight: .hairline,
            ),
        )
}

/// A dot's height, or below the drawing where the sample is no number: nothing is drawn there,
/// and the dot keeps its place in the list so the pointer's index still means its sample.
fn dot_y(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    x: Float,
    ymin: Float,
    ymax: Float,
) -> Float {
    let y = eval_at(expr, deg, answer, memory, x, 0.0)
    if y.is_nan() {
        return 2.0
    }
    down(ymin, ymax, y)
}

/// The curve's ink, by its place in the list.
fn ink_at(i: Int) -> ui.Ink {
    if i % 5 == 0 {
        .accent
    } else if i % 5 == 1 {
        .info
    } else if i % 5 == 2 { .success } else if i % 5 == 3 { .warning } else { .danger }
}

/// What the pointer stands at, in words: the first visible curve's value at that sample.
fn trace_said(
    shown: List<Curve>,
    deg: Bool,
    answer: Float,
    memory: Float,
    xmin: Float,
    xmax: Float,
    at: Int,
) -> Text {
    if let first = shown.first() {
        if at >= 0 and at < 64 {
            let x = sample_x(xmin, xmax, at)
            let y = eval_at(first.expr, deg, answer, memory, x, 0.0)
            return "x = {said(x)}, y = {said(y)}"
        }
    }
    ""
}

/// The 3D graph's shapes: the floor grid under every visible surface, each drawn as filled cells
/// shaded by the angle each turns to the light.
fn plot3d(
    shown: List<Surface>,
    starter: Bool,
    deg: Bool,
    answer: Float,
    memory: Float,
    yaw: Float,
    pitch: Float,
    zoom: Float,
) -> List<ui.Shape> {
    let floor = lines3d(yaw, pitch, zoom)
    let plots = if starter {
        surface_faces(starter_surface(), deg, answer, memory, yaw, pitch, zoom, .accent)
    } else {
        shown.flat_map(
            (x, i) -> surface_faces(x.expr, deg, answer, memory, yaw, pitch, zoom, ink_at(i)),
        )
    }
    [floor, plots].flat_map(s -> s)
}

/// The surface a 3D graph opens with, before anyone adds one.
fn starter_surface() -> Text {
    "sin(sqrt(x^2 + y^2))"
}

/// How many cells across a surface is drawn: fine enough that its folds read as a body rather
/// than as a net of lines.
/// One surface as bands that run along its rows, each filled in `ink` and lit by the angle it
/// turns to the light, the ones farthest from the eye first so a nearer fold is drawn over the
/// one behind it. A band is one shape: a cell per band would be a thousand values a frame, more
/// than a view may hand the window between two draws.
fn surface_faces(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    yaw: Float,
    pitch: Float,
    zoom: Float,
    ink: ui.Ink,
) -> List<ui.Shape> {
    // A height field read from outside: along each axis its bands go away from the eye in the
    // order its turned coordinate grows, so painting them in that order stacks them right.
    let far_y = yaw.cos() < 0.0
    let far_x = yaw.sin() < 0.0
    surface_lattice()
        .map(
            (row, j) -> surface_band(
                expr,
                deg,
                answer,
                memory,
                yaw,
                pitch,
                zoom,
                ink,
                if far_y { 10.0 - row } else { row },
                far_x,
            ),
        )
}

/// The steps across a surface, one per band's edge: where a corner stands along one axis.
fn surface_lattice() -> List<Float> {
    [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]
}

/// One band of a surface: the strip between two rows, filled in the surface's ink at the light
/// its face turns to. A corner that is no number leaves the band out.
fn surface_band(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    yaw: Float,
    pitch: Float,
    zoom: Float,
    ink: ui.Ink,
    row: Float,
    far_x: Bool,
) -> ui.Shape {
    let step = 14.0 / 10.0
    let y = -7.0 + step * row
    // The near edge runs across, the far edge comes back, so the strip is one closed ring.
    // The near edge runs across, the far edge comes back, so the strip is one closed ring.
    let near = surface_lattice()
        .flat_map(
            (column, i) -> band_corner(
                expr,
                deg,
                answer,
                memory,
                yaw,
                pitch,
                zoom,
                y,
                step,
                if far_x { 10.0 - column } else { column },
                0.0,
            ),
        )
    let far = surface_lattice()
        .flat_map(
            (column, i) -> band_corner(
                expr,
                deg,
                answer,
                memory,
                yaw,
                pitch,
                zoom,
                y,
                step,
                if far_x { column } else { 10.0 - column },
                1.0,
            ),
        )
    let outline = [near, far].flat_map(s -> s)
    ui.Shape.path(
        points: outline,
        closed: true,
        fill: .some(ink),
        shade: .some(band_light(expr, deg, answer, memory, y, step)),
    )
}

/// One corner of a band, at `along` across it and `at_the_back` of it, as the drawing shows it.
fn band_corner(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    yaw: Float,
    pitch: Float,
    zoom: Float,
    y: Float,
    step: Float,
    along: Float,
    across: Float,
) -> List<Float> {
    let cx = -7.0 + step * along
    let cy = y + step * across
    let z = eval_at(expr, deg, answer, memory, cx, cy)
    if z.is_nan() {
        return []
    }
    projected(cx, cy, z, yaw, pitch, zoom)
}

/// How much light reaches the band between the rows at `y`, `step` apart: its face's tilt
/// against a light above and to one side, never so dark a fold disappears.
fn band_light(expr: Text, deg: Bool, answer: Float, memory: Float, y: Float, step: Float) -> Float {
    let h = 0.35
    let x = 0.0
    let middle = y + step / 2.0
    let dx = (eval_at(expr, deg, answer, memory, x + h, middle) - eval_at(expr, deg, answer, memory, x - h, middle)) / (2.0 * h)
    let dy = (eval_at(expr, deg, answer, memory, x, middle + h) - eval_at(expr, deg, answer, memory, x, middle - h)) / (2.0 * h)
    if dx.is_nan() or dy.is_nan() {
        return 1.0
    }
    // The face's normal (-dx, -dy, 1), against a light from above, ahead and one side.
    let reach = (dx * dx + dy * dy + 1.0).sqrt()
    let lit = (-0.42 * dx - 0.5 * dy + 0.76) / reach
    (0.45 + 0.55 * lit.max(0.0)).clamp(0.35, 1.0)
}

/// One point of a surface, turned by the orbit and tilted, as a place in the drawing.
fn project_point(
    expr: Text,
    deg: Bool,
    answer: Float,
    memory: Float,
    yaw: Float,
    pitch: Float,
    zoom: Float,
    i: Int,
    j: Int,
) -> List<Float> {
    let x = -7.0 + 14.0 * i.to_float() / 12.0
    let y = -7.0 + 14.0 * j.to_float() / 12.0
    let z = eval_at(expr, deg, answer, memory, x, y)
    if z.is_nan() {
        return []
    }
    projected(x, y, z, yaw, pitch, zoom)
}

/// A world point as the drawing's fractions: turned about the vertical, then tilted toward the
/// person, its height scaled so a surface has some to show.
fn projected(x: Float, y: Float, z: Float, yaw: Float, pitch: Float, zoom: Float) -> List<Float> {
    let turned_x = x * yaw.cos() - y * yaw.sin()
    let turned_y = x * yaw.sin() + y * yaw.cos()
    let rise = turned_y * pitch.sin() + z * pitch.cos() * 3.0
    [0.5 + turned_x / 20.0 * zoom, 0.58 - rise / 20.0 * zoom]
}

/// The floor a surface is read against: a grid at zero height.
fn lines3d(yaw: Float, pitch: Float, zoom: Float) -> List<ui.Shape> {
    let one_way = floor_cells().map((v, i) -> floor_line(yaw, pitch, zoom, i, 0))
    let the_other = floor_cells().map((v, i) -> floor_line(yaw, pitch, zoom, 6, i))
    [one_way, the_other].flat_map(s -> s)
}

fn floor_line(yaw: Float, pitch: Float, zoom: Float, along: Int, at: Int) -> ui.Shape {
    let x1 = -7.0 + 14.0 * along.to_float() / 6.0
    let y1 = -7.0 + 14.0 * at.to_float() / 6.0
    let x2 = -7.0 + 14.0 * at.to_float() / 6.0
    let y2 = -7.0 + 14.0 * along.to_float() / 6.0
    let start = projected(x1, y1, 0.0, yaw, pitch, zoom)
    let end = projected(x2, y2, 0.0, yaw, pitch, zoom)
    ui.Shape.line(
        x1: start.at(0) ?? 0.0,
        y1: start.at(1) ?? 0.0,
        x2: end.at(0) ?? 0.0,
        y2: end.at(1) ?? 0.0,
        stroke: .track,
        weight: .hairline,
    )
}

/// A team working on a release, with a few sums already worked out.
scenario "a working morning" {
    calculator.calculations.insert(Calculation(expr: "12*(4+3)", result: "84"))
    calculator.calculations.insert(Calculation(expr: "sqrt(2)", result: "1.4142135624"))
    calculator.calculations.insert(Calculation(expr: "5!", result: "120"))
    calculator.calculations.insert(Calculation(expr: "2^10", result: "1024"))
}
