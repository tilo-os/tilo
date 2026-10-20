app so.tilo.counter "Counter"

/// A number everyone can count up together.
type Counter { value: Int merge counter }

/// Count one up.
action increment(c: Counter) { c.value += 1 }

view main(c: Counter) {
  column {
    text(c.value)
    button("+1", on: increment(c)) #plus
  }
}
