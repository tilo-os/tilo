app so.tilo.counter "Counter"

/// A counter that several people can bump at once.
type Counter {
    /// The current value.
    value: Int merge counter
}

root Counter

default view counter for Counter

/// Add one to the counter.
action increment(c: Counter) {
    c.value += 1
}

/// Take one away from the counter.
action decrement(c: Counter) {
    c.value -= 1
}

/// The counter's window.
view counter(c: Counter) {
    page {
        stack(gap: .section, align: .center) {
            text("{c.value}", style: .display)
            controls(c)
        }
    }
}

view controls(c: Counter) {
    row(gap: .related) {
        button("-1", on: decrement(c), size: .large) as minus
        button("+1", on: increment(c), style: .primary, size: .large) as plus
    }
}
