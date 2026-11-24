/// The structural operator graph: the operators a person placed, the typed ports they offer, and
/// the connections that hold. A person picks an output and then an input; the device refuses a
/// connection whose types differ, whose input another connection holds, or that would close a
/// cycle, and says why.
view graph_body(p: Project) {
    // What the dragging pointer has done since it went down, so the canvas pans with it.
    state held_x: Float = 0.0
    state held_y: Float = 0.0
    state from_x: Float = 0.0
    state from_y: Float = 0.0
    state pan_x: Float = 0.0
    state pan_y: Float = 0.0
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            ui.Dropdown(options: kind_names(), placeholder: "Add operator", caption: "Operator") {
                on change(label): add_operator(p, label)
            }
            ui.Button("Zoom out", icon: .minus, icon_only: true) {
                on press: zoom_graph(p, "out")
            }
            ui.Text(
                if p.zoom == 100 { "Fit to window" } else { "{p.zoom}%" },
                role: .caption,
                numeric: true,
            )
            ui.Button("Zoom in", icon: .plus, icon_only: true) {
                on press: zoom_graph(p, "in")
            }
            ui.Button("Fit to window", icon: .expand) {
                on press: set(pan_x, 0.0).then(set(pan_y, 0.0)).then(zoom_graph(p, "fit"))
            }
        }
        ui.Text(
            "Structural design only. This graph does not run or qualify hardware.",
            role: .caption,
            secondary: true,
        )
        if p.output_node != "" {
            row(gap: .related, align: .center) {
                ui.Text(connecting_line(p), role: .caption)
                ui.Button("Cancel", icon: .x) {
                    on press: clear_output(p)
                }
            }
        } else if not p.operators.is_empty() {
            ui.Text(
                "To connect two operators, choose an output, then an input it can feed.",
                role: .caption,
                secondary: true,
            )
        }
        for connection in p.connections.filter(c -> not c.active) {
            ui.Text(conflict_line(connection), role: .caption)
        }
        if p.operators.is_empty() {
            ui.EmptyState(
                "No operators yet",
                message: "Add one to start designing.",
                icon: .git_branch,
                inline: true,
            )
        } else {
            ui.Drawing(
                graph_shapes(p, pan_x, pan_y),
                meaning: .decoration,
                label: .some("The structural operator design"),
                height: .some(560.0),
                motion: .calm,
            ) {
                on press(i): tap_graph(p, i)
                on drag_start(x, y): set(held_x, x)
                    .then(set(held_y, y))
                    .then(set(from_x, pan_x))
                    .then(set(from_y, pan_y))
                on drag(x, y): set(pan_x, from_x - (x - held_x))
                    .then(set(pan_y, from_y - (y - held_y)))
                on wheel(dx, dy): set(pan_x, pan_x - dx / 1000.0)
                    .then(set(pan_y, pan_y - dy / 1000.0))
            }
        }
    }
}

/// The line saying what a person is connecting from.
fn connecting_line(p: Project) -> Text {
    let from = if let op = p.operators.find(o -> o.node == p.output_node) {
        kind_words(op.kind)
    } else { p.output_node }
    "Connecting from the {port_words(p.output_port)} output of {from}"
}

/// One connection the design no longer holds, and why.
fn conflict_line(c: Connection) -> Text {
    "{c.from_node}.{port_words(c.from_port)} cannot feed {c.to_node}.{port_words(c.to_port)}: {refusal_words(c.reason)}"
}

// ---------------------------------------------------------------------------
// The canvas: the shapes, and what each of them is
// ---------------------------------------------------------------------------
fn node_width() -> Float {
    260.0
}

fn node_height() -> Float {
    120.0
}

fn world_width(p: Project) -> Float {
    (p.operators.map(op -> op.x.to_float() + node_width()).sort_by(reach -> reach, desc).first()
        ?? 400.0)
        .max(400.0)
}

fn world_height(p: Project) -> Float {
    (p.operators.map(op -> op.y.to_float() + node_height()).sort_by(reach -> reach, desc).first()
        ?? 300.0)
        .max(300.0)
}

