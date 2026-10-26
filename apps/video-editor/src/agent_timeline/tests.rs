#![allow(
    clippy::float_cmp,
    reason = "test assertions over exact values written and read back"
)]

use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_actions::DoorError;
use ui::tilo_resource::{Invoke, Resource as _};

use super::*;
use crate::VideoEditor;
use crate::resource::load_project;

/// A WAV header announcing `seconds` of 16 kHz mono 8-bit sound; the probe
/// reads the length from the header, so no samples are needed.
fn wav(seconds: u32) -> Vec<u8> {
    let rate: u32 = 16_000;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(36_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes()); // PCM
    bytes.extend(1_u16.to_le_bytes()); // mono
    bytes.extend(rate.to_le_bytes());
    bytes.extend(rate.to_le_bytes()); // byte rate
    bytes.extend(1_u16.to_le_bytes()); // block align
    bytes.extend(8_u16.to_le_bytes()); // bits
    bytes.extend(b"data");
    bytes.extend((rate * seconds).to_le_bytes());
    bytes
}

/// A PNG header of a `width`×`height` picture.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(13_u32.to_be_bytes());
    bytes.extend(b"IHDR");
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

/// An editor over `project`, as an agent's replica holds it.
fn editor(project: &Project) -> TestApp<VideoEditor> {
    let mut app = TestApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app
}

/// The address of clip `id`, where its own verbs are published.
fn clip_resource(id: Uuid) -> String {
    format!("{}/clips/{id}", Project::KIND)
}

/// A 30 s shot on V1 at 0 whose sound is split off onto a linked clip on A1:
/// the project, the picture clip and the sound clip.
fn linked_shot() -> (Project, Uuid, Uuid) {
    let mut p = Project::new("shot");
    let media = Uuid::new_v4();
    p.media.push(MediaItem {
        id: media,
        name: "shot.mp4".into(),
        kind: MediaKind::Video,
        duration: 30.0,
        has_audio: true,
        ..MediaItem::default()
    });
    let (v1, a1) = (Uuid::new_v4(), Uuid::new_v4());
    p.tracks.push(Track {
        id: v1,
        name: "V1".into(),
        index: 1,
        ..Track::default()
    });
    p.tracks.push(Track {
        id: a1,
        name: "A1".into(),
        kind: TrackKind::Audio,
        index: 1,
        ..Track::default()
    });
    let clip = p.add_clip(v1, media, 0.0).unwrap();
    let sound = p.add_linked_audio(clip, a1).unwrap();
    (p, clip, sound)
}

/// Import `bytes` as `name` through the effect, answering its value.
fn import(app: &mut TestApp<VideoEditor>, bytes: &[u8], name: &str) -> Result<Value, DoorError> {
    let hash = app.content().put(bytes).unwrap();
    app.effect(&VideoEditorEffect::ImportContent {
        content: format!("tilo.content:{hash}"),
        name: Some(name.to_owned()),
        duration: None,
    })
    .map(|output| output.value.unwrap())
}

fn call(app: &mut TestApp<VideoEditor>, verb: &str, args: Value) -> Result<Value, DoorError> {
    app.call(verb, args)
        .map(|answer| answer.unwrap_or(Value::Null))
}

#[test]
fn room_content_lands_in_the_bin_once_with_the_length_its_container_says() {
    let mut app = editor(&Project::new("cut"));
    let first = import(&mut app, &wav(4), "voice.wav").unwrap();
    assert_eq!(first["source"]["kind"], "audio");
    assert_eq!(first["source"]["duration"], 4.0);
    let media = first["media"].as_str().unwrap().to_owned();
    let again = import(&mut app, &wav(4), "voice.wav").unwrap();
    assert_eq!(
        again["media"],
        media.as_str(),
        "the same content is the same source"
    );
    let state = app.state();
    assert_eq!(state.media.len(), 1);
    let item = state.media.first().unwrap();
    assert_eq!(item.name, "voice.wav");
    assert!(
        item.path.starts_with("tilo.content:"),
        "the source is read by its content address: {}",
        item.path
    );
}

