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

/// Take one away from the counter.
action decrement(c: Counter) {
    c.value -= 1
}

view main(c: Counter) {
    page {
        stack(gap: section, align: center) {
            text("{c.value}", style: "display")
            controls(c)
        }
    }
}

view controls(c: Counter) {
    row(gap: related) {
        button("-1", style: "large", on: decrement(c)) #minus
        button("+1", style: "primary large", on: increment(c)) #plus
    }
}
