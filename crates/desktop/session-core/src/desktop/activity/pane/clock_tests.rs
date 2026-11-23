//! The pane's two readings of a time it is handed: whether a device was away long enough for a
//! recap of what changed, and how far back a period reaches. Each is a function of the time passed
//! in and nothing else, so each is a table of the times that decide it.

use super::*;

tilo_test_env::scenario! {
    fn the_panes_readings_of_a_time_keep_their_boundaries;

    phase away_recap_requires_the_full_threshold_and_fails_closed_on_clock_rollback;
    {
        const AFTER: u64 = 45 * 60;
        // When the device left, when it came back, whether a recap is due, and the case.
        let cases: [(u64, u64, bool, &str); 8] = [
            (10_000, 10_000 + AFTER - 1, false, "a second short"),
            (10_000, 10_000 + AFTER, true, "the threshold to the second"),
            (10_000, 10_000 - 1, false, "a clock that rolled back"),
            (10_000, 10_000, false, "back the second it left"),
            (10_000, 10_000 + AFTER + 1, true, "a second past"),
            (0, AFTER - 1, false, "left at the epoch, a second short"),
            (0, AFTER, true, "left at the epoch, to the second"),
            (0, 0, false, "left and back at the epoch"),
        ];
        for (departed, back, due, case) in cases {
            assert_eq!(
                away_recap_is_due(departed, SystemTime::UNIX_EPOCH + Duration::from_secs(back)),
                due,
                "{case}: left at {departed}, back at {back}"
            );
        }
    }

    phase period_windows_are_half_open_and_all_time_is_unbounded;
    {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(Period::AllTime.range(now).is_none());
        for (period, name, back) in [
            (Period::Week, "week", 604_800),
            (Period::Today, "today", 86_400),
            (Period::Month, "month", 2_592_000),
        ] {
            let (start, end) = period.range(now).unwrap();
            assert_eq!(
                now.duration_since(start).unwrap(),
                Duration::from_secs(back),
                "the {name} window reaches back its own length"
            );
            assert!(
                end > now,
                "{name}: a change committed this instant is inside the window"
            );
        }
    }
}
