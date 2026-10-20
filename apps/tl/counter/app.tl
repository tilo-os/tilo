app so.tilo.counter "Counter" (icon: .hash)

/// A counter that several people can bump at once.
type Counter {
    /// The current value.
    value: Int merge counter
}

root Counter

default view counter for Counter

/// Add one to the counter.
primary action increment(c: Counter) (icon: .plus) {
    c.value += 1
}

/// Take one away from the counter.
action decrement(c: Counter) (icon: .minus) {
    c.value -= 1
}

/// The counter's window: the value, and the two buttons that change it.
view counter(c: Counter) {
    page {
        section("Count") {
            text("{c.value}", role: .numeric)
        }
        controls(c)
    }
}

view controls(c: Counter) {
    row(gap: .related) {
        button(decrement(c)) as minus
        button(increment(c)) as plus
    }
}

/// A counter of visitors after a busy morning.
scenario "a busy morning" {
    let c = root Counter
    c.value += 42
}
