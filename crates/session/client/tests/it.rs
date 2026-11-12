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
// is that worker, as the relay host's own is.
tilo_automerge_isolate::test_worker!();

#[path = "support/participant.rs"]
mod participant;

#[path = "support/call.rs"]
mod call;

#[path = "support/mod.rs"]
mod support;

#[path = "agent_door.rs"]
mod agent_door;
#[path = "agent_login.rs"]
mod agent_login;
#[path = "browser_root.rs"]
mod browser_root;
#[path = "carriers.rs"]
mod carriers;
#[path = "channel_member.rs"]
mod channel_member;
#[path = "chat_landing_work.rs"]
mod chat_landing_work;
#[path = "chat_restart.rs"]
mod chat_restart;
#[path = "checkpoint_race.rs"]
mod checkpoint_race;
#[path = "compaction.rs"]
mod compaction;
#[path = "content_answer.rs"]
mod content_answer;
#[path = "conversation_over_https.rs"]
mod conversation_over_https;
#[path = "crowded_entry.rs"]
mod crowded_entry;
#[path = "door_entry.rs"]
mod door_entry;
#[path = "fetch_proxy.rs"]
mod fetch_proxy;
#[path = "forgotten_device.rs"]
mod forgotten_device;
#[path = "fresh_person.rs"]
mod fresh_person;
#[path = "guest_opens_chat.rs"]
mod guest_opens_chat;
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
#[path = "login_codes.rs"]
mod login_codes;
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
#[path = "outbox_replay.rs"]
mod outbox_replay;
#[path = "own_workspace.rs"]
mod own_workspace;
#[path = "pairing.rs"]
mod pairing;
#[path = "passkey_root.rs"]
mod passkey_root;
#[path = "place_workspace.rs"]
mod place_workspace;
#[path = "plain_http.rs"]
mod plain_http;
#[path = "recovery.rs"]
mod recovery;
#[path = "refused_commit.rs"]
mod refused_commit;
#[path = "resume.rs"]
mod resume;
#[path = "stand_in_revocation.rs"]
mod stand_in_revocation;
#[path = "surface_by_hash.rs"]
mod surface_by_hash;
#[path = "voice_browsers.rs"]
mod voice_browsers;
#[path = "voice_call.rs"]
mod voice_call;
#[path = "voice_call_list.rs"]
mod voice_call_list;
#[path = "voice_every_workspace.rs"]
mod voice_every_workspace;
#[path = "voice_open.rs"]
mod voice_open;
#[path = "voice_participation.rs"]
mod voice_participation;
#[path = "window_input.rs"]
mod window_input;
#[path = "write_before_record.rs"]
mod write_before_record;
