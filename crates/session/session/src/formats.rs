//! The person's region formats, followed from their settings: the first day of their week, their
//! date and clock styles, which Settings' Region & Formats pane writes to the `locale` section.
//!
//! The session's own date and clock formatters read them through [`ui::format::region`], which
//! is read once and kept; a change is read again here, and every tilolang host running in this
//! session's apps is told on its door ([`crate::apps::Apps::formats_changed`]), so the instances
//! write what their windows show again in the formats the person chose. Their windows' dates,
//! clocks and "this week" read the same day their desk's calendar starts a week on.

#[cfg(target_os = "linux")]
use crate::Session;

/// The person's formats, followed from their settings.
#[cfg(target_os = "linux")]
pub(crate) struct Formats {
    /// The settings version the formats were last followed at, so the document is read again only
    /// when it changed.
    seen: std::cell::Cell<Option<settings_store::SettingsStamp>>,
}

#[cfg(target_os = "linux")]
impl Formats {
    pub(crate) fn new() -> Self {
        Self {
            seen: std::cell::Cell::new(None),
        }
    }

    /// Follow the person's formats when their settings changed since the last pass. Returns
    /// whether they are another set of formats now: the caller tells whoever shows them.
    pub(crate) fn follow(&self) -> bool {
        let stamp = settings_store::stamp();
        if !settings_store::document_changed(self.seen.get().is_some(), self.seen.get(), stamp) {
            return false;
        }
        self.seen.set(stamp);
        ui::format::region::refresh()
    }
}

#[cfg(target_os = "linux")]
impl Session {
    /// Read the person's formats again where they changed, and tell the hosts running their apps:
    /// the session's own surface re-renders its dates in them, and every tilolang instance writes
    /// its dates, clocks and weeks again.
    pub(crate) fn follow_formats(&self) {
        if !self.formats.follow() {
            return;
        }
        if let Some(apps) = self.shared.launch.clone() {
            apps.formats_changed();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use settings_store::SettingsSource as _;
    use std::sync::Arc;

    /// A chosen region reaches the session's formatters on the change, once, and again on the
    /// next change: a pass over unchanged settings reads nothing.
    #[test]
    fn a_region_the_person_changes_is_followed_once_per_change() {
        let before = ui::format::region::current();
        let document = Arc::new(settings_store::HostDocument::new(
            serde_json::json!({
                "locale": {"first_day_of_week": "Sunday", "time_format": "H12"}
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
            |_| Ok(()),
        ));
        let follow = Formats::new();
        let source: Arc<dyn settings_store::SettingsSource> = Arc::clone(&document) as _;
        settings_store::install_source(Some(source));

        assert!(follow.follow(), "the person starts their week on Sunday");
        assert_eq!(
            ui::format::region::current().first_day,
            ui::format::region::FirstDayOfWeek::Sunday
        );
        assert!(!follow.follow(), "unchanged settings read nothing again");

        document
            .update_section(
                "locale",
                serde_json::json!({"first_day_of_week": "Monday", "time_format": "H24"})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .unwrap();
        assert!(follow.follow(), "they put their week back to Monday");

        settings_store::install_source(None);
        ui::format::region::install(before);
    }
}