#[test]
fn content_that_is_no_media_or_not_held_is_refused() {
    let mut app = editor(&Project::new("cut"));
    let error = import(&mut app, b"<!DOCTYPE html><html></html>", "page.mp4").unwrap_err();
    assert!(
        format!("{error:?}").contains("not a media file"),
        "{error:?}"
    );
    let missing = app
        .effect(&VideoEditorEffect::ImportContent {
            content: format!("tilo.content:{}", "ab".repeat(32)),
            name: None,
            duration: None,
        })
        .unwrap_err();
    assert!(format!("{missing:?}").contains("not held"), "{missing:?}");
    assert!(app.state().media.is_empty());
}

#[test]
fn a_source_named_in_the_bin_is_placed_after_the_last_clip_of_its_kind() {
    let mut app = editor(&Project::new("cut"));
    import(&mut app, &wav(4), "voice.wav").unwrap();
    import(&mut app, &png(640, 360), "logo.png").unwrap();

    let first = call(&mut app, "add-clip", json!({ "media": "voice.wav" })).unwrap();
    let second = call(
        &mut app,
        "add-clip",
        json!({ "media": "VOICE.WAV", "in": 1.0 }),
    )
    .unwrap();
    assert_eq!(
        first["placed"]["track"], "A1",
        "a sound goes on a sound track, made for it"
    );
    assert_eq!(
        second["placed"]["start"], 4.0,
        "after the clip already there"
    );
    assert_eq!(second["placed"]["end"], 7.0);

    let still = call(
        &mut app,
        "add-clip",
        json!({ "media": "logo.png", "at": 2.0, "out": 9.0, "fade_in": 0.5 }),
    )
    .unwrap();
    assert_eq!(still["placed"]["track"], "V1");
    assert_eq!(
        still["placed"]["end"], 11.0,
        "a still lasts as long as asked"
    );
    assert_eq!(still["placed"]["fade_in"], 0.5);
    let clip: Uuid = still["clip"].as_str().unwrap().parse().unwrap();
    assert!(app.state().clip(clip).is_some());
}

#[test]
fn footage_named_on_no_track_goes_on_v1_under_the_titles_above_it() {
    let mut app = editor(&Project::new("cut"));
    import(&mut app, &png(640, 360), "logo.png").unwrap();
    call(&mut app, "add-clip", json!({ "media": "logo.png" })).unwrap();
    call(&mut app, "add-track", json!({ "kind": "Video" })).unwrap();
    let placed = call(
        &mut app,
        "add-clip",
        json!({ "media": "logo.png", "at": 10.0 }),
    )
    .unwrap();
    assert_eq!(
        placed["placed"]["track"], "V1",
        "the cut stays on V1 though V2 is the top track"
    );
}

#[test]
fn placing_refuses_unknown_sources_taken_spans_and_windows_past_the_source() {
    let mut app = editor(&Project::new("cut"));
    import(&mut app, &wav(4), "voice.wav").unwrap();
    let unknown = call(&mut app, "add-clip", json!({ "media": "drone.mov" })).unwrap_err();
    assert!(
        format!("{unknown:?}").contains("import-content"),
        "the refusal says how to bring a source in: {unknown:?}"
    );
    call(
        &mut app,
        "add-clip",
        json!({ "media": "voice.wav", "at": 0.0 }),
    )
    .unwrap();
    let taken = call(
        &mut app,
        "add-clip",
        json!({ "media": "voice.wav", "at": 2.0 }),
    )
    .unwrap_err();
    assert!(format!("{taken:?}").contains("taken"), "{taken:?}");
    let long = call(
        &mut app,
        "add-clip",
        json!({ "media": "voice.wav", "out": 9.0 }),
    )
    .unwrap_err();
    assert!(format!("{long:?}").contains("lasts"), "{long:?}");
    let unnamed = call(
        &mut app,
        "add-clip",
        json!({ "media": "voice.wav", "track": "Music" }),
    )
    .unwrap_err();
    assert!(format!("{unnamed:?}").contains("no track"), "{unnamed:?}");
    let wrong = call(
        &mut app,
        "add-clip",
        json!({ "media": "voice.wav", "track": "V9" }),
    )
    .unwrap_err();
    assert!(
        format!("{wrong:?}").contains("would be a video track"),
        "{wrong:?}"
    );
    assert_eq!(app.state().tracks.len(), 1, "a refused clip makes no track");
}

