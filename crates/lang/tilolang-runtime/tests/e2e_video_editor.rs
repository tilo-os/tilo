//! The Video Editor (`apps/tl/video-editor/app.tl`) end to end: a source probed and a clip
//! placed, trimmed, split and sped up through its actions; the frame the engine draws, the cut
//! it renders and the file it makes; and its window, with the bin, the timeline and the
//! inspector.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use std::collections::HashMap;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::host_work::{EngineHost, JobHandlers, Push};
    use tilolang_runtime::{
        AmStore, EnumValue, Held, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/video-editor/app.tl");
    /// The session the editor keeps for its meter, so the `use` resolves.
    const PLAYBACK: &str = include_str!("../../../../apps/tl/playback/app.tl");

    const STILL: &str = "tilo-blob://still1";
    const SWATCH: &str = "tilo-blob://swatch1";
    const FILM: &str = "tilo-blob://film1";

    /// The device's engine: what it was asked, and the answers it gives.
    /// A stream the device was handed: its job, and where the test says the play has got to.
    type Stream = (String, std::sync::Arc<dyn Push>);

    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// The job's state as the next ask answers it: `running` until it is `done`.
        ready: Rc<RefCell<bool>>,
        /// What this device cannot read: an ask whose input names one of these is refused as
        /// bytes it does not hold.
        unreadable: Rc<RefCell<Vec<String>>>,
        /// What each start named to run as its job moves and ends.
        handlers: Rc<RefCell<Vec<JobHandlers>>>,
        /// What each stream was handed: where a test says the play has got to.
        streams: Rc<RefCell<Vec<Stream>>>,
        /// The jobs a stream started, in order.
        live: Rc<RefCell<Vec<String>>>,
        /// What this device measures the cut's mix at, when a test says it clips: how far over
        /// full scale, and where.
        over: Rc<RefCell<Option<(f64, Vec<f64>)>>>,
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

        /// The input of the last ask of `op`.
        fn input_of(&self, op: &str) -> String {
            self.asked
                .borrow()
                .iter()
                .rev()
                .find(|(asked, _)| asked == op)
                .map_or_else(
                    || panic!("the device was never asked `{op}`"),
                    |(_, input)| input.clone(),
                )
        }

        /// Says the live play reached `at`, as the device pushes it with every few frames.
        fn reached(&self, job: &str, at: f64) {
            self.heard(job, at, 0.0, &[]);
        }

        /// Says the live play reached `at`, having heard the mix go `over_db` over full scale at
        /// `overs`: what the device pushes as it plays.
        fn heard(&self, job: &str, at: f64, over_db: f64, overs: &[f64]) {
            let push = self
                .streams
                .borrow()
                .iter()
                .find(|(started, _)| started == job)
                .map(|(_, push)| std::sync::Arc::clone(push))
                .expect("the stream the job names");
            let word = serde_json::json!({
                "at": at,
                "over_db": over_db,
                "overs": overs,
                "tracks": [],
            });
            push.state(job, &word.to_string());
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if self
                .unreadable
                .borrow()
                .iter()
                .any(|name| input.contains(name.as_str()))
            {
                return Err("that source's bytes are not on this device yet".to_owned());
            }
            match op {
                "probe" => Ok(r#"{"kind":"video","duration":10.0,"width":1920,"height":1080,"fps":30.0,"has_audio":true,"codec":"h264"}"#.to_owned()),
                "still" => Ok(format!(r#"{{"url":"{STILL}"}}"#)),
                "thumb" => Ok(format!(r#"{{"url":"{SWATCH}"}}"#)),
                "sample" => Ok(r#"{"red":0.25,"green":0.5,"blue":0.75}"#.to_owned()),
                "scrub" | "cut" => Ok(String::new()),
                "waveform" => Ok(r#"{"peaks":[0.2,0.6,0.4,0.9]}"#.to_owned()),
                "mix" => {
                    let said = self.over.borrow().clone();
                    let (over_db, overs) = said.unwrap_or((0.0, Vec::new()));
                    Ok(serde_json::json!({
                        "over_db": over_db,
                        "overs": overs,
                        "tracks": [],
                    })
                    .to_string())
                }
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op == "render" {
                self.handlers.borrow_mut().push(handlers.clone());
                Ok("job-1".to_owned())
            } else {
                Err(format!("`{op}` is no op this device runs"))
            }
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                if job != "job-1" {
                    return Err(format!("no job `{job}`"));
                }
                if *self.ready.borrow() {
                    Ok(format!(
                        r#"{{"state":"done","done":300,"total":300,"url":"{FILM}","error":""}}"#
                    ))
                } else {
                    Ok(
                        r#"{"state":"running","done":120,"total":300,"url":"","error":""}"#
                            .to_owned(),
                    )
                }
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn stream(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            push: std::sync::Arc<dyn Push>,
        ) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            if op != "play" {
                return Err(format!("`{op}` is no stream this device runs"));
            }
            let job = format!("live-{}", self.live.borrow().len().saturating_add(1));
            self.live.borrow_mut().push(job.clone());
            self.streams.borrow_mut().push((job.clone(), push));
            Ok(format!(r#"{{"job":"{job}"}}"#))
        }

        fn cancel(&self, job: &str) -> bool {
            job == "job-1" || job.starts_with("live-")
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
            String::from_utf8_lossy(
                &self
                    .device
                    .0
                    .borrow()
                    .get(&file.hash)
                    .cloned()
                    .unwrap_or_default(),
            )
            .into_owned()
        }
    }

    /// The media type and bytes of what the person copied.
    type Copied = Option<(String, Vec<u8>)>;

    /// The person's clipboard, as the device answers it.
    #[derive(Clone, Default)]
    struct Selection(Rc<RefCell<Copied>>);

    impl ClipboardHost for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
            let held = self.0.borrow().clone()?;
            types
                .iter()
                .any(|kind| held.0.starts_with(kind))
                .then_some(held)
        }
    }

    impl Studio {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let device = Device::default();
            rt.set_blobs(device.clone());
            rt.set_clipboard(Selection::default());
            let playback = App::compile(PLAYBACK, &[]);
            playback.load(&rt);
            let app = App::compile(SOURCE, &[playback.meta().clone()]);
            // The app's code is trusted and the clipboard granted to it, as a person's press of
            // Copy and Paste in its window grants it.
            authority.trust(&app.load(&rt));
            for action in ["copy_clips", "cut_clips", "paste_clips"] {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Editor").unwrap();
            Self {
                rt,
                app,
                root,
                engine,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            self.try_call(action, args).unwrap()
        }

        /// A call that may be refused, which a `require` that does not hold is.
        fn try_call(
            &self,
            action: &str,
            args: Vec<Value>,
        ) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args))
        }

        /// A source a person chose, as the device hands it over.
        fn file(name: &str) -> Value {
            Value::file(FileValue::new(name, "video/mp4", 1024, format!("h-{name}")))
        }

        fn new_project(&self) -> ResourceRef {
            returned_ref(self.call("new_project", vec![Value::resource(&self.root)]))
        }

        fn import(&self, p: &ResourceRef, name: &str) {
            self.call("import_one", vec![Value::resource(p), Studio::file(name)]);
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

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// A `List<Float>` field of a record, as numbers.
        fn numbers(&self, r: &ResourceRef, field: &str) -> Vec<f64> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Float(n) => n,
                        other => panic!("{field} holds {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The bin's second line for the row of the source called `title`.
        fn bin_meta(&self, title: &str) -> Option<String> {
            let window = self.window();
            of_kind(window.tree(), "ui.ListRow")
                .into_iter()
                .find(|row| row.props.get("title").and_then(Value::as_text) == Some(title))
                .and_then(|row| row.props.get("meta").and_then(Value::as_text))
                .map(str::to_owned)
        }

        fn number(&self, r: &ResourceRef, field: &str) -> f64 {
            match self.rt.read(r, field).unwrap() {
                Value::Float(n) => n,
                Value::Int(n) => f64::from(i32::try_from(n).unwrap_or(0)),
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The job the device started last moved on, as `record` says it stands: the handler the
        /// start named, filled with the job, invoked as the person who started it.
        fn moves(
            &self,
            handler: impl Fn(&JobHandlers) -> Option<serde_json::Value>,
            record: &serde_json::Value,
        ) {
            let kept = handler(
                self.engine
                    .handlers
                    .borrow()
                    .last()
                    .expect("the device started a job"),
            )
            .expect("the start named this handler");
            let call = self.rt.job_call(&kept, record).unwrap();
            self.rt.invoke_action(&me(), call).unwrap();
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("window"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// A cut with one source on the video track at zero.
        fn placed(&self) -> (ResourceRef, ResourceRef, ResourceRef) {
            let p = self.new_project();
            self.import(&p, "Harbour.mp4");
            let media = self.media(&p).into_iter().next().expect("a source");
            let track = self
                .tracks(&p)
                .into_iter()
                .find(|t| self.text(t, "name") == "V1")
                .expect("V1");
            self.place(&p, &media, &track, 0.0);
            let clip = self.clips(&p).into_iter().next().expect("a clip");
            (p, media, clip)
        }
    }

    /// A title placed on the cut's video track, as `add_title` makes it.
    fn a_title(s: &Studio, p: &ResourceRef) -> ResourceRef {
        let made = s.call(
            "add_title",
            vec![
                Value::resource(p),
                Value::text("Harbour"),
                Value::Float(0.0),
            ],
        );
        if let InvokeOutcome::Done(Value::Ref(r)) = made {
            return std::rc::Rc::unwrap_or_clone(r.target);
        }
        panic!("a title is not placed: {made:?}");
    }

    /// The cut as the device read it, from the last ask of `op`.
    fn cut_of(s: &Studio, op: &str) -> serde_json::Value {
        let ask: serde_json::Value = serde_json::from_str(&s.engine.input_of(op)).expect("the ask");
        serde_json::from_str(ask["project"].as_str().expect("the cut")).expect("the cut reads")
    }

    /// A number a window keeps as its own state, whatever the state's slot wraps it in.
    fn number_state(window: &ViewInstance, name: &str) -> Option<f64> {
        match window.view_state(name) {
            Some(Value::Float(n)) => Some(n),
            Some(Value::Opt(Some(inner))) => match *inner {
                Value::Float(n) => Some(n),
                _ => None,
            },
            _ => None,
        }
    }

    /// The number a JSON object holds under `key`.
    fn json_number(value: &serde_json::Value, key: &str) -> f64 {
        value[key]
            .as_f64()
            .unwrap_or_else(|| panic!("{key} is {value:?}"))
    }

    /// A `Look` case, as the inspector's sliders name it.
    fn look_case(s: &Studio, case: &str) -> Value {
        Value::Enum(EnumValue {
            ty: s.app.compiled.ids.get("Look").expect("Look").to_owned(),
            variant: s
                .app
                .compiled
                .ids
                .get(&format!("Look.{case}"))
                .unwrap_or_else(|| panic!("Look.{case}"))
                .to_owned(),
            fields: std::collections::BTreeMap::default(),
        })
    }

    tilo_test_env::scenario! {
        fn the_editor_cuts_a_film_and_shows_it;

        phase a_new_cut_starts_with_a_video_and_a_sound_track;
        {
            let s = Studio::new();
            let p = s.new_project();
            assert_eq!(s.text(&p, "name"), "Untitled project");
            assert_eq!(s.tracks(&p).len(), 2);
            assert!((s.number(&p, "fps") - 30.0).abs() < f64::EPSILON, "the shape a cut starts at");
            assert!((s.number(&p, "width") - 1920.0).abs() < f64::EPSILON);
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
            assert!((s.number(&media[0], "duration") - 10.0).abs() < f64::EPSILON);
            assert!((s.number(&media[0], "width") - 1920.0).abs() < f64::EPSILON);
            assert_eq!(
                s.text(&media[0], "still"),
                STILL,
                "the bin's swatch is the engine's picture"
            );
            match s.rt.read(&media[0], "peaks").unwrap() {
                Value::List(peaks) => assert_eq!(peaks.len(), 4, "the waveform reached the source"),
                other => panic!("peaks read {other:?}"),
            }
            assert!((s.number(&p, "width") - 1920.0).abs() < f64::EPSILON, "a cut takes its shape from its first source");
        }

        phase a_source_placed_becomes_a_clip_at_a_place_on_its_track;
        {
            let s = Studio::new();
            let (p, media, clip) = s.placed();
            assert_eq!(s.clips(&p).len(), 1);
            assert_eq!(s.text(&clip, "name"), "Harbour.mp4");
            assert!((s.number(&clip, "start") - 0.0).abs() < f64::EPSILON);
            assert!((s.number(&clip, "out_point") - 10.0).abs() < f64::EPSILON);
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
                    Value::List(Vec::new()),
                ],
            );
            assert!((s.number(&clip, "start") - 3.0).abs() < f64::EPSILON, "moved along the timeline");
            assert!((s.number(&clip, "in_point") - 0.0).abs() < f64::EPSILON, "its source stayed put");
            s.call(
                "commit_drag",
                vec![
                    Value::resource(&p),
                    Value::text("tail"),
                    Value::text(id),
                    Value::Float(0.0),
                    Value::Float(-4.0),
                    Value::List(Vec::new()),
                ],
            );
            assert!((s.number(&clip, "out_point") - 6.0).abs() < f64::EPSILON, "trimmed from its tail");
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
            assert!((s.number(&clip, "out_point") - 4.0).abs() < f64::EPSILON, "the first half ends there");
            assert!((s.number(right, "in_point") - 4.0).abs() < f64::EPSILON, "the second reads on from there");
            s.call("set_speed", vec![Value::resource(right), Value::Float(2.0)]);
            assert!((s.number(right, "speed_rate") - 2.0).abs() < f64::EPSILON);
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
            assert!((s.number(&markers[0], "t") - 2.0).abs() < f64::EPSILON);
            s.call("mark_in", vec![Value::resource(&p), Value::Float(1.0)]);
            s.call("mark_out", vec![Value::resource(&p), Value::Float(5.0)]);
            assert!((s.number(&p, "mark_in") - 1.0).abs() < f64::EPSILON);
            assert!((s.number(&p, "mark_out") - 5.0).abs() < f64::EPSILON);
            s.call("clear_marks", vec![Value::resource(&p)]);
            assert!((s.number(&p, "mark_in") - -1.0).abs() < f64::EPSILON);
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
            // The title is drawn from its own words: a title asked the device to probe nothing.
            let ops = s.engine.ops();
            assert!(
                ops.iter().all(|op| op == "still"),
                "a title needs no probe, only the frame it is drawn in: {ops:?}"
            );
        }

        phase a_render_runs_as_a_job_and_its_moves_fill_the_bar;
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
            s.moves(
                |handlers| handlers.progress.clone(),
                &serde_json::json!({
                    "id": "job-1", "op": "render", "state": "running",
                    "detail": {"state": "running", "done": 120, "total": 300, "url": "", "error": ""},
                }),
            );
            assert!((s.number(&p, "job_done") - 120.0).abs() < f64::EPSILON, "how far it has got");
            s.moves(
                |handlers| handlers.done.clone(),
                &serde_json::json!({
                    "id": "job-1", "op": "render", "state": "done",
                    "detail": {"state": "done", "done": 300, "total": 300, "url": FILM, "error": ""},
                }),
            );
            assert_eq!(s.text(&p, "exported"), FILM);
            assert_eq!(s.text(&p, "job"), "", "the job is over");
        }

        phase a_projects_sources_and_their_places_are_a_file_a_person_keeps;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            s.call(
                "set_anchor",
                vec![
                    Value::resource(&clip),
                    Value::Float(0.0),
                    Value::Float(1.0),
                ],
            );
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
            let reopened = s.clips(&opened).into_iter().next().expect("a clip");
            assert!((s.number(&reopened, "anchor_x") - 0.0).abs() < f64::EPSILON, "the anchor came with it");
            assert!((s.number(&reopened, "anchor_y") - 1.0).abs() < f64::EPSILON);
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
            assert!((s.number(&keys[0], "v") - 1.0).abs() < f64::EPSILON, "the value the field held");
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
            assert!((s.number(&clip, "key_red") - 0.25).abs() < f64::EPSILON);
            assert!((s.number(&clip, "key_green") - 0.5).abs() < f64::EPSILON);
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

        phase an_edit_draws_the_frame_the_mask_and_the_mix_are_its_own;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            let look = |case: &str| {
                Value::Enum(EnumValue {
                    ty: s.app.compiled.ids.get("Look").expect("Look").to_owned(),
                    variant: s
                        .app
                        .compiled
                        .ids
                        .get(&format!("Look.{case}"))
                        .unwrap_or_else(|| panic!("Look.{case}"))
                        .to_owned(),
                    fields: std::collections::BTreeMap::default(),
                })
            };
            let stills = || s.engine.ops().iter().filter(|op| *op == "still").count();
            // The preview draws the frame the person looks at.
            let before = stills();
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            assert_eq!(stills(), before + 1, "the frame at the playhead");
            // A look the person changes draws it again: the edit is seen as it is made, not at
            // the next move of the playhead.
            let before = stills();
            s.call(
                "set_look",
                vec![Value::resource(&clip), look("opacity"), Value::Float(0.4)],
            );
            assert!((s.number(&clip, "opacity") - 0.4).abs() < f64::EPSILON);
            assert_eq!(stills(), before + 1, "the frame was drawn again after the edit");
            // The mask is a shape the clip shows through, with its place, size and edge.
            s.call("set_mask", vec![Value::resource(&clip), Value::text("ellipse")]);
            assert_eq!(s.text(&clip, "mask_shape"), "ellipse");
            s.call("invert_mask", vec![Value::resource(&clip), Value::Bool(true)]);
            assert!(matches!(
                s.rt.read(&clip, "mask_invert").unwrap(),
                Value::Bool(true)
            ));
            s.call(
                "set_look",
                vec![Value::resource(&clip), look("mask_feather"), Value::Float(0.3)],
            );
            assert!((s.number(&clip, "mask_feather") - 0.3).abs() < f64::EPSILON);
            // Crop reaches every side, and both scales are the clip's.
            s.call(
                "set_look",
                vec![Value::resource(&clip), look("crop_top"), Value::Float(0.2)],
            );
            s.call(
                "set_look",
                vec![Value::resource(&clip), look("scale_y"), Value::Float(0.5)],
            );
            assert!((s.number(&clip, "crop_top") - 0.2).abs() < f64::EPSILON);
            assert!((s.number(&clip, "scale_y") - 0.5).abs() < f64::EPSILON);
            // The track's mix is its own: level, pan and the switches.
            let track = s
                .tracks(&p)
                .into_iter()
                .find(|t| s.text(t, "name") == "V1")
                .expect("V1");
            s.call(
                "set_track_gain",
                vec![Value::resource(&track), Value::Float(-6.0)],
            );
            s.call(
                "set_track_pan",
                vec![Value::resource(&track), Value::Float(-1.0)],
            );
            s.call(
                "set_track_muted",
                vec![Value::resource(&track), Value::Bool(true)],
            );
            assert!((s.number(&track, "gain_db") - -6.0).abs() < f64::EPSILON);
            assert!((s.number(&track, "pan") - -1.0).abs() < f64::EPSILON);
            assert!(matches!(
                s.rt.read(&track, "muted").unwrap(),
                Value::Bool(true)
            ));
            s.call("reset_mix", vec![Value::resource(&track)]);
            assert!((s.number(&track, "gain_db") - 0.0).abs() < f64::EPSILON);
            assert!((s.number(&track, "pan") - 0.0).abs() < f64::EPSILON);
        }

        phase the_window_draws_the_bin_the_timeline_and_the_inspector;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let window = s.window();
            let tree = window.tree();
            assert_eq!(
                of_kind(tree, "ui.TrackTimeline").len(),
                1,
                "the ruler over the tracks:\n{}",
                dump(tree)
            );
            let rows = of_kind(tree, "ui.TimelineTrack");
            assert_eq!(rows.len(), s.tracks(&p).len(), "a row a track:\n{}", dump(tree));
            assert!(
                rows.iter()
                    .any(|r| r.props.get("label").and_then(Value::as_text) == Some("V1")),
                "each row says its track's name"
            );
            let spans = of_kind(tree, "ui.TimelineSpan");
            assert_eq!(spans.len(), 1, "the clip stands on its track:\n{}", dump(tree));
            assert_eq!(
                spans[0].props.get("item").and_then(Value::as_text),
                Some(clip.id_string().as_str()),
                "the span stands for the clip"
            );
            assert_eq!(spans[0].props.get("start"), Some(&Value::Float(0.0)));
            assert_eq!(
                spans[0].props.get("label").and_then(Value::as_text),
                Some(s.text(&clip, "name").as_str())
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


        phase a_batch_names_the_files_that_did_not_arrive;
        {
            let s = Studio::new();
            let p = s.new_project();
            s.engine
                .unreadable
                .borrow_mut()
                .push("Notes.txt".to_owned());
            s.call(
                "import_files",
                vec![
                    Value::resource(&p),
                    Value::List(vec![
                        Studio::file("Harbour.mp4"),
                        Studio::file("Notes.txt"),
                    ]),
                ],
            );
            assert_eq!(s.media(&p).len(), 1, "the file it could read became a source");
            assert_eq!(
                s.text(&p, "trouble"),
                "Not added: Notes.txt: that source's bytes are not on this device yet",
                "the report names the file and what the device said of it"
            );
            // A batch it could read whole leaves no report behind.
            s.engine.unreadable.borrow_mut().clear();
            s.call(
                "import_files",
                vec![Value::resource(&p), Value::List(vec![Studio::file("Quay.mp4")])],
            );
            assert_eq!(s.text(&p, "trouble"), "", "nothing refused, nothing to say");
            assert_eq!(s.media(&p).len(), 2);
        }

        phase a_source_this_device_cannot_read_is_marked_and_relinked;
        {
            let s = Studio::new();
            let (p, media, clip) = s.placed();
            // The bytes go away: another device holds them now.
            s.engine
                .unreadable
                .borrow_mut()
                .push("Harbour.mp4".to_owned());
            s.call("refresh_source", vec![Value::resource(&p), Value::resource(&media)]);
            assert!(s.flag(&media, "missing"), "the source is marked not on this device");
            assert_eq!(
                s.text(&p, "trouble"),
                "that source's bytes are not on this device yet",
                "and the window says why"
            );
            assert_eq!(
                s.bin_meta("Harbour.mp4").as_deref(),
                Some("Not on this device"),
                "the bin's row says it where the person looks"
            );
            // The person finds the file again and points the source at it.
            s.engine.unreadable.borrow_mut().clear();
            s.call(
                "relink",
                vec![
                    Value::resource(&p),
                    Value::resource(&media),
                    Studio::file("Harbour-found.mp4"),
                ],
            );
            assert!(!s.flag(&media, "missing"), "the source reads again");
            assert_eq!(s.text(&media, "name"), "Harbour-found.mp4");
            assert!((s.number(&media, "duration") - 10.0).abs() < f64::EPSILON, "what its new file says");
            assert_eq!(
                s.bin_meta("Harbour-found.mp4").as_deref(),
                Some("0:10.0"),
                "the row shows its length again"
            );
            // The clip is a cut of the source, not of the file: it keeps the source it had.
            let clip_media = match s.rt.read(&clip, "media").unwrap() {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("media reads {other:?}"),
            };
            assert_eq!(clip_media.id_string(), media.id_string());
        }

        phase the_timeline_moves_trims_and_marks_through_the_window;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            let tracks = s.tracks(&p);
            let track_named = |name: &str| {
                tracks
                    .iter()
                    .find(|t| s.text(t, "name") == name)
                    .unwrap_or_else(|| panic!("{name}"))
                    .id_string()
            };
            let span_key = |window: &ViewInstance| {
                of_kind(window.tree(), "ui.TimelineSpan")
                    .first()
                    .expect("the clip's span")
                    .key
                    .clone()
            };
            // Dragged along V1 to three seconds: the editor commits the drag.
            let window = s.window();
            let moved = window.fire_with(
                &span_key(&window),
                "moved",
                Some(&Value::List(vec![Value::Float(3.0), Value::text(track_named("V1"))])),
            );
            assert!(matches!(moved, Ok(InvokeOutcome::Done(_))), "{moved:?}");
            assert!((s.number(&clip, "start") - 3.0).abs() < f64::EPSILON, "moved along the timeline");
            // Dragged onto a sound track: a picture does not go there, but the time it was let go
            // at still holds.
            let window = s.window();
            let onto_sound = window.fire_with(
                &span_key(&window),
                "moved",
                Some(&Value::List(vec![Value::Float(4.0), Value::text(track_named("A1"))])),
            );
            assert!(matches!(onto_sound, Ok(InvokeOutcome::Done(_))), "{onto_sound:?}");
            assert!((s.number(&clip, "start") - 4.0).abs() < f64::EPSILON);
            let on_track = match s.rt.read(&clip, "track").unwrap() {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target).id_string(),
                other => panic!("track reads {other:?}"),
            };
            assert_eq!(on_track, track_named("V1"), "a picture stays on a picture track");
            // Its trailing edge dragged in: four seconds of it are left.
            let window = s.window();
            let trimmed = window.fire_with(
                &span_key(&window),
                "trimmed",
                Some(&Value::List(vec![Value::Float(4.0), Value::Float(4.0)])),
            );
            assert!(matches!(trimmed, Ok(InvokeOutcome::Done(_))), "{trimmed:?}");
            assert!((s.number(&clip, "out_point") - 4.0).abs() < f64::EPSILON, "trimmed from its tail");
            assert!((s.number(&clip, "start") - 4.0).abs() < f64::EPSILON, "its start stayed");
            // A marker dragged along the ruler lands on the frame nearest where it was let go.
            s.call("add_marker", vec![Value::resource(&p), Value::Float(1.0)]);
            let window = s.window();
            let marker = of_kind(window.tree(), "ui.TimelineMarker")
                .first()
                .expect("the marker stands on the ruler")
                .key
                .clone();
            let dragged = window.fire_with(&marker, "moved", Some(&Value::Float(2.5)));
            assert!(matches!(dragged, Ok(InvokeOutcome::Done(_))), "{dragged:?}");
            let marked = s.records(&p, "markers");
            assert!((s.number(&marked[0], "t") - 2.5).abs() < f64::EPSILON);
        }

        phase a_clip_turns_around_its_anchor;
        {
            let s = Studio::new();
            let (p, _media, clip) = s.placed();
            assert!((s.number(&clip, "anchor_x") - 0.5).abs() < f64::EPSILON, "a clip starts pinned at its middle");
            assert!((s.number(&clip, "anchor_y") - 0.5).abs() < f64::EPSILON);
            s.call(
                "set_anchor",
                vec![
                    Value::resource(&clip),
                    Value::Float(0.0),
                    Value::Float(1.0),
                ],
            );
            assert!((s.number(&clip, "anchor_x") - 0.0).abs() < f64::EPSILON);
            assert!((s.number(&clip, "anchor_y") - 1.0).abs() < f64::EPSILON);
            // The engine draws the frame turned around the point the clip names.
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let ask: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("still")).expect("the still's ask");
            let cut: serde_json::Value =
                serde_json::from_str(ask["project"].as_str().expect("a cut")).expect("a cut");
            let placed: serde_json::Value =
                serde_json::from_str(cut["clips"][0].as_str().expect("a clip")).expect("a clip");
            assert_eq!(placed["anchor_x"], 0.0);
            assert_eq!(placed["anchor_y"], 1.0);
            // Two clips side by side hang each from the edge it takes, so what it shows fills
            // its half: the anchor is what makes that a half-width scale about an edge rather
            // than about the middle.
            for (side, anchor) in [(-0.25, 0.0), (0.25, 1.0)] {
                s.call(
                    "set_anchor",
                    vec![
                        Value::resource(&clip),
                        Value::Float(anchor),
                        Value::Float(0.5),
                    ],
                );
                s.call(
                    "set_look",
                    vec![
                        Value::resource(&clip),
                        look_case(&s, "scale_x"),
                        Value::Float(0.5),
                    ],
                );
                s.call(
                    "set_look",
                    vec![
                        Value::resource(&clip),
                        look_case(&s, "pos_x"),
                        Value::Float(side),
                    ],
                );
                assert!((s.number(&clip, "anchor_x") - anchor).abs() < f64::EPSILON);
                assert!((s.number(&clip, "pos_x") - side).abs() < f64::EPSILON);
            }
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let ask: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("still")).expect("the still's ask");
            let cut: serde_json::Value =
                serde_json::from_str(ask["project"].as_str().expect("a cut")).expect("a cut");
            let placed: serde_json::Value =
                serde_json::from_str(cut["clips"][0].as_str().expect("a clip")).expect("a clip");
            assert_eq!(placed["scale_x"], 0.5);
            assert_eq!(placed["anchor_x"], 1.0, "half the frame, hung from the edge it takes");
        }

        phase a_cut_plays_live_and_stops_where_it_is_paused;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            s.call(
                "play_cut",
                vec![
                    Value::resource(&p),
                    Value::Float(2.0),
                    Value::Float(1.0),
                ],
            );
            assert_eq!(s.text(&p, "live"), "live-1");
            let ask: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("play")).expect("the play's ask");
            assert_eq!(ask["from"], 2.0, "it plays from the playhead");
            assert_eq!(ask["rate"], 1.0, "at normal speed");
            assert_eq!(ask["sound"], true, "and the device plays its sound");
            assert_eq!(ask["width"], 640, "a frame the window can show at once");
            // The window shows the stream's own frame, not a render of the cut.
            s.engine.reached("live-1", 5.0);
            let window = s.window();
            assert!(
                of_kind(window.tree(), "ui.Image").iter().any(|image| {
                    image.props.get("src").and_then(Value::as_text)
                        == Some("tilo-live://live-1")
                }),
                "the preview is the live play:\n{}",
                dump(window.tree())
            );
            assert!(
                of_kind(window.tree(), "ui.Text").iter().any(|text| {
                    text.props.get("value").and_then(Value::as_text) == Some("0:05.0")
                }),
                "the transport reads where the play has got:\n{}",
                dump(window.tree())
            );
            // Pause stops the device's play and stands on the frame it reached.
            s.call("pause_cut", vec![Value::resource(&p)]);
            assert_eq!(s.text(&p, "live"), "", "the play is over");
            let still: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("still")).expect("the still's ask");
            assert_eq!(still["at"], 5.0, "the frame it stopped on");
        }

        phase the_transport_shuttles_and_runs_backwards;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            let rate = || -> f64 {
                let ask: serde_json::Value =
                    serde_json::from_str(&s.engine.input_of("play")).expect("the play's ask");
                ask["rate"].as_f64().unwrap_or_default()
            };
            s.call(
                "shuttle",
                vec![Value::resource(&p), Value::Float(0.0), Value::Float(1.0)],
            );
            assert!((rate() - 1.0).abs() < f64::EPSILON);
            assert!((s.number(&p, "live_rate") - 1.0).abs() < f64::EPSILON);
            s.call(
                "shuttle",
                vec![Value::resource(&p), Value::Float(0.0), Value::Float(1.0)],
            );
            assert!((rate() - 2.0).abs() < f64::EPSILON, "the same way again doubles it");
            s.call(
                "shuttle",
                vec![Value::resource(&p), Value::Float(0.0), Value::Float(1.0)],
            );
            assert!((rate() - 4.0).abs() < f64::EPSILON);
            s.call(
                "shuttle",
                vec![Value::resource(&p), Value::Float(3.0), Value::Float(-1.0)],
            );
            assert!((rate() - -1.0).abs() < f64::EPSILON, "the other way starts again at normal speed, backwards");
            assert!((s.number(&p, "live_rate") - -1.0).abs() < f64::EPSILON);
        }

        phase a_scrub_draws_the_frame_and_plays_the_sound_there;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            s.call(
                "play_cut",
                vec![Value::resource(&p), Value::Float(0.0), Value::Float(1.0)],
            );
            // Dragged to the tenth second.
            s.call(
                "scrub_to",
                vec![Value::resource(&p), Value::Float(10.0)],
            );
            assert_eq!(s.text(&p, "live"), "", "dragging the playhead stops the play");
            let scrub: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("scrub")).expect("the scrub's ask");
            assert_eq!(scrub["at"], 10.0, "the sound where the playhead was dragged");
            let still: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("still")).expect("the still's ask");
            assert_eq!(still["at"], 10.0, "and the frame under it");
        }

        phase a_film_whose_footage_is_missing_is_refused;
        {
            let s = Studio::new();
            let (p, media, _) = s.placed();
            s.engine
                .unreadable
                .borrow_mut()
                .push("Harbour.mp4".to_owned());
            s.call(
                "refresh_source",
                vec![Value::resource(&p), Value::resource(&media)],
            );
            assert!(s.flag(&media, "missing"));
            let renders = || {
                s.engine
                    .ops()
                    .iter()
                    .filter(|op| *op == "render")
                    .count()
            };
            let before = renders();
            s.call(
                "export_cut",
                vec![
                    Value::resource(&p),
                    Value::text("1080"),
                    Value::text("mp4"),
                    Value::Int(30),
                ],
            );
            assert_eq!(renders(), before, "no render is started");
            assert_eq!(s.text(&p, "job"), "");
            assert!(
                s.text(&p, "trouble").contains("Harbour.mp4"),
                "the refusal names the source: {:?}",
                s.text(&p, "trouble")
            );
            // The export controls say it before the person tries.
            let window = s.window();
            assert!(
                of_kind(window.tree(), "ui.Banner").iter().any(|banner| {
                    banner
                        .props
                        .get("message")
                        .and_then(Value::as_text)
                        .is_some_and(|message| message.contains("Harbour.mp4"))
                }),
                "the export section names the source it lacks:\n{}",
                dump(window.tree())
            );
            // Relinking the source lets the film be made.
            s.engine.unreadable.borrow_mut().clear();
            s.call(
                "relink",
                vec![
                    Value::resource(&p),
                    Value::resource(&media),
                    Studio::file("Harbour-found.mp4"),
                ],
            );
            s.call(
                "export_cut",
                vec![
                    Value::resource(&p),
                    Value::text("1080"),
                    Value::text("mp4"),
                    Value::Int(30),
                ],
            );
            assert_eq!(renders(), before + 1, "the film is rendered once the source is here");
            assert_eq!(s.text(&p, "job"), "job-1");
        }

        phase pressing_a_clip_opens_what_its_place_shows;
        {
            let s = Studio::new();
            let _ = s.placed();
            let window = s.window();
            let span = of_kind(window.tree(), "ui.TimelineSpan").remove(0).key.clone();
            let at = window.fire_with(&span, "pressed", Some(&Value::Int(0)));
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

        phase the_timeline_chooses_many_and_the_clipboard_carries_them;
        {
            let s = Studio::new();
            let (p, media, first) = s.placed();
            let track = s.tracks(&p).into_iter().find(|t| s.text(t, "name") == "V1").unwrap();
            s.place(&p, &media, &track, 12.0);
            let second = s
                .clips(&p)
                .into_iter()
                .find(|c| c.id_string() != first.id_string())
                .expect("a second clip");
            // Each clip's span, in the order the clips start.
            let spans = |window: &ViewInstance| {
                let mut spans = of_kind(window.tree(), "ui.TimelineSpan")
                    .into_iter()
                    .map(|span| {
                        let start = match span.props.get("start") {
                            Some(Value::Float(t)) => *t,
                            other => panic!("a span starts at {other:?}"),
                        };
                        (start, span.key.clone())
                    })
                    .collect::<Vec<_>>();
                spans.sort_by(|a, b| a.0.total_cmp(&b.0));
                spans.into_iter().map(|(_, key)| key).collect::<Vec<_>>()
            };
            assert_eq!(spans(&s.window()).len(), 2, "two clips on the timeline");
            let press = |nth: usize, held: Held| {
                let window = s.window();
                let key = spans(&window).remove(nth);
                let done = window.fire_held(&key, "pressed", Some(&Value::Int(0)), None, held);
                assert!(matches!(done, Ok(InvokeOutcome::Done(_))), "{done:?}");
            };
            let selected = || match s.window().view_state("selected") {
                Some(Value::List(ids)) => ids
                    .into_iter()
                    .filter_map(|id| id.as_text().map(str::to_owned))
                    .collect::<Vec<_>>(),
                other => panic!("the selection reads {other:?}"),
            };
            press(0, Held::default());
            assert_eq!(selected(), vec![first.id_string()], "a press chooses one clip");
            press(1, Held { shift: true, ..Held::default() });
            assert_eq!(
                selected(),
                vec![first.id_string(), second.id_string()],
                "Shift adds the next one"
            );
            press(0, Held { command: true, ..Held::default() });
            assert_eq!(selected(), vec![second.id_string()], "the command key takes one back out");
            press(0, Held { command: true, ..Held::default() });

            // The Edit menu acts on both: a copy carries them, a paste puts them at the playhead.
            let both = Value::List(vec![Value::resource(&first), Value::resource(&second)]);
            let brightness = Value::Enum(EnumValue {
                ty: s.app.compiled.ids.get("Look").expect("Look").to_owned(),
                variant: s.app.compiled.ids.get("Look.brightness").expect("brightness").to_owned(),
                fields: std::collections::BTreeMap::default(),
            });
            s.call("set_look", vec![Value::resource(&first), brightness, Value::Float(0.8)]);
            s.call("add_effect", vec![Value::resource(&first), Value::text("sepia")]);
            let copied = s.call("copy_clips", vec![Value::resource(&p), both.clone()]);
            assert!(matches!(copied, InvokeOutcome::Done(_)), "the copy ran: {copied:?}");
            s.call("paste_clips", vec![Value::resource(&p), Value::Float(40.0)]);
            let clips = s.clips(&p);
            assert_eq!(clips.len(), 4, "the paste made two clips: {}", s.text(&p, "trouble"));
            let mut starts: Vec<f64> = clips.iter().map(|c| s.number(c, "start")).collect();
            starts.sort_by(f64::total_cmp);
            assert_eq!(starts, vec![0.0, 12.0, 40.0, 52.0], "keeping their distance from the playhead");
            let pasted = clips
                .iter()
                .find(|c| (s.number(c, "start") - 40.0).abs() < f64::EPSILON)
                .expect("the pasted first clip");
            assert!((s.number(pasted, "brightness") - 0.8).abs() < f64::EPSILON, "with its look");
            assert_eq!(s.records(pasted, "effects").len(), 1, "and its effects");

            // A nudge moves them together, a second's frames on; duplicate puts a copy right
            // after them; delete takes them away.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a cut's frame rate is far inside i64"
            )]
            let second_of_frames = s.number(&p, "fps").round() as i64;
            s.call("nudge", vec![Value::resource(&p), both.clone(), Value::Int(second_of_frames)]);
            assert!((s.number(&first, "start") - 1.0).abs() < f64::EPSILON, "a second later");
            assert!((s.number(&second, "start") - 13.0).abs() < f64::EPSILON, "and the other with it");
            s.call("duplicate_clips", vec![Value::resource(&p), both.clone()]);
            assert_eq!(s.clips(&p).len(), 6);
            assert!(
                s.clips(&p)
                    .iter()
                    .any(|c| (s.number(c, "start") - 23.0).abs() < f64::EPSILON),
                "the copy of the first starts where the second ended"
            );
            s.call("remove_clips", vec![Value::resource(&p), both]);
            assert_eq!(s.clips(&p).len(), 4);
        }
    }

    tilo_test_env::scenario! {
        fn a_mark_is_renamed_and_keeps_a_name;

        phase a_mark_takes_a_new_name;
        {
            let s = Studio::new();
            let p = s.new_project();
            let made = s.call("add_marker", vec![Value::resource(&p), Value::Float(3.0)]);
            let marker = match made {
                InvokeOutcome::Done(Value::Ref(r)) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("a marker is not made: {other:?}"),
            };
            assert_eq!(s.text(&marker, "name"), "Marker 1");
            s.call("rename_marker", vec![Value::resource(&marker), Value::text("  Beat  ")]);
            assert_eq!(s.text(&marker, "name"), "Beat", "trimmed, and the name is the mark's");
        }

        phase a_mark_is_not_left_with_no_name;
        {
            let s = Studio::new();
            let p = s.new_project();
            let made = s.call("add_marker", vec![Value::resource(&p), Value::Float(3.0)]);
            let marker = match made {
                InvokeOutcome::Done(Value::Ref(r)) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("a marker is not made: {other:?}"),
            };
            let blank = s.try_call("rename_marker", vec![Value::resource(&marker), Value::text("   ")]);
            assert!(blank.is_err(), "a blank name is refused: {blank:?}");
            assert_eq!(
                s.text(&marker, "name"),
                "Marker 1",
                "so the mark keeps the name it had: a mark nobody can find is not a marker"
            );
        }

        phase the_marker_list_types_the_name_where_it_stands;
        {
            let s = Studio::new();
            let p = s.new_project();
            s.call("add_marker", vec![Value::resource(&p), Value::Float(3.0)]);
            let window = s.window();
            let label = of_kind(window.tree(), "ui.EditableLabel")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the mark's name is edited in place:\n{}", dump(window.tree())));
            assert!(
                match label.props.get("value") {
                    Some(Value::Text(name)) => name == "Marker 1",
                    other => other.is_none(),
                },
                "it shows the name the mark has: {:?}",
                label.props.get("value")
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_picture_fills_the_frame_and_a_mask_fits_it;

        phase filling_the_frame_takes_every_crop_scale_turn_and_move_back;
        {
            let s = Studio::new();
            let (_, _, clip) = s.placed();
            let scale = look_case(&s, "scale_x");
            s.call("set_look", vec![Value::resource(&clip), scale, Value::Float(0.5)]);
            let crop = look_case(&s, "crop_left");
            s.call("set_look", vec![Value::resource(&clip), crop, Value::Float(0.2)]);
            assert_eq!(s.number(&clip, "scale_x"), 0.5);
            assert_eq!(s.number(&clip, "crop_left"), 0.2);
            s.call("fill_frame", vec![Value::resource(&clip)]);
            assert_eq!(s.number(&clip, "scale_x"), 1.0, "the picture as it comes");
            assert_eq!(s.number(&clip, "crop_left"), 0.0, "no crop");
            assert_eq!(s.number(&clip, "pos_x"), 0.0);
            // Nothing left to take back, the action refuses rather than pretending it did
            // something.
            let again = s.try_call("fill_frame", vec![Value::resource(&clip)]);
            assert!(again.is_err(), "a picture already plain is refused: {again:?}");
        }

        phase a_mask_is_fitted_over_the_picture_as_the_frame_shows_it;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            // The source is 1920 × 1080 and the cut 1000 × 500: wider than the source, so the
            // picture takes the full height and 16/9 of the width.
            s.call(
                "set_frame_size",
                vec![Value::resource(&p), Value::Int(1000), Value::Int(500)],
            );
            s.call("set_mask", vec![Value::resource(&clip), Value::text("rect")]);
            s.call("fit_mask", vec![Value::resource(&clip)]);
            let across = (1920.0_f64 / 1080.0) / 2.0;
            assert!(
                (s.number(&clip, "mask_w") - across).abs() < 1e-6,
                "the mask takes the picture's width: {} against {across}",
                s.number(&clip, "mask_w")
            );
            assert!((s.number(&clip, "mask_h") - 1.0).abs() < 1e-6, "and its whole height");
            // Scaled up, the mask grows with it and follows the move.
            let scale = look_case(&s, "scale_x");
            s.call("set_look", vec![Value::resource(&clip), scale, Value::Float(2.0)]);
            let across_again = look_case(&s, "pos_x");
            s.call("set_look", vec![Value::resource(&clip), across_again, Value::Float(0.25)]);
            s.call("fit_mask", vec![Value::resource(&clip)]);
            assert!((s.number(&clip, "mask_w") - 2.0 * across).abs() < 1e-6);
            assert!((s.number(&clip, "mask_x") - 0.25).abs() < 1e-6, "it moves with the picture");
        }
    }

    tilo_test_env::scenario! {
        fn the_mix_says_where_it_clipped_and_comes_down_as_one;

        phase the_device_measures_where_the_mix_goes_over;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            *s.engine.over.borrow_mut() = Some((6.0, vec![3.0, 7.5]));
            s.call("check_mix", vec![Value::resource(&p)]);
            assert_eq!(s.number(&p, "over_db"), 6.0, "how far over full scale");
            assert_eq!(s.numbers(&p, "clips_at"), vec![3.0, 7.5], "and where");
        }

        phase a_play_hears_the_mix_as_it_goes;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            s.call(
                "play_cut",
                vec![Value::resource(&p), Value::Float(0.0), Value::Float(1.0)],
            );
            s.engine.heard("live-1", 4.0, 4.5, &[3.0]);
            s.call("hear_mix", vec![Value::resource(&p)]);
            assert_eq!(s.number(&p, "over_db"), 4.5, "what the play has heard so far");
            assert_eq!(s.numbers(&p, "clips_at"), vec![3.0]);
            let at = s.call("live_at", vec![Value::resource(&p)]);
            assert!(
                matches!(at, InvokeOutcome::Done(Value::Float(f)) if (f - 4.0).abs() < 1e-9),
                "the play's word says where it stands: {at:?}"
            );
        }

        phase the_window_flags_the_clip_and_presses_take_the_playhead_there;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            *s.engine.over.borrow_mut() = Some((6.0, vec![3.0, 7.5]));
            s.call("check_mix", vec![Value::resource(&p)]);
            let window = s.window();
            assert!(
                of_kind(window.tree(), "ui.Banner")
                    .iter()
                    .any(|banner| banner.props.get("title").and_then(Value::as_text)
                        == Some("The mix is +6 dB over full scale")),
                "the mix's flag says how far over it went:\n{}",
                dump(window.tree())
            );
            // The button beside the tracks takes the playhead to the first place it clipped,
            // and the device is asked for the frame there.
            let go = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|button| button.props.get("label").and_then(Value::as_text)
                    == Some("Go to where it clipped"))
                .expect("the way to the clip");
            let key = go.key.clone();
            let jumped = window.fire_with(&key, "press", None);
            assert!(matches!(jumped, Ok(InvokeOutcome::Done(_))), "{jumped:?}");
            assert_eq!(number_state(&window, "playhead"), Some(3.0));
            let ask: serde_json::Value =
                serde_json::from_str(&s.engine.input_of("still")).expect("the still's ask");
            assert_eq!(json_number(&ask, "at"), 3.0, "the frame where it clipped");
            // Pressing again goes to the next place, and then round to the first.
            let again = window.fire_with(&key, "press", None);
            assert!(matches!(again, Ok(InvokeOutcome::Done(_))), "{again:?}");
            assert_eq!(number_state(&window, "playhead"), Some(7.5));
            let round = window.fire_with(&key, "press", None);
            assert!(matches!(round, Ok(InvokeOutcome::Done(_))), "{round:?}");
            assert_eq!(
                number_state(&window, "playhead"),
                Some(3.0),
                "it comes back rather than stopping at the last place"
            );
        }

        phase the_trim_comes_down_as_one_and_keeps_the_balance;
        {
            let s = Studio::new();
            let (p, _, _) = s.placed();
            let tracks = s.tracks(&p);
            let gain = |t: &ResourceRef| s.number(t, "gain_db");
            *s.engine.over.borrow_mut() = Some((6.0, vec![3.0]));
            s.call("check_mix", vec![Value::resource(&p)]);
            let over = s.number(&p, "over_db");
            // The trim is what ends it: the device measures the mix the trim made, and it fits.
            *s.engine.over.borrow_mut() = None;
            s.call("trim_tracks", vec![Value::resource(&p), Value::Float(over)]);
            for track in &tracks {
                assert_eq!(gain(track), -6.0, "every track comes down by the same amount");
            }
            assert_eq!(s.number(&p, "over_db"), 0.0, "the trim measures the mix it made");
            assert!(s.numbers(&p, "clips_at").is_empty());
            let nothing = s.try_call("trim_tracks", vec![Value::resource(&p), Value::Float(0.0)]);
            assert!(nothing.is_err(), "a trim of nothing is refused: {nothing:?}");
        }
    }

    tilo_test_env::scenario! {
        fn a_clip_takes_a_tone_curve_per_channel;

        phase a_channel_curve_reaches_the_device_that_draws_it;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            let points = Value::List(vec![
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.5),
                Value::Float(0.7),
                Value::Float(1.0),
                Value::Float(1.0),
            ]);
            s.call("set_curve", vec![Value::resource(&clip), Value::text("red"), points]);
            assert_eq!(s.numbers(&clip, "curve_red"), vec![0.0, 0.0, 0.5, 0.7, 1.0, 1.0]);
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let cut = cut_of(&s, "still");
            let asked: serde_json::Value =
                serde_json::from_str(cut["clips"][0].as_str().unwrap()).expect("the clip");
            let red: Vec<f64> = asked["curve_red"]
                .as_array()
                .expect("the red curve")
                .iter()
                .map(|n| n.as_f64().expect("a number"))
                .collect();
            assert_eq!(red, vec![0.0, 0.0, 0.5, 0.7, 1.0, 1.0], "the grade gets its points");
            assert!(
                asked["curve_master"].as_array().is_some_and(Vec::is_empty),
                "the master is left straight"
            );
            // Bypassed, mixed out, and back to the straight line.
            s.call("set_curve_mix", vec![Value::resource(&clip), Value::Float(0.0)]);
            assert_eq!(s.number(&clip, "curve_mix"), 0.0);
            s.call("reset_curve", vec![Value::resource(&clip), Value::text("red")]);
            assert!(s.numbers(&clip, "curve_red").is_empty(), "the straight line");
        }

        phase the_inspector_shows_the_curve_and_its_edits_land;
        {
            let s = Studio::new();
            let (p, _, clip) = s.placed();
            let points = Value::List(vec![
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.25),
                Value::Float(0.4),
                Value::Float(1.0),
                Value::Float(1.0),
            ]);
            s.call("set_curve", vec![Value::resource(&clip), Value::text("master"), points]);
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let window = s.window();
            let span = of_kind(window.tree(), "ui.TimelineSpan").remove(0).key.clone();
            let picked = window.fire_with(&span, "pressed", Some(&Value::Int(0)));
            assert!(matches!(picked, Ok(InvokeOutcome::Done(_))), "{picked:?}");
            let window = s.window();
            let look = of_kind(window.tree(), "ui.Segment")
                .into_iter()
                .find(|segment| segment.props.get("label").and_then(Value::as_text) == Some("Look"))
                .expect("the Look tab");
            let look = look.key.clone();
            let opened = window.fire_with(&look, "press", None);
            assert!(matches!(opened, Ok(InvokeOutcome::Done(_))), "{opened:?}");
            let window = s.window();
            let editor = of_kind(window.tree(), "ui.CurveEditor")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the curve editor:\n{}", dump(window.tree())));
            let drawn = match editor.props.get("points") {
                Some(Value::List(items)) => items
                    .iter()
                    .map(|item| match item {
                        Value::Float(n) => *n,
                        other => panic!("a point reads {other:?}"),
                    })
                    .collect::<Vec<f64>>(),
                other => panic!("the editor's points read {other:?}"),
            };
            assert_eq!(drawn, vec![0.0, 0.0, 0.25, 0.4, 1.0, 1.0], "it draws the model's points");
            // A drag the editor reports writes the channel it shows, and the picture follows.
            let key = editor.key.clone();
            let moved = Value::List(vec![
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.5),
                Value::Float(0.6),
                Value::Float(1.0),
                Value::Float(1.0),
            ]);
            let done = window.fire_with(&key, "change", Some(&moved));
            assert!(matches!(done, Ok(InvokeOutcome::Done(_))), "{done:?}");
            assert_eq!(
                s.numbers(&clip, "curve_master"),
                vec![0.0, 0.0, 0.5, 0.6, 1.0, 1.0],
                "the shaped curve is the clip's"
            );
            let cut = cut_of(&s, "still");
            let asked: serde_json::Value =
                serde_json::from_str(cut["clips"][0].as_str().unwrap()).expect("the clip");
            let master: Vec<f64> = asked["curve_master"]
                .as_array()
                .expect("the master curve")
                .iter()
                .map(|n| n.as_f64().expect("a number"))
                .collect();
            assert_eq!(master, vec![0.0, 0.0, 0.5, 0.6, 1.0, 1.0], "and the picture follows");
        }
    }

    tilo_test_env::scenario! {
        fn a_title_is_set_in_the_font_the_person_chose;

        phase a_title_takes_a_family_and_the_engine_draws_it;
        {
            let s = Studio::new();
            let p = s.new_project();
            let title = a_title(&s, &p);
            s.call("set_font", vec![Value::resource(&title), Value::text("DejaVu Serif")]);
            assert_eq!(s.text(&title, "font"), "DejaVu Serif");
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let cut = cut_of(&s, "still");
            let media: serde_json::Value =
                serde_json::from_str(cut["media"][0].as_str().unwrap()).expect("the source");
            assert_eq!(
                media["font"], "DejaVu Serif",
                "the device is asked for the title in the family the person chose"
            );
            // The system's own sans is a family of no name, and a way back to it.
            s.call("set_font", vec![Value::resource(&title), Value::text("")]);
            s.call("preview", vec![Value::resource(&p), Value::Float(1.0)]);
            let cut = cut_of(&s, "still");
            let media: serde_json::Value =
                serde_json::from_str(cut["media"][0].as_str().unwrap()).expect("the source");
            assert_eq!(media["font"], "", "back to the system's own sans");
        }

        phase the_inspector_offers_the_faces_this_device_sets_text_in;
        {
            let s = Studio::new();
            let p = s.new_project();
            let title = a_title(&s, &p);
            let window = s.window();
            let _ = &title;
            let row = of_kind(window.tree(), "ui.ListRow")
                .into_iter()
                .find(|row| row.props.get("title").and_then(Value::as_text) == Some("Harbour"))
                .expect("the title's row in the bin");
            let key = row.key.clone();
            let opened = window.fire_with(&key, "activate", None);
            assert!(matches!(opened, Ok(InvokeOutcome::Done(_))), "{opened:?}");
            let window = s.window();
            assert!(
                of_kind(window.tree(), "ui.MenuButton")
                    .iter()
                    .any(|button| button.props.get("label").and_then(Value::as_text)
                        == Some("System sans")),
                "the title's font picker names the family in force:\n{}",
                dump(window.tree())
            );
        }
    }
}
