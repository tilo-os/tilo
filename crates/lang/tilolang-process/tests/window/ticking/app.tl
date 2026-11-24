app so.tilo.lang.ticking "Ticking" (icon: .clock, category: .utility)

/// One tick of the clock, by whoever's window made it.
type Tick {
    by: Actor
}

/// A clock the people at the workspace's windows share.
type Clock {
    ticks: Collection<Tick>
}

root Clock

default view clock for Clock

/// The clock's window: how many ticks it took, and a Start that makes this window tick it once a
/// second, for as long as it shows the view.
view clock(c: Clock) {
    state running: Bool = false
    std.page(c) {
        ui.Text("{c.ticks.len()} ticks")
        if running {
            every 1s: tick(c)
        } else {
            ui.Button("Start") {
                on press: set(running, true)
            }
        }
    }
}

/// One tick, by whoever's window made it.
action tick(c: Clock) {
    c.ticks.insert(Tick(by: invoker))
}