#[test]
fn a_new_project_takes_its_first_clips_on_the_tracks_they_name() {
    let mut app = editor(&Project::new("cut"));
    assert!(app.state().tracks.is_empty(), "a new project has no tracks");
    import(&mut app, &png(640, 360), "logo.png").unwrap();
    import(&mut app, &wav(4), "voice.wav").unwrap();

    let first = call(
        &mut app,
        "add-clip",
        json!({ "media": "logo.png", "track": "V1", "at": 0.0, "out": 3.0 }),
    )
    .unwrap();
    assert_eq!(first["placed"]["track"], "V1");
    let title = call(&mut app, "add-title", json!({ "text": "Harbour" })).unwrap();
    let over = call(
        &mut app,
        "add-clip",
        json!({ "media": title["media"], "track": "v2", "at": 0.0, "out": 2.0 }),
    )
    .unwrap();
    assert_eq!(over["placed"]["track"], "V2", "a lane's name in any case");
    let sound = call(
        &mut app,
        "add-clip",
        json!({ "media": "voice.wav", "track": "A1" }),
    )
    .unwrap();
    assert_eq!(sound["placed"]["track"], "A1");
    call(
        &mut app,
        "add-clip",
        json!({ "media": "logo.png", "track": "V1", "at": 3.0, "out": 4.0 }),
    )
    .unwrap();

    let state = app.state();
    let lanes: Vec<(&str, TrackKind, u32)> = state
        .display_tracks()
        .into_iter()
        .map(|t| (t.name.as_str(), t.kind, t.index))
        .collect();
    assert_eq!(
        lanes,
        vec![
            ("V2", TrackKind::Video, 2),
            ("V1", TrackKind::Video, 1),
            ("A1", TrackKind::Audio, 1),
        ],
        "each named lane is made once, and V2 is over V1"
    );
    assert_eq!(state.clips.len(), 4);
    let read = read_timeline(&state, None).unwrap();
    assert_eq!(read["project"]["duration"], 7.0, "{read}");

    call(&mut app, "add-track", json!({ "kind": "Video" })).unwrap();
    assert_eq!(
        app.state().display_tracks().first().map(|t| t.name.clone()),
        Some("V3".to_owned()),
        "add-track still makes the next lane"
    );
}

#[test]
fn a_moved_clip_takes_its_linked_partner_by_the_same_amount() {
    let (p, clip, audio) = linked_shot();
    let track = p.clip(clip).unwrap().track;
    let mut app = editor(&p);
    let moved = app
        .call_on(&clip_resource(clip), "move-clip", json!({ "at": 5.0 }))
        .unwrap()
        .unwrap();
    assert_eq!(moved["clips"].as_array().unwrap().len(), 2, "{moved}");
    let state = app.state();
    assert_eq!(state.clip(clip).unwrap().start, 5.0);
    assert_eq!(state.clip(audio).unwrap().start, 5.0);
    assert_eq!(state.clip(clip).unwrap().track, track);
    let onto_sound = app.call_on(
        &clip_resource(clip),
        "move-clip",
        json!({ "at": 0.0, "track": "A1" }),
    );
    assert!(
        onto_sound.is_err(),
        "a picture never lands on a sound track"
    );
}

#[test]
fn a_clip_is_set_and_bad_values_are_refused_without_a_change() {
    let (p, clip, _) = linked_shot();
    let mut app = editor(&p);
    let address = clip_resource(clip);
    let set = app
        .call_on(
            &address,
            "set-clip",
            json!({ "fade_in": 1.0, "fade_out": 2.0, "opacity": 0.5, "transition": "dissolve" }),
        )
        .unwrap()
        .unwrap();
    let shown = &set["clips"][0];
    assert_eq!(
        (
            shown["fade_in"].clone(),
            shown["opacity"].clone(),
            shown["transition"].clone()
        ),
        (json!(1.0), json!(0.5), json!("dissolve"))
    );
    for bad in [
        json!({ "opacity": 1.5 }),
        json!({ "fade_in": 20.0, "fade_out": 20.0 }),
        json!({ "transition": "spin" }),
        json!({}),
    ] {
        assert!(
            app.call_on(&address, "set-clip", bad.clone()).is_err(),
            "{bad}"
        );
    }
    let state = app.state();
    assert_eq!(state.clip(clip).unwrap().fade_out, 2.0);
}

