//! The imager's record on its own: reviewing images, the profile's field
//! edits, merging and history, through the same door the host uses.

use super::*;
use crate::model::{Filesystem, SourceKind, sample_recent_sources, sample_source};
use ui::tilo_resource::{Edit, Replica, Resource as _};

fn edit(imager: &mut Replica<Imager>, edit: &Edit<Imager>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    imager
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn review(imager: &mut Replica<Imager>, source: &Source) {
    imager
        .apply(Imager::SELECT_SOURCE.with(source.clone()))
        .unwrap();
}

fn recent_paths(imager: &Replica<Imager>) -> Vec<String> {
    imager
        .state()
        .unwrap()
        .recent_sources()
        .into_iter()
        .map(|source| source.path)
        .collect()
}

/// A new record holds no image, no history and the default profile.
#[test]
fn a_new_record_starts_with_empty_records() {
    let imager = Replica::<Imager>::new();
    let state = imager.state().unwrap();
    assert!(state.source.is_none());
    assert!(state.recent.is_empty());
    assert_eq!(state.profile, InstallProfile::default());
}

/// Each profile field is its own field edit: setting one leaves the others
/// as they were, and nothing secret is stored.
#[test]
fn profile_field_edits_set_one_field_and_store_no_secret() {
    let mut imager = Replica::<Imager>::new();
    let profile = Imager::paths().profile;
    for change in [
        profile.hostname.replace("lab01"),
        profile.user.full_name.replace("Ada Lovelace"),
        profile.user.username.replace("ada"),
        profile.locale.replace("nl_NL.UTF-8"),
        profile.timezone.replace("Europe/Amsterdam"),
        profile.keyboard.replace("nl"),
        profile.filesystem.set(Filesystem::Xfs),
        profile.wifi_ssid.replace("lab"),
        profile.device_label.replace("Lab desk"),
        profile.enroll_local_identity.set(true),
    ] {
        edit(&mut imager, &change).unwrap();
    }
    let p = imager.state().unwrap().profile;
    assert_eq!(p.hostname, "lab01");
    assert_eq!(p.user.full_name, "Ada Lovelace");
    assert_eq!(p.user.username, "ada");
    assert_eq!(p.locale, "nl_NL.UTF-8");
    assert_eq!(p.timezone, "Europe/Amsterdam");
    assert_eq!(p.keyboard, "nl");
    assert_eq!(p.filesystem, Filesystem::Xfs);
    assert_eq!(p.wifi_ssid, "lab");
    assert_eq!(p.device_label, "Lab desk");
    assert!(p.enroll_local_identity);
    let mut local = imager.state().unwrap();
    local.profile.user.password = "hunter22".into();
    local.profile.encryption_passphrase = "correct horse".into();
    let mut doc = ui::tilo_resource::Doc::new();
    ui::tilo_resource::Fields::write_fields(&local, &mut doc, &automerge::ROOT).unwrap();
    let saved = String::from_utf8_lossy(&doc.save()).into_owned();
    assert!(
        !saved.contains("hunter22") && !saved.contains("correct horse"),
        "secrets are never collaborative"
    );
}

/// Reviewing an image remembers the one reviewed before at the front of
/// the recent ones; reviewing it again moves it back to the front; the
/// image itself only changes through reviewing or clearing.
#[test]
fn reviewing_images_keeps_them_in_recency_order() {
    let mut imager = Replica::<Imager>::new();
    let [tilo, fedora, arch]: [Source; 3] = sample_recent_sources().try_into().unwrap();
    assert!(
        imager.apply(Imager::CLEAR_SOURCE.with(())).is_err(),
        "nothing to clear before a review"
    );
    review(&mut imager, &tilo);
    assert!(recent_paths(&imager).is_empty());
    review(&mut imager, &fedora);
    review(&mut imager, &arch);
    assert_eq!(
        recent_paths(&imager),
        [fedora.path.clone(), tilo.path.clone()]
    );
    review(&mut imager, &fedora);
    assert_eq!(
        recent_paths(&imager),
        [arch.path.clone(), fedora.path.clone(), tilo.path.clone()],
        "the image reviewed last before is first"
    );
    review(&mut imager, &fedora);
    assert_eq!(
        recent_paths(&imager).len(),
        3,
        "reviewing the same image again remembers nothing new"
    );
    assert!(
        edit(&mut imager, &Imager::paths().source.set(None)).is_err(),
        "the image is not a field anyone sets"
    );
    imager.apply(Imager::CLEAR_SOURCE.with(())).unwrap();
    let state = imager.state().unwrap();
    assert!(state.source.is_none());
    assert_eq!(state.recent.len(), 3, "clearing keeps the history");
}

/// Two people editing different profile fields keep both; two people
/// reviewing at once keep both remembered images, the tie broken by id.
#[test]
fn concurrent_edits_converge() {
    let mut ours = Replica::<Imager>::new();
    review(&mut ours, &sample_source());
    let mut theirs = ours.fork();
    let profile = Imager::paths().profile;
    edit(&mut ours, &profile.hostname.replace("studio")).unwrap();
    edit(&mut theirs, &profile.keyboard.replace("de")).unwrap();
    let [_, fedora, arch]: [Source; 3] = sample_recent_sources().try_into().unwrap();
    review(&mut ours, &fedora);
    review(&mut ours, &arch);
    review(&mut theirs, &arch);
    review(&mut theirs, &fedora);

    ours.sync_with(&mut theirs).unwrap();
    let merged = ours.state().unwrap();
    assert_eq!(merged, theirs.state().unwrap());
    assert_eq!(merged.profile.hostname, "studio");
    assert_eq!(merged.profile.keyboard, "de");
    let remembered = merged.recent_sources();
    assert_eq!(remembered.len(), 3, "both people's history is kept");
    assert_eq!(
        remembered.last().map(|source| source.kind),
        Some(SourceKind::TiloImage),
        "the first image was remembered first"
    );
    let (a, b) = (&remembered[0], &remembered[1]);
    assert!(a.id() < b.id(), "equal ranks sort by id");
}

/// An agent changes the profile and reviews images with one call each, and
/// the person reads what it did in the app's own sentences.
#[test]
fn an_agents_changes_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = ui::framework::testing::TestApp::<ImagerShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = Imager::KIND;
    let source = serde_json::to_value(sample_source()).unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "select-source",
        serde_json::json!({ "source": source }),
    )
    .unwrap();
    let profile = Imager::paths().profile;
    for change in [
        profile.hostname.replace("lab01"),
        profile.user.username.replace("ada"),
        profile.filesystem.set(Filesystem::Ext4),
        profile.enroll_local_identity.set(true),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": change }),
        )
        .unwrap();
    }
    app.call_by(agent, kind, "clear-source", serde_json::json!({}))
        .unwrap();
    let state = app.state();
    assert_eq!(
        (state.profile.hostname.as_str(), state.source.is_none()),
        ("lab01", true)
    );

    let entries: Vec<_> = app
        .history()
        .into_iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences = app.sentences(&entries);
    assert_eq!(
        sentences,
        [
            "Chose the image to write",
            "Changed the hostname",
            "Changed the first user's username",
            "Set the filesystem to Ext4",
            "Chose to enroll the new device",
            "Cleared the image to write",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada"))
    );
}
