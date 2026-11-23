//! The Video Editor (`apps/tl/video-editor/app.tl`) end to end: a source probed and a clip
//! placed, trimmed, split and sped up through its actions; the frame the engine draws, the cut
//! it renders and the file it makes; and its window, with the bin, the timeline's drawing and the
//! inspector.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use std::collections::HashMap;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/video-editor/app.tl");

    const STILL: &str = "tilo-blob://still1";
    const SWATCH: &str = "tilo-blob://swatch1";
    const FILM: &str = "tilo-blob://film1";

    /// The device's engine: what it was asked, and the answers it gives.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// The job's state as the next ask answers it: `running` until it is `done`.
        ready: Rc<RefCell<bool>>,
    }

    impl Engine {
        fn last_input(&self) -> String {
            self.asked
                .borrow()
                .last()
                .map(|(_, input)| input.clone())
                .unwrap_or_default()
        }

        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "probe" => Ok(r#"{"kind":"video","duration":10.0,"width":1920,"height":1080,"fps":30.0,"has_audio":true,"codec":"h264"}"#.to_owned()),
                "still" => Ok(format!(r#"{{"url":"{STILL}"}}"#)),
                "thumb" => Ok(format!(r#"{{"url":"{SWATCH}"}}"#)),
                "sample" => Ok(r#"{"red":0.25,"green":0.5,"blue":0.75}"#.to_owned()),
                "waveform" => Ok(r#"{"peaks":[0.2,0.6,0.4,0.9]}"#.to_owned()),
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op == "render" {
                Ok(r#"{"job":"job-1"}"#.to_owned())
            } else {
                Err(format!("`{op}` is no op this device runs"))
            }
        }

        fn status(&self, job: &str) -> Result<String, String> {
            if job != "job-1" {
                return Err(format!("no job `{job}`"));
            }
            if *self.ready.borrow() {
                Ok(format!(
                    r#"{{"state":"done","done":300,"total":300,"url":"{FILM}","error":""}}"#
                ))
            } else {
                Ok(r#"{"state":"running","done":120,"total":300,"url":"","error":""}"#.to_owned())
            }
        }

        fn cancel(&self, job: &str) -> bool {
            job == "job-1"
        }
    }

    /// The device's content store: what a file an action made holds, and what a file it opens
    /// reads back.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = format!("h{}", bytes.len());
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    struct Studio {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
        device: Device,
    }

    impl Studio {
        fn held(&self, file: &FileValue) -> String {
            String::from_utf8_lossy(&self.device.0.borrow().get(&file.hash).cloned().unwrap_or_default()).into_owned()
        }
    }

    impl Studio {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let device = Device::default();
            rt.set_blobs(device.clone());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Video Editor").unwrap();
            Self {
                rt,
                app,
                root,
                engine,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// A source a person chose, as the device hands it over.
        fn file(name: &str) -> Value {
            Value::file(FileValue::new(
                name,
                "video/mp4",
                1024,
                format!("h{}", name.len()),
            ))
        }

        fn new_project(&self) -> ResourceRef {
            returned_ref(self.call("new_project", vec![Value::resource(&self.root)]))
        }

        fn import(&self, p: &ResourceRef, name: &str) {
            self.call("import_one", vec![Value::resource(p), Self::file(name)]);
        }

        fn place(&self, p: &ResourceRef, media: &ResourceRef, track: &ResourceRef, at: f64) {
            self.call(
                "place",
                vec![
                    Value::resource(p),
                    Value::resource(media),
                    Value::resource(track),
                    Value::Float(at),
                ],
            );
        }

        fn records(&self, p: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(p, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn tracks(&self, p: &ResourceRef) -> Vec<ResourceRef> {
            self.records(p, "tracks")
        }

        fn media(&self, p: &ResourceRef) -> Vec<ResourceRef> {
            self.records(p, "media")
        }

        fn clips(&self, p: &ResourceRef) -> Vec<ResourceRef> {
            self.records(p, "clips")
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, r: &ResourceRef, field: &str) -> f64 {
            match self.rt.read(r, field).unwrap() {
                Value::Float(n) => n,
                Value::Int(n) => f64::from(i32::try_from(n).unwrap_or(0)),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("window"), vec![Value::resource(&self.root)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// A cut with one source on the video track at zero.
        fn placed(&self) -> (ResourceRef, ResourceRef, ResourceRef) {
            let p = self.new_project();
            self.import(&p, "Harbour.mp4");
            let media = self.media(&p).into_iter().next().expect("a source");
            let track = self.tracks(&p)
                .into_iter()
                .find(|t| self.text(t, "name") == "V1")
                .expect("V1");
            self.place(&p, &media, &track, 0.0);
            let clip = self.clips(&p).into_iter().next().expect("a clip");
            (p, media, clip)
        }
    }

    tilo_test_env::scenario! {
        fn the_editor_cuts_a_film_and_shows_it;

        phase a_new_cut_starts_with_a_video_and_a_sound_track;
        {
            let s = Studio::new();
            let p = s.new_project();
            assert_eq!(s.text(&p, "name"), "Untitled project");
            assert_eq!(s.tracks(&p).len(), 2);
            assert_eq!(s.number(&p, "fps"), 30.0, "the shape a cut starts at");
            assert_eq!(s.number(&p, "width"), 1920.0);
        }

        phase a_source_is_probed_and_reaches_the_bin;
        {
            let s = Studio::new();
            let p = s.new_project();
            s.import(&p, "Harbour.mp4");
            assert!(
                s.engine.ops().contains(&"probe".to_owned()),
                "the device was asked what the file is: {:?}",
                s.engine.ops()
            );
            let media = s.media(&p);
            assert_eq!(media.len(), 1);
            assert_eq!(s.text(&media[0], "name"), "Harbour.mp4");
            assert_eq!(s.number(&media[0], "duration"), 10.0);
            assert_eq!(s.number(&media[0], "width"), 1920.0);
            assert_eq!(
                s.text(&media[0], "still"),
                STILL,
                "the bin's swatch is the engine's picture"
            );
            match s.rt.read(&media[0], "peaks").unwrap() {
                Value::List(peaks) => assert_eq!(peaks.len(), 4, "the waveform reached the source"),
                other => panic!("peaks read {other:?}"),
            }
            assert_eq!(
                s.number(&p, "width"),
                1920.0,
                "a cut takes its shape from its first source"
            );
        }

        phase a_source_placed_becomes_a_clip_at_a_place_on_its_track;
        {
            let s = Studio::new();
            let (p, media, clip) = s.placed();
            assert_eq!(s.clips(&p).len(), 1);
            assert_eq!(s.text(&clip, "name"), "Harbour.mp4");
            assert_eq!(s.number(&clip, "start"), 0.0);
            assert_eq!(s.number(&clip, "out_point"), 10.0);
            let _ = media;
            // The frame the preview shows comes from the cut as the engine reads it.
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            assert_eq!(s.text(&p, "still"), STILL);
            let input = s.engine.last_input();
            let ask: serde_json::Value = serde_json::from_str(&input).unwrap();
            let cut: serde_json::Value =
                serde_json::from_str(ask["project"].as_str().unwrap()).unwrap();
            let clip: serde_json::Value =
                serde_json::from_str(cut["clips"][0].as_str().unwrap()).unwrap();
            assert_eq!(clip["out_point"], 10.0);
            let track: serde_json::Value =
                serde_json::from_str(cut["tracks"][0].as_str().unwrap()).unwrap();
            assert_eq!(track["kind"], "video");
        }

        phase a_drag_moves_and_trims_a_clip;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            let id = clip.id_string();
            s.call(
                "commit_drag",
                vec![
                    Value::resource(&p),
                    Value::text("move"),
                    Value::text(id.clone()),
                    Value::Float(0.0),
                    Value::Float(3.0),
                ],
            );
            assert_eq!(s.number(&clip, "start"), 3.0, "moved along the timeline");
            assert_eq!(s.number(&clip, "in_point"), 0.0, "its source stayed put");
            s.call(
                "commit_drag",
                vec![
                    Value::resource(&p),
                    Value::text("tail"),
                    Value::text(id.clone()),
                    Value::Float(0.0),
                    Value::Float(-4.0),
                ],
            );
            assert_eq!(s.number(&clip, "out_point"), 6.0, "trimmed from its tail");
        }

        phase a_clip_is_split_and_speed_changes_its_length;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            s.call("split_clip", vec![Value::resource(&p), Value::resource(&clip), Value::Float(4.0)]);
            let clips = s.clips(&p);
            assert_eq!(clips.len(), 2, "one clip became two");
            let right = clips
                .iter()
                .find(|c| (s.number(c, "start") - 4.0).abs() < 1e-6)
                .expect("the second half starts where the cut was");
            assert_eq!(s.number(&clip, "out_point"), 4.0, "the first half ends there");
            assert_eq!(s.number(right, "in_point"), 4.0, "the second reads on from there");
            s.call("set_speed", vec![Value::resource(right), Value::Float(2.0)]);
            assert_eq!(s.number(right, "speed_rate"), 2.0);
        }

        phase a_transition_a_marker_and_the_marks_are_kept;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            s.call(
                "set_transition",
                vec![
                    Value::resource(&clip),
                    Value::text("dissolve"),
                    Value::Float(0.6),
                ],
            );
            assert_eq!(s.text(&clip, "transition"), "dissolve");
            s.call("add_marker", vec![Value::resource(&p), Value::Float(2.0)]);
            let markers = s.records(&p, "markers");
            assert_eq!(markers.len(), 1);
            assert_eq!(s.number(&markers[0], "t"), 2.0);
            s.call("mark_in", vec![Value::resource(&p), Value::Float(1.0)]);
            s.call("mark_out", vec![Value::resource(&p), Value::Float(5.0)]);
            assert_eq!(s.number(&p, "mark_in"), 1.0);
            assert_eq!(s.number(&p, "mark_out"), 5.0);
            s.call("clear_marks", vec![Value::resource(&p)]);
            assert_eq!(s.number(&p, "mark_in"), -1.0);
        }

        phase a_title_is_words_the_cut_draws;
        {
            let s = Studio::new();
            let p = s.new_project();
            returned_ref(s.call(
                "add_title",
                vec![Value::resource(&p), Value::text("Harbour"), Value::Float(0.0)],
            ));
            let media = s.media(&p);
            assert_eq!(media.len(), 1);
            assert_eq!(s.text(&media[0], "words"), "Harbour");
            assert_eq!(s.clips(&p).len(), 1, "the title is placed too");
            let input = s.engine.last_input();
            assert!(input.is_empty(), "a title needs no engine work: {input}");
        }

        phase a_render_runs_as_a_job_whose_progress_is_asked;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            s.call(
                "export_cut",
                vec![
                    Value::resource(&p),
                    Value::text("1080"),
                    Value::text("mp4"),
                    Value::Int(30),
                ],
            );
            assert_eq!(s.text(&p, "job"), "job-1");
            let started = s.engine.last_input();
            let ask: serde_json::Value = serde_json::from_str(&started).unwrap();
            assert_eq!(ask["resolution"], "1080");
            assert_eq!(ask["container"], "mp4");
            let cut: serde_json::Value = serde_json::from_str(ask["project"].as_str().unwrap()).unwrap();
            assert_eq!(cut["name"], "Untitled project");
            s.call("poll_render", vec![Value::resource(&p)]);
            assert_eq!(s.number(&p, "job_done"), 120.0, "how far it has got");
            *s.engine.ready.borrow_mut() = true;
            s.call("poll_render", vec![Value::resource(&p)]);
            assert_eq!(s.text(&p, "exported"), FILM);
            assert_eq!(s.text(&p, "job"), "", "the job is over");
        }

        phase a_preview_render_is_the_cut_small_and_can_be_stopped;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            s.call("preview_cut", vec![Value::resource(&p)]);
            assert_eq!(s.text(&p, "job_kind"), "preview");
            let ask: serde_json::Value = serde_json::from_str(&s.engine.last_input()).unwrap();
            assert_eq!(ask["resolution"], "720");
            s.call("cancel_render", vec![Value::resource(&p)]);
            assert_eq!(s.text(&p, "job"), "");
        }

        phase a_projects_sources_and_their_places_are_a_file_a_person_keeps;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            let file = match s.call("save_project", vec![Value::resource(&p)]) {
                InvokeOutcome::Done(Value::File(file)) => (*file).clone(),
                other => panic!("save_project answers a file: {other:?}"),
            };
            assert_eq!(file.name, "Untitled project.tlvp");
            let bytes = s.held(&file);
            assert!(bytes.contains("Harbour.mp4"), "the file holds the cut: {bytes}");
            let opened = returned_ref(s.call(
                "open_project_file",
                vec![Value::resource(&s.root), Value::file(file)],
            ));
            assert_eq!(s.text(&opened, "name"), "Untitled project");
            assert_eq!(s.media(&opened).len(), 1, "its sources came with it");
            assert_eq!(s.clips(&opened).len(), 1, "and their places");
        }

        phase keys_colours_filmstrips_and_notes_are_a_clip_s_own;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            s.call(
                "key_at",
                vec![
                    Value::resource(&clip),
                    Value::Float(0.0),
                    Value::text("opacity"),
                    Value::text("ease"),
                ],
            );
            let keys = s.records(&clip, "keys");
            assert_eq!(keys.len(), 1, "one key on the clip");
            assert_eq!(s.text(&keys[0], "param"), "opacity");
            assert_eq!(s.text(&keys[0], "interp"), "ease");
            assert_eq!(s.number(&keys[0], "v"), 1.0, "the value the field held");
            // A sample from the frame becomes the colour the key removes.
            s.call(
                "sample_key",
                vec![
                    Value::resource(&p),
                    Value::resource(&clip),
                    Value::Float(0.0),
                    Value::Float(0.4),
                    Value::Float(0.6),
                ],
            );
            assert_eq!(s.number(&clip, "key_red"), 0.25);
            assert_eq!(s.number(&clip, "key_green"), 0.5);
            assert!(matches!(s.rt.read(&clip, "key_enabled").unwrap(), Value::Bool(true)));
            // The filmstrip: eight frames of the clip, kept.
            s.call("filmstrip", vec![Value::resource(&p), Value::resource(&clip)]);
            match s.rt.read(&clip, "strips").unwrap() {
                Value::List(strips) => {
                    assert_eq!(strips.len(), 8);
                    assert_eq!(strips[0], Value::text(SWATCH));
                }
                other => panic!("strips read {other:?}"),
            }
            // A marker's note is its own words.
            s.call("add_marker", vec![Value::resource(&p), Value::Float(2.0)]);
            let marker = s.records(&p, "markers").into_iter().next().expect("a marker");
            s.call(
                "note_marker",
                vec![Value::resource(&marker), Value::text("the ferry")],
            );
            assert_eq!(s.text(&marker, "note"), "the ferry");
        }

        phase the_window_draws_the_bin_the_timeline_and_the_inspector;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let window = s.window();
            let tree = window.tree();
            let drawings = of_kind(tree, "ui.Drawing");
            assert_eq!(drawings.len(), 2, "the ruler over the tracks:\n{}", dump(tree));
            let shapes = |drawing: &UiNode| match drawing.props.get("shapes") {
                Some(Value::List(shapes)) => shapes.len(),
                other => panic!("a drawing holds shapes, not {other:?}"),
            };
            assert!(shapes(drawings[0]) >= 20, "the ruler's ticks and the playhead");
            assert!(
                shapes(drawings[1]) >= 8,
                "the tracks, the clip and its waveform: {}",
                shapes(drawings[1])
            );
            assert!(
                of_kind(tree, "ui.Image").iter().any(|image| {
                    image.props.get("src").and_then(Value::as_text) == Some(STILL)
                }),
                "the preview is the frame the engine drew:\n{}",
                dump(tree)
            );
            let rows = of_kind(tree, "ui.ListRow")
                .iter()
                .filter_map(|r| r.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert!(rows.iter().any(|r| r == "Harbour.mp4"), "the bin lists it: {rows:?}");
        }

        phase pressing_a_clip_opens_what_its_place_shows;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            let window = s.window();
            let track_drawing = of_kind(window.tree(), "ui.Drawing").remove(1);
            // The clip's body is the fourth shape: the ground, V1's name, its line, then the clip.
            let at = window.fire_with(
                &track_drawing.key,
                "press",
                Some(&Value::Int(3)),
            );
            assert!(matches!(at, Ok(InvokeOutcome::Done(_))), "{at:?}");
            let after = s.window();
            assert!(
                of_kind(after.tree(), "ui.TextInput")
                    .iter()
                    .any(|f| f.props.get("placeholder").and_then(Value::as_text) == Some("Name")),
                "the inspector shows the clip that was pressed:\n{}",
                dump(after.tree())
            );
        }
    }

}
