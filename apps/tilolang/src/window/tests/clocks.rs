//! Times and dates an app writes in its own sentences: they read in the clock and the date
//! style the person chose in Settings, as the window's own times do.

/// The clock an app writes reads as the clock its window shows: the person's, from Settings.
#[test]
fn a_time_the_app_writes_reads_in_the_clock_the_person_chose() {
    use tilolang_tilo::instance::Platform as _;

    let before = ui::format::region::current();
    let chosen = |time| ui::format::region::RegionFormats { time, ..before };
    // An hour and a minute, on a 12-hour clock and on a 24-hour one: midnight and noon are
    // where the two clocks differ most, and the minutes before each where the half-day turns.
    let rows = [
        (10, 41, "10:41 AM", "10:41"),
        (0, 0, "12:00 AM", "00:00"),
        (0, 7, "12:07 AM", "00:07"),
        (11, 59, "11:59 AM", "11:59"),
        (12, 0, "12:00 PM", "12:00"),
        (13, 5, "1:05 PM", "13:05"),
        (23, 59, "11:59 PM", "23:59"),
    ];
    ui::format::region::install(chosen(ui::clock::TimeStyle::H12));
    for (hour, minute, half_day, _) in rows {
        assert_eq!(
            super::InProcess.clock(hour, minute),
            half_day,
            "{hour}:{minute:02}: a 12-hour clock reads as the window's own times do"
        );
    }
    ui::format::region::install(chosen(ui::clock::TimeStyle::H24));
    for (hour, minute, _, whole_day) in rows {
        assert_eq!(
            super::InProcess.clock(hour, minute),
            whole_day,
            "{hour}:{minute:02} on a 24-hour clock"
        );
    }
    ui::format::region::install(before);
}
