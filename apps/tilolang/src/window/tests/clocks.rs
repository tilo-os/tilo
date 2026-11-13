//! Times and dates an app writes in its own sentences: they read in the clock and the date
//! style the person chose in Settings, as the window's own times do.

/// The clock an app writes reads as the clock its window shows: the person's, from Settings.
#[test]
fn a_time_the_app_writes_reads_in_the_clock_the_person_chose() {
    use tilolang_tilo::instance::Platform as _;

    let before = ui::format::region::current();
    let chosen = |time| ui::format::region::RegionFormats { time, ..before };
    ui::format::region::install(chosen(ui::clock::TimeStyle::H12));
    assert_eq!(
        super::InProcess.clock(10, 41),
        "10:41 AM",
        "a 12-hour clock reads as the window's own times do"
    );
    ui::format::region::install(chosen(ui::clock::TimeStyle::H24));
    assert_eq!(super::InProcess.clock(10, 41), "10:41");
    ui::format::region::install(before);
}
