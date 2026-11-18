//! Date helpers for birthdays, over the calendar arithmetic in `ui::date`.

pub fn days_in_month(year: i32, month: u32) -> u32 {
    ui::date::days_in_month(i64::from(year), month)
}
