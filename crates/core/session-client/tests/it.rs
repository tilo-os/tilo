// One test binary instead of one per file; #[path] keeps the files in place.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

// A site admits its members' records through an Automerge worker: this binary
// is that worker, as the server host's own is.
tilo_automerge_isolate::test_worker!();

#[path = "support/mod.rs"]
mod support;

#[path = "support/native.rs"]
mod native;

#[path = "barrier_of_workspace.rs"]
mod barrier_of_workspace;
#[path = "browser_root.rs"]
mod browser_root;
#[path = "carriers.rs"]
mod carriers;
#[path = "channel_member.rs"]
mod channel_member;
#[path = "checkpoint_race.rs"]
mod checkpoint_race;
#[path = "content_answer.rs"]
mod content_answer;
#[path = "conversation_over_https.rs"]
mod conversation_over_https;
#[path = "entry.rs"]
mod entry;
#[path = "fetch_proxy.rs"]
mod fetch_proxy;
#[path = "first_landing.rs"]
mod first_landing;
#[path = "flow_four_devices.rs"]
mod flow_four_devices;
#[path = "flow_laptop_phone_server.rs"]
mod flow_laptop_phone_server;
#[path = "folder_scale.rs"]
mod folder_scale;
#[path = "forgotten_device.rs"]
mod forgotten_device;
#[path = "fresh_person.rs"]
mod fresh_person;
#[path = "guest_opens_chat.rs"]
mod guest_opens_chat;
#[path = "handed_history.rs"]
mod handed_history;
#[path = "headless_audio_lifetime.rs"]
mod headless_audio_lifetime;
#[path = "held_document_keys.rs"]
mod held_document_keys;
#[path = "host_order.rs"]
mod host_order;
#[path = "invitation_entry.rs"]
mod invitation_entry;
#[path = "late_join.rs"]
mod late_join;
#[path = "layout_views.rs"]
mod layout_views;
#[path = "lifecycle_states.rs"]
mod lifecycle_states;
#[path = "lounge_fresh_page.rs"]
mod lounge_fresh_page;
#[path = "message_latency.rs"]
mod message_latency;
#[path = "newcomer_channels.rs"]
mod newcomer_channels;
#[path = "node_workspace.rs"]
mod node_workspace;
#[path = "on_demand_records.rs"]
mod on_demand_records;
#[path = "one_write_path.rs"]
mod one_write_path;
#[path = "outbox_replay.rs"]
mod outbox_replay;
#[path = "own_workspace.rs"]
mod own_workspace;
#[path = "place_workspace.rs"]
mod place_workspace;
#[path = "plain_http.rs"]
mod plain_http;
#[path = "recovery.rs"]
mod recovery;
#[path = "resume.rs"]
mod resume;
#[path = "revoked_device_history.rs"]
mod revoked_device_history;
#[path = "routed_actions.rs"]
mod routed_actions;
#[path = "server_restart.rs"]
mod server_restart;
#[path = "session_store.rs"]
mod session_store;
#[path = "surface_by_hash.rs"]
mod surface_by_hash;
#[path = "voice_browsers.rs"]
mod voice_browsers;
#[path = "write_before_record.rs"]
mod write_before_record;
