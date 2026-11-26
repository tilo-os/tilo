//! The workspace's activity as the session publishes it: the feed a window's History follows,
//! and the people, groups and changes it names there. The activity declares its records, its
//! actions and their arguments once (`tilo_session_core::desktop::activity::vocabulary`), and
//! this binds to that published surface so the two cannot drift.

tilo_resource::bindings!(
    provider = "tilo.activity",
    surface = "../../desktop/session-core/workspace-activity-surface.json"
);
