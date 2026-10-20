app so.tilo.counter "Counter"

/// A counter that several people can bump at once.
type Counter {
    /// The current value.
    value: Int merge counter
}

/// Add one to the counter.
action increment(c: Counter) {
    c.value += 1
}

view counter(c: Counter) {
    column {
        text(c.value)
        button("+1", on: increment(c)) as plus
    }
}