#[test]
fn removing_a_clip_applies_at_once_and_can_close_the_gap() {
    let mut app = editor(&Project::new("cut"));
    import(&mut app, &wav(4), "voice.wav").unwrap();
    let first = call(&mut app, "add-clip", json!({ "media": "voice.wav" })).unwrap();
    let second = call(&mut app, "add-clip", json!({ "media": "voice.wav" })).unwrap();
    let (first, second): (Uuid, Uuid) = (
        first["clip"].as_str().unwrap().parse().unwrap(),
        second["clip"].as_str().unwrap().parse().unwrap(),
    );
    let address = clip_resource(first);
    // Undo takes a removed clip back, so removing asks nothing.
    let removed = app
        .call_on(&address, "remove-clip", json!({ "ripple": true }))
        .expect("an undoable removal applies at once")
        .unwrap();
    assert_eq!(removed["removed"], json!([first.to_string()]));
    let state = app.state();
    assert!(state.clip(first).is_none());
    assert_eq!(state.clip(second).unwrap().start, 0.0, "the gap closed");
    assert!(app.undo(), "the removal is one undo step");
    assert!(
        app.state().clip(first).is_some(),
        "Undo brings the clip back"
    );
}

#[test]
fn a_title_is_added_styled_and_restyled_by_its_answered_id() {
    let mut app = editor(&Project::new("cut"));
    let added = call(
        &mut app,
        "add-title",
        json!({ "text": "Opening", "colour": "#ffcc00", "size": 0.2, "align": "left" }),
    )
    .unwrap();
    let media = added["media"].as_str().unwrap().to_owned();
    assert_eq!(added["source"]["colour"], "#ffcc00");
    assert_eq!(added["source"]["align"], "left");
    let bad = call(
        &mut app,
        "add-title",
        json!({ "text": "x", "colour": "teal-ish" }),
    );
    assert!(bad.is_err());

    let restyled = app
        .call_on(
            &format!("{}/media/{media}", Project::KIND),
            "set-title",
            json!({ "text": "Chapter one\nby Ada", "size": 0.1 }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(restyled["source"]["text"], "Chapter one\nby Ada");
    let state = app.state();
    let title = state.media(media.parse().unwrap()).unwrap();
    assert_eq!(title.name, "Chapter one");
    assert_eq!(title.text.as_ref().unwrap().size, 0.1);
    assert_eq!(title.text.as_ref().unwrap().align, TextAlign::Left, "kept");
    call(&mut app, "add-clip", json!({ "media": media, "out": 8.0 })).unwrap();
}

#[test]
fn the_timeline_reads_whole_or_by_item_and_exports_as_the_same_json() {
    let mut app = editor(&Project::new("cut"));
    import(&mut app, &wav(4), "voice.wav").unwrap();
    let placed = call(&mut app, "add-clip", json!({ "media": "voice.wav" })).unwrap();
    let whole = call(&mut app, "read", json!({})).unwrap();
    assert_eq!(whole["tracks"][0]["name"], "A1");
    assert_eq!(whole["tracks"][0]["clips"][0]["media_name"], "voice.wav");
    assert_eq!(whole["bin"][0]["kind"], "audio");
    let clip = call(&mut app, "read", json!({ "item": placed["clip"] })).unwrap();
    assert_eq!(clip["end"], 4.0);
    let track = call(&mut app, "read", json!({ "item": "a1" })).unwrap();
    assert_eq!(track["kind"], "audio");
    assert!(call(&mut app, "read", json!({ "item": "nothing" })).is_err());

    let exported = app
        .effect(&VideoEditorEffect::ExportTimelineContent)
        .unwrap();
    let address = exported.value.unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(exported.resources, vec![address.clone()]);
    let hash = tilo_actions::content_hash(&address).unwrap();
    let kept: Value = serde_json::from_slice(&app.content().read(hash).unwrap()).unwrap();
    assert_eq!(kept, whole);
}

#[test]
fn colours_and_alignments_are_read_as_people_write_them() {
    assert_eq!(colour_named("white").unwrap(), [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(colour_named("#f00").unwrap(), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(colour_hex(colour_named("#ffcc0080").unwrap()), "#ffcc0080");
    assert!(colour_named("#12345").is_err());
    assert_eq!(align_named("Centre").unwrap(), TextAlign::Center);
    assert!(transition_named("spin").is_err());
    assert_eq!(
        content_media_id(&"ab".repeat(32)),
        content_media_id(&"AB".repeat(32))
    );
}

#[test]
fn a_field_edit_naming_no_field_says_which_fields_there_are() {
    let mut app = editor(&Project::new("cut"));
    let edit = json!({ "path": [{ "field": "project" }, { "field": "name" }], "op": "set", "value": "Promo" });
    let refused = format!(
        "{:?}",
        call(&mut app, "field-edit", json!({ "edit": edit })).unwrap_err()
    );
    assert!(refused.contains("no field `project`"), "{refused}");
    assert!(!refused.contains(": : "), "no empty place: {refused}");
    assert!(refused.contains("name"), "the fields are named: {refused}");
    let renamed = json!({ "path": [{ "field": "name" }], "op": "set", "value": "Promo" });
    call(&mut app, "field-edit", json!({ "edit": renamed })).unwrap();
    assert_eq!(app.state().name, "Promo");
}

/// The refusal `result` carries, as text.
fn refusal(result: Result<Value, DoorError>) -> String {
    format!("{:?}", result.expect_err("refused"))
}

#[test]
fn a_title_is_restyled_by_the_words_it_shows() {
    let mut app = editor(&Project::new("cut"));
    let added = call(&mut app, "add-title", json!({ "text": "HARBOUR DAYS" })).unwrap();
    let media = added["media"].as_str().unwrap().to_owned();
    // What agents sent: the title named by its words, not its id.
    let restyled = call(
        &mut app,
        "set-title",
        json!({ "media": " harbour   Days ", "text": "OPENS 7 NOV", "colour": "#ffd400" }),
    )
    .unwrap();
    assert_eq!(restyled["media"], media.as_str());
    assert_eq!(restyled["source"]["text"], "OPENS 7 NOV");
    assert_eq!(restyled["source"]["colour"], "#ffd400");

    let placed = call(&mut app, "add-clip", json!({ "media": media, "out": 3.0 })).unwrap();
    let by_clip = call(
        &mut app,
        "set-title",
        json!({ "media": placed["clip"], "size": 0.2 }),
    )
    .unwrap();
    assert_eq!(
        by_clip["media"],
        media.as_str(),
        "a clip names the title it shows"
    );

    let unknown = refusal(call(
        &mut app,
        "set-title",
        json!({ "media": "HARBOUR DAYS", "colour": "red" }),
    ));
    assert!(
        unknown.contains("no title reads `HARBOUR DAYS`")
            && unknown.contains("“OPENS 7 NOV”")
            && unknown.contains(&media),
        "the refusal lists the titles and their ids: {unknown}"
    );
}

#[test]
fn titles_reading_alike_or_none_at_all_are_refused_with_what_to_name() {
    let mut app = editor(&Project::new("cut"));
    let none = refusal(call(
        &mut app,
        "set-title",
        json!({ "media": "Opening", "colour": "red" }),
    ));
    assert!(none.contains("add-title"), "{none}");
    let first = call(&mut app, "add-title", json!({ "text": "Opening" })).unwrap();
    let second = call(&mut app, "add-title", json!({ "text": "opening" })).unwrap();
    let before = app.state().media;
    let alike = refusal(call(
        &mut app,
        "set-title",
        json!({ "media": "OPENING", "colour": "red" }),
    ));
    for id in [&first["media"], &second["media"]] {
        assert!(alike.contains(id.as_str().unwrap()), "{alike}");
    }
    assert!(alike.contains("2 titles"), "{alike}");
    assert_eq!(
        app.state().media,
        before,
        "a refused restyle changes no title"
    );
}

#[test]
fn a_clip_is_named_by_its_source_and_by_its_start_when_placed_twice() {
    let mut app = editor(&Project::new("cut"));
    import(&mut app, &png(640, 360), "shot-b.png").unwrap();
    import(&mut app, &wav(4), "voice.wav").unwrap();
    let first = call(
        &mut app,
        "add-clip",
        json!({ "media": "shot-b.png", "at": 0.0, "out": 3.0 }),
    )
    .unwrap();
    let second = call(
        &mut app,
        "add-clip",
        json!({ "media": "shot-b.png", "at": 3.0, "out": 6.0 }),
    )
    .unwrap();
    let (first, second) = (
        first["clip"].as_str().unwrap().to_owned(),
        second["clip"].as_str().unwrap().to_owned(),
    );

    let twice = refusal(call(
        &mut app,
        "set-clip",
        json!({ "clip": "shot-b.png", "fade_out": 1.0 }),
    ));
    assert!(
        twice.contains(&first) && twice.contains(&second) && twice.contains("shot-b.png@<start>"),
        "the refusal lists each placement with its id: {twice}"
    );
    let set = call(
        &mut app,
        "set-clip",
        json!({ "clip": "SHOT-B.PNG@3", "fade_out": 1.0 }),
    )
    .unwrap();
    assert_eq!(set["clips"][0]["id"], second.as_str());
    let spelled = call(
        &mut app,
        "set-clip",
        json!({ "clip": "shot-b.png at 0 s", "fade_in": 0.5 }),
    )
    .unwrap();
    assert_eq!(spelled["clips"][0]["id"], first.as_str());
    let late = refusal(call(
        &mut app,
        "set-clip",
        json!({ "clip": "shot-b.png@7", "fade_in": 0.5 }),
    ));
    assert!(
        late.contains("starts at 7") && late.contains(&first),
        "{late}"
    );
    let unknown = refusal(call(
        &mut app,
        "remove-clip",
        json!({ "clip": "shot-c.mp4" }),
    ));
    assert!(
        unknown.contains("names no clip") && unknown.contains(&second),
        "an unknown name lists the clips: {unknown}"
    );

    let sound = call(&mut app, "add-clip", json!({ "media": "voice.wav" })).unwrap();
    call(
        &mut app,
        "move-clip",
        json!({ "clip": "voice.wav", "at": 1.0 }),
    )
    .unwrap();
    call(
        &mut app,
        "trim-out-clip",
        json!({ "clip": "Voice.wav", "at": 4.0 }),
    )
    .unwrap();
    let state = app.state();
    let moved = state
        .clip(sound["clip"].as_str().unwrap().parse().unwrap())
        .unwrap();
    assert_eq!((moved.start, moved.end()), (1.0, 4.0));
    call(&mut app, "remove-clip", json!({ "clip": "voice.wav" })).unwrap();
    assert_eq!(app.state().clips.len(), 2);
}

#[test]
fn a_picture_and_its_linked_sound_are_named_as_one_placement() {
    let (p, clip, sound) = linked_shot();
    let mut app = editor(&p);
    let set = call(
        &mut app,
        "set-clip",
        json!({ "clip": "shot.mp4", "fade_in": 1.0 }),
    )
    .unwrap();
    assert_eq!(set["clips"][0]["id"], clip.to_string().as_str());
    call(
        &mut app,
        "move-clip",
        json!({ "clip": "shot.mp4", "at": 2.0 }),
    )
    .unwrap();
    let state = app.state();
    assert_eq!(state.clip(clip).unwrap().start, 2.0);
    assert_eq!(state.clip(sound).unwrap().start, 2.0);
}

#[test]
fn a_start_is_read_only_after_a_name() {
    assert_eq!(with_start("shot-b.mp4@3"), Some(("shot-b.mp4", 3.0)));
    assert_eq!(with_start("shot-b.mp4 @ 2.5s"), Some(("shot-b.mp4", 2.5)));
    assert_eq!(with_start("Shot B AT 4 s"), Some(("Shot B", 4.0)));
    assert_eq!(with_start("Lunch at the harbour"), None);
    assert_eq!(with_start("@3"), None);
    assert_eq!(with_start("shot.mp4@-1"), None);
    assert_eq!(folded("  Harbour\n  DAYS "), "harbour days");
}