/// How much of the canvas one design unit takes, fit to the box and then zoomed.
fn graph_scale(p: Project) -> Float {
    (1.0 / world_width(p)).min(1.0 / world_height(p)) * (p.zoom.to_float() / 100.0)
}

fn offset_x(p: Project, pan_x: Float) -> Float {
    (1.0 - world_width(p) * graph_scale(p)) / 2.0 + pan_x
}

fn offset_y(p: Project, pan_y: Float) -> Float {
    (1.0 - world_height(p) * graph_scale(p)) / 2.0 + pan_y
}

fn box_x(p: Project, op: Operator, pan_x: Float) -> Float {
    offset_x(p, pan_x) + op.x.to_float() * graph_scale(p)
}

fn box_y(p: Project, op: Operator, pan_y: Float) -> Float {
    offset_y(p, pan_y) + op.y.to_float() * graph_scale(p)
}

/// One port's place down an operator's side: the inputs and outputs share out the height.
fn port_y(p: Project, op: Operator, index: Int, count: Int, pan_y: Float) -> Float {
    let across = (index.to_float() + 1.0) / (count.to_float() + 1.0)
    box_y(p, op, pan_y) + node_height() * graph_scale(p) * across
}

/// The type the chosen output moves, and empty when no output is chosen.
fn output_type(p: Project) -> Text {
    if let op = p.operators.find(o -> o.node == p.output_node) {
        if let port = op.outputs.find(out -> out.key == p.output_port) {
            port.data_type
        } else { "" }
    } else { "" }
}

/// The shapes of the graph: the connections under their operators, then each operator with its
/// title and its ports. The order is `graph_targets`'s, shape for shape.
fn graph_shapes(p: Project, pan_x: Float, pan_y: Float) -> List<ui.Shape> {
    let edges = p.connections.filter(c -> c.active).map(c -> connection_line(p, c, pan_x, pan_y))
    let nodes = p.operators
        .sort_by(op -> op.node)
        .flat_map(op -> operator_shapes(p, op, pan_x, pan_y))
    edges + nodes
}

/// What each of `graph_shapes`' shapes is: empty for a connection, then a node or one of its
/// ports, as `node:<id>`, `in:<node>:<port>` or `out:<node>:<port>`.
fn graph_targets(p: Project) -> List<Text> {
    let edges = p.connections.filter(c -> c.active).map(c -> "")
    let nodes = p.operators
        .sort_by(op -> op.node)
        .flat_map(
            op -> ["node:{op.node}", "node:{op.node}"]
                + op.inputs.flat_map(
                    port -> ["in:{op.node}:{port.key}", "in:{op.node}:{port.key}"],
                )
                + op.outputs.flat_map(
                    port -> ["out:{op.node}:{port.key}", "out:{op.node}:{port.key}"],
                ),
        )
    edges + nodes
}

fn connection_line(p: Project, c: Connection, pan_x: Float, pan_y: Float) -> ui.Shape {
    let from = p.operators.find(op -> op.node == c.from_node)
    let to = p.operators.find(op -> op.node == c.to_node)
    let from_index = if let op = from {
        if let port = op.outputs.find(found -> found.key == c.from_port) {
            op.outputs.index_of(port) ?? 0
        } else { 0 }
    } else { 0 }
    let to_index = if let op = to {
        if let port = op.inputs.find(found -> found.key == c.to_port) {
            op.inputs.index_of(port) ?? 0
        } else { 0 }
    } else { 0 }
    let x1 = if let op = from {
        box_x(p, op, pan_x) + node_width() * graph_scale(p)
    } else { 0.0 }
    let y1 = if let op = from {
        port_y(p, op, from_index, op.outputs.len(), pan_y)
    } else { 0.0 }
    let x2 = if let op = to { box_x(p, op, pan_x) } else { 0.0 }
    let y2 = if let op = to {
        port_y(p, op, to_index, op.inputs.len(), pan_y)
    } else { 0.0 }
    ui.Shape.line(x1: x1, y1: y1, x2: x2, y2: y2, stroke: .muted, weight: .regular)
}

