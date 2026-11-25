//! The imager's record on its own: reviewing images, the profile's field edits, merging
//! and history, through the same api the host uses.

use super::*;
use crate::model::{Filesystem, SourceKind, sample_recent_sources, sample_source};
use tilo_resource::{AppNamed, Edit, Replica};

fn edit(imager: &mut Replica<Imager, AppNamed>, edit: &Edit<Imager>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    imager
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn review(imager: &mut Replica<Imager, AppNamed>, source: &Source) {
    imager
        .apply(Imager::SELECT_SOURCE.with(source.clone()))
        .unwrap();
}

fn recent_paths(imager: &Replica<Imager, AppNamed>) -> Vec<String> {
    imager
        .state()
        .unwrap()
        .recent_sources()
        .into_iter()
        .map(|source| source.path)
        .collect()
}

tilo_test_env::scenario! {
    fn the_imager_record_edits_converge_and_store_no_secret;

    phase a_new_record_starts_with_empty_records;
    {
        let imager = Replica::<Imager>::new().in_app(crate::resource::APP_ID, "Imager");
        let state = imager.state().unwrap();
        assert!(state.source.is_none());
        assert!(state.recent.is_empty());
        assert_eq!(state.profile, InstallProfile::default());
    }

    phase profile_field_edits_set_one_field_and_store_no_secret;
    {
        let mut imager = Replica::<Imager>::new().in_app(crate::resource::APP_ID, "Imager");
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
            profile.device_label.replace("Lab tower"),
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
        assert_eq!(p.device_label, "Lab tower");
        assert!(p.enroll_local_identity);
        let mut local = imager.state().unwrap();
        local.profile.user.password = "hunter22".into();
        local.profile.encryption_passphrase = "correct horse".into();
        let mut doc = tilo_resource::Doc::new();
        tilo_resource::Fields::write_fields(&local, &mut doc, &automerge::ROOT).unwrap();
        let saved = String::from_utf8_lossy(&doc.save()).into_owned();
        assert!(
            !saved.contains("hunter22") && !saved.contains("correct horse"),
            "secrets are never collaborative"
        );
    }

    phase reviewing_images_keeps_them_in_recency_order;
    {
        let mut imager = Replica::<Imager>::new().in_app(crate::resource::APP_ID, "Imager");
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

    phase concurrent_edits_converge;
    {
        let mut ours = Replica::<Imager>::new().in_app(crate::resource::APP_ID, "Imager");
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

    phase an_agents_changes_render_as_the_apps_sentences;
    {
        let mut imager = Replica::<Imager>::new().in_app(crate::resource::APP_ID, "Imager").as_agent_for("claude", "ada");
        review(&mut imager, &sample_source());
        let profile = Imager::paths().profile;
        for change in [
            profile.hostname.replace("lab01"),
            profile.user.username.replace("ada"),
            profile.filesystem.set(Filesystem::Ext4),
            profile.enroll_local_identity.set(true),
        ] {
            edit(&mut imager, &change).unwrap();
        }
        imager.apply(Imager::CLEAR_SOURCE.invoke()).unwrap();
        let state = imager.state().unwrap();
        assert_eq!(
            (state.profile.hostname.as_str(), state.source.is_none()),
            ("lab01", true)
        );

        let entries: Vec<_> = tilo_resource::history::versions(imager.doc_mut())
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.acting.agent_id() == Some("claude"))
            })
            .collect();
        let source = CATALOG.source();
        let catalogue = tilo_history::Catalogue::new("en-US", &Vec::from_iter(source));
        let reader = tilo_history::Reader::new("en-US")
            .with_app(&catalogue)
            .with_labels(&ui::history_source::sentence::ThreadLabels);
        let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
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
            (invoker.acting.is_agent(), invoker.acting.acted_for_person()),
            (true, Some("ada"))
        );
    }
}
