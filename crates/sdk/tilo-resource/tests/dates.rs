use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use tilo_resource::{Invoke, Model, Replica, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Diary {
    pub day: Option<NaiveDate>,
    pub at: Option<NaiveDateTime>,
    pub alarm: Option<NaiveTime>,
    pub days: Vec<NaiveDate>,
}

#[resource(kind = "test.diary", label = "Diary", params = diary)]
impl Diary {
    /// Plan a moment
    #[action]
    pub fn plan(&mut self, #[param("When")] at: NaiveDateTime, #[param("On")] day: NaiveDate) {
        self.at = Some(at);
        self.day = Some(day);
    }
}

/// A date, a time and a moment are stored as their text, set whole and
/// merged last writer wins.
#[test]
fn dates_and_times_are_set_whole() {
    let mut diary = Replica::<Diary>::new();
    let day = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
    let at = day.and_hms_opt(9, 30, 0).unwrap();
    let alarm = NaiveTime::from_hms_opt(7, 0, 0).unwrap();
    let paths = Diary::paths();
    for edit in [
        paths.day.set(Some(day)),
        paths.at.set(Some(at)),
        paths.alarm.set(Some(alarm)),
        paths.days.set(vec![day, day.succ_opt().unwrap()]),
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
    assert!(NaiveDate::from_json(&serde_json::json!("not a date")).is_err());
}

/// A verb takes a date and a moment as their text, as its schema says.
#[test]
fn a_verb_takes_dates_and_times_as_their_text() {
    let mut diary = Replica::<Diary>::new();
    let day = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
    let at = day.and_hms_opt(9, 30, 0).unwrap();
    let plan = Diary::PLAN.with(diary::Plan { at, day });
    assert_eq!(plan.arguments["at"], "2026-10-09T09:30:00");
    assert_eq!(plan.arguments["day"], "2026-10-09");
    diary.apply(plan).unwrap();
    let state = diary.state().unwrap();
    assert_eq!((state.at, state.day), (Some(at), Some(day)));
}
