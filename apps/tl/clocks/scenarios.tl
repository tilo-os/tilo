/// Scenarios: the app as people meet it, staged for the gallery, `tilo-layout-check` and the
/// tests. They are seed data for shots and tests, never a person's: a real first launch starts
/// empty.
///
/// Three cities compared, two alarms set and a tea on, with the stopwatch running: a morning the
/// workspace keeps, on every device.
scenario "a morning of alarms" -> Clocks {
    add_world_clock(clocks, "Tokyo", "Asia/Tokyo")
    add_world_clock(clocks, "London", "Europe/London")
    add_world_clock(clocks, "New York", "America/New_York")
    add_alarm(clocks, "Wake up", 7, 0, [.mon, .tue, .wed, .thu, .fri])
    add_alarm(clocks, "Gym", 18, 30)
    add_timer(clocks, "Tea", 300)
    add_timer(clocks, "Focus", 1500)
    start_stopwatch(clocks)
    clocks
}
