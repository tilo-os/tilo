use jiff::civil::{Date, DateTime, Time, date, time};
use tilo_resource::{Invoke, Model, Replica, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Diary {
    pub day: Option<Date>,
    pub at: Option<DateTime>,
    pub alarm: Option<Time>,
    pub days: Vec<Date>,
}

#[resource(kind = "test.diary", params = diary, catalogue = "tests/locales")]
impl Diary {
    /// Plan a moment
    #[action]
    pub fn plan(&mut self, #[param("When")] at: DateTime, #[param("On")] day: Date) {
        self.at = Some(at);
        self.day = Some(day);
    }
}

tilo_test_env::scenario! {
    fn dates_and_times_are_whole_values_written_as_text;

    phase dates_and_times_are_set_whole;
    // A date, a time and a moment are stored as their text, set whole and
    // merged last writer wins.
    {
        let mut diary = Replica::<Diary>::new();
        let day = date(2026, 10, 9);
        let at = day.at(9, 30, 0, 0);
        let alarm = time(7, 0, 0, 0);
        let paths = Diary::paths();
        for edit in [
            paths.day.set(Some(day)),
            paths.at.set(Some(at)),
            paths.alarm.set(Some(alarm)),
            paths.days.set(vec![day, day.tomorrow().unwrap()]),
        ] {
            diary.apply(Invoke::edit(&edit).unwrap()).unwrap();
        }
        let state = diary.state().unwrap();
        assert_eq!(state.day, Some(day));
        assert_eq!(state.at, Some(at));
        assert_eq!(state.alarm, Some(alarm));
        assert_eq!(state.days.len(), 2);
        assert_eq!(
            Model::to_json(&at),
            serde_json::json!("2026-10-09T09:30:00")
        );
        assert!(Date::from_json(&serde_json::json!("not a date")).is_err());
    }

    phase a_verb_takes_dates_and_times_as_their_text;
    // A verb takes a date and a moment as their text, as its schema says.
    {
        let mut diary = Replica::<Diary>::new();
        let day = date(2026, 10, 9);
        let at = day.at(9, 30, 0, 0);
        let plan = Diary::PLAN.with(diary::Plan { at, day });
        assert_eq!(plan.arguments["at"], "2026-10-09T09:30:00");
        assert_eq!(plan.arguments["day"], "2026-10-09");
        diary.apply(plan).unwrap();
        let state = diary.state().unwrap();
        assert_eq!((state.at, state.day), (Some(at), Some(day)));
    }
}