fn operator_shapes(p: Project, op: Operator, pan_x: Float, pan_y: Float) -> List<ui.Shape> {
    let x = box_x(p, op, pan_x)
    let y = box_y(p, op, pan_y)
    let w = node_width() * graph_scale(p)
    let h = node_height() * graph_scale(p)
    let chosen = p.node == op.node
    let box = [
        ui.Shape.rect(
            x: x,
            y: y,
            w: w,
            h: h,
            round: 0.02,
            stroke: .some(if chosen { .accent } else { .muted }),
            weight: if chosen { .strong } else { .regular },
        ),
        ui.Shape.text(
            x: x + 0.012,
            y: y + 0.03,
            text: kind_words(op.kind),
            role: .label,
            ink: .ink,
            anchor: .start,
        ),
        ui.Shape.text(
            x: x + 0.012,
            y: y + 0.075,
            text: "{op.node} · {shape_said(op.shape)}",
            role: .caption,
            ink: .muted,
            anchor: .start,
        ),
    ]
    let compatible = output_type(p)
    let inputs = op.inputs.flat_map(
        port -> [
            ui.Shape.ellipse(
                cx: x,
                cy: port_y(p, op, op.inputs.index_of(port) ?? 0, op.inputs.len(), pan_y),
                rx: 0.008,
                ry: 0.014,
                fill: .some(
                    if compatible != "" and compatible == port.data_type {
                        .success
                    } else { .track },
                ),
                stroke: .some(.muted),
                weight: .regular,
            ),
            ui.Shape.text(
                x: x + 0.014,
                y: port_y(p, op, op.inputs.index_of(port) ?? 0, op.inputs.len(), pan_y) + 0.022,
                text: port_words(port.key),
                role: .caption,
                ink: .muted,
                anchor: .start,
            ),
        ],
    )
    let chosen_port = p.output_node == op.node
    let outputs = op.outputs.flat_map(
        port -> [
            ui.Shape.ellipse(
                cx: x + w,
                cy: port_y(p, op, op.outputs.index_of(port) ?? 0, op.outputs.len(), pan_y),
                rx: 0.008,
                ry: 0.014,
                fill: .some(
                    if chosen_port and p.output_port == port.key { .accent } else { .track },
                ),
                stroke: .some(.muted),
                weight: .regular,
            ),
            ui.Shape.text(
                x: x + w - 0.014,
                y: port_y(p, op, op.outputs.index_of(port) ?? 0, op.outputs.len(), pan_y) + 0.022,
                text: port_words(port.key),
                role: .caption,
                ink: .muted,
                anchor: .end,
            ),
        ],
    )
    box + inputs + outputs
}

/// A press on the canvas: an operator's title opens its facts, an output is what the next input
/// connects from, and an input connects or opens its operator.
// ---------------------------------------------------------------------------
// Choosing what a press on the canvas means
// ---------------------------------------------------------------------------
action tap_graph(p: Project, index: Int) {
    let targets = graph_targets(p)
    if index < 0 or index >= targets.len() {
        return
    }
    let parts = (targets.at(index) ?? "").split(":")
    let kind = parts.at(0) ?? ""
    if kind == "node" {
        if let op = p.operators.find(o -> o.node == (parts.at(1) ?? "")) { inspect_operator(p, op) }
    } else if kind == "out" {
        if let op = p.operators.find(o -> o.node == (parts.at(1) ?? "")) {
            select_output(p, op, parts.at(2) ?? "")
        }
    } else if kind == "in" {
        let node = parts.at(1) ?? ""
        let port = parts.at(2) ?? ""
        if p.output_node != "" {
            if let from = p.operators.find(o -> o.node == p.output_node) {
                if let to = p.operators.find(o -> o.node == node) {
                    connect(p, from, p.output_port, to, port)
                }
            }
        } else if let to = p.operators.find(o -> o.node == node) { inspect_operator(p, to) }
    }
}
