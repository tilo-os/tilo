//! A file's pixels and the files a person chooses, end to end: `File.image_info`, the crop,
//! turn, resize and encode a program applies, `File.thumbnail`, and `file.choose` /
//! `file.choose_folder` filling the parameter an action still needs.

#[cfg(test)]
mod cases {

    use crate::common::*;
    use tilo_image_codec::{DecodedImage, Encoding};
    use tilolang_runtime::blob::FileValue;
    use tilolang_runtime::{AmStore, InvokeOutcome, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.images "Images"

type Shelf {
    note: Text
}

action read(s: Shelf, f: File) uses image {
    let said = match f.image_info() {
        .some(info) => "{info.width}x{info.height}"
        .none => "none"
    }
    s.note = said
}

action crop(s: Shelf, f: File) uses image {
    let said = match f.cropped(0.0, 0.0, 0.5, 1.0) {
        .ok(out) => "{out.name} {out.url}",
        .err(why) => why
    }
    s.note = said
}

action turn(s: Shelf, f: File) uses image {
    let said = match f.turned(1) {
        .ok(out) => "{out.name} {out.url}",
        .err(why) => why
    }
    s.note = said
}

action resize(s: Shelf, f: File) uses image {
    let said = match f.resized(1, 1) {
        .ok(out) => "{out.name} {out.url}",
        .err(why) => why
    }
    s.note = said
}

action encode(s: Shelf, f: File) uses image {
    let said = match f.encoded(format: "webp") {
        .ok(out) => "{out.media} {out.url}",
        .err(why) => why
    }
    s.note = said
}

action huge(s: Shelf, f: File) uses image {
    let out = f.thumbnail(99999)
}

action nothing(s: Shelf, f: File) uses image {
    let said = match f.resized(0, 0) {
        .ok(out) => out.name,
        .err(why) => why
    }
    s.note = said
}

action here(s: Shelf, f: File) uses image {
    s.note = if f.here() { "here" } else { "arriving" }
}

action thumb(s: Shelf, f: File) uses image {
    let said = match f.thumbnail(64) {
        .some(preview) => preview.name,
        .none => "none"
    }
    s.note = said
}

action imported(s: Shelf, files: List<File>) (label: "Import many") {
    let first = match files.first() {
        .some(file) => file.path,
        .none => ""
    }
    s.note = "{files.len()} {first}"
}

action imported_one(s: Shelf, f: File) (label: "Import one") {
    s.note = f.path
}

action imported_folder(s: Shelf, files: List<File>) (label: "Import folder") {
    let first = match files.first() {
        .some(file) => file.path,
        .none => ""
    }
    s.note = "{files.len()} {first}"
}

action imported_path(s: Shelf, folder: Text) (label: "Import folder's path") {
    s.note = folder
}

view shelf(s: Shelf) {
    stack {
        ui.Button("Import many") {
            on press: file.choose(imported(s))
        }
        ui.Button("Import one") {
            on press: file.choose(imported_one(s))
        }
        ui.Button("Import folder") {
            on press: file.choose_folder(imported_folder(s))
        }
        ui.Button("Import folder's path") {
            on press: file.choose_folder(imported_path(s))
        }
    }
}
"#;

    /// A 3x2 PNG of one colour: what a photograph in the store is.
    fn photo_png() -> Vec<u8> {
        let image = DecodedImage::filled(3, 2, [10, 20, 30, 255]);
        tilo_image_codec::encode_now(&image, Encoding::Png).expect("the fixture encodes")
    }

    struct Bench {
        rt: Runtime,
        app: App,
        shelf: tilolang_runtime::ResourceRef,
        device: Files,
        me: tilolang_runtime::Actor,
    }

    impl Bench {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            // A PNG's preview is a picture of its own, named for its size; nothing else has one.
            let device = Files::default().previewing(|files, file, size| {
                let stem = file.name.split('.').next().unwrap_or("preview");
                (file.media == "image/png").then(|| {
                    files.keeping(&format!("{stem}-{size}.png"), "image/png", &photo_png())
                })
            });
            rt.set_blobs(device.clone());
            let shelf = rt.create("Shelf").unwrap();
            Self {
                rt,
                app,
                shelf,
                device,
                me: me(),
            }
        }

        fn photo(&self) -> FileValue {
            self.device.keeping("photo.png", "image/png", &photo_png())
        }

        fn note(&self) -> String {
            match self.rt.read(&self.shelf, "note").unwrap() {
                Value::Text(text) => text,
                other => panic!("the note is {other:?}"),
            }
        }

        fn call(&self, action: &str, file: &FileValue) -> String {
            let out = self
                .rt
                .invoke_action(
                    &self.me,
                    (
                        self.app.action(action).as_str(),
                        vec![Value::resource(&self.shelf), Value::file(file.clone())],
                    ),
                )
                .unwrap();
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            self.note()
        }

        fn press(&self, label: &str) -> String {
            let view = self
                .rt
                .open_view(
                    &self.me,
                    &self.app.view("shelf"),
                    vec![Value::resource(&self.shelf)],
                )
                .unwrap();
            let button = of_kind(view.tree(), "ui.Button")
                .into_iter()
                .find(|button| {
                    matches!(button.props.get("label"), Some(Value::Text(text)) if text == label)
                })
                .unwrap_or_else(|| panic!("no {label} control in\n{}", dump(view.tree())))
                .key
                .clone();
            let out = view.press(&button).unwrap();
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            self.note()
        }
    }

    tilo_test_env::scenario! {
        fn image_1;

        phase an_image_states_its_size_and_is_cropped_turned_resized_and_encoded;
        {
            let bench = Bench::new();
            let photo = bench.photo();
            assert_eq!(bench.call("read", &photo), "3x2");

            for (action, name, dimensions) in [
                ("crop", "photo-cropped.png", (2, 2)),
                ("turn", "photo-turned.png", (2, 3)),
                ("resize", "photo-resized.png", (1, 1)),
            ] {
                let said = bench.call(action, &photo);
                let (produced, url) = said.split_once(' ').expect("a file's name and address");
                assert_eq!(produced, name);
                let image = tilo_image_codec::decode_upright_now(&bench.device.bytes_at(url), None)
                    .expect("the produced file decodes");
                assert_eq!(
                    (image.width, image.height),
                    dimensions,
                    "{action} moved the pixels it names"
                );
            }

            let said = bench.call("encode", &photo);
            let (media, url) = said
                .split_once(' ')
                .expect("the file's media type and address");
            assert_eq!(media, "image/webp");
            assert!(bench.device.bytes_at(url).starts_with(b"RIFF"));
        }

        phase a_file_says_whether_this_device_holds_its_bytes;
        {
            let bench = Bench::new();
            let photo = bench.photo();
            assert_eq!(bench.call("here", &photo), "here");
            // A record whose bytes another device still holds: its address is all this one has.
            let away = FileValue::new("away.png", "image/png", 12, "not-in-this-store");
            assert_eq!(bench.call("here", &away), "arriving");
        }

        phase a_thumbnail_comes_from_the_devices_preview_cache_or_is_none;
        {
            let bench = Bench::new();
            let photo = bench.photo();
            assert_eq!(bench.call("thumb", &photo), "photo-64.png");
            let not_an_image = bench.device.keeping("plan.md", "text/markdown", b"# Plan");
            assert_eq!(bench.call("thumb", &not_an_image), "none");
        }

        phase a_press_with_no_chooser_is_answered_with_the_systems_refusal;
        {
            // A device that serves no chooser at all, as one nobody is at: the press is told what
            // it cannot do, in words the app shows, rather than doing nothing.
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let shelf = rt.create("Shelf").unwrap();
            let view = rt
                .open_view(&me(), &app.view("shelf"), vec![Value::resource(&shelf)])
                .unwrap();
            let button = of_kind(view.tree(), "ui.Button")
                .into_iter()
                .find(|button| {
                    matches!(button.props.get("label"), Some(Value::Text(text)) if text == "Import one")
                })
                .unwrap_or_else(|| panic!("no Import one control in\n{}", dump(view.tree())))
                .key
                .clone();
            let refused = view
                .press(&button)
                .expect_err("a host that chooses no files refuses the press");
            assert!(refused.to_string().contains("chooses no files"), "{refused}");
        }

        phase a_chooser_fills_a_list_of_files_or_the_one_file_an_action_asks_for;
        {
            let bench = Bench::new();
            let one = bench
                .device
                .keeping("a.png", "image/png", &photo_png())
                .with_path("trip/a.png");
            let two = bench
                .device
                .keeping("b.png", "image/png", &photo_png())
                .with_path("trip/b.png");
            bench.device.pick(vec![one.clone(), two]);

            assert_eq!(bench.press("Import many"), "2 trip/a.png");
            let asked = bench.device.asked().last().cloned().unwrap();
            assert!(asked.multiple, "a `List<File>` may take several");
            assert!(!asked.folder);

            bench.device.pick(vec![one]);
            assert_eq!(bench.press("Import one"), "trip/a.png");
            let asked = bench.device.asked().last().cloned().unwrap();
            assert!(!asked.multiple, "a `File` takes the one file chosen");

            let one = bench
                .device
                .keeping("a.png", "image/png", &photo_png())
                .with_path("trip/a.png");
            let two = bench
                .device
                .keeping("b.png", "image/png", &photo_png())
                .with_path("trip/b.png");
            bench.device.pick(vec![one, two]);
            assert_eq!(bench.press("Import folder"), "2 trip/a.png");
            let asked = bench.device.asked().last().cloned().unwrap();
            assert!(asked.folder, "a folder chooser asks for a folder");

            // An action taking the folder itself is filled with the reference that reaches it:
            // what `File.list`, `File.read` and `File.watch` take, never a list of its files.
            bench.device.pick_folder("/chosen/Shelf");
            assert_eq!(bench.press("Import folder's path"), "/chosen/Shelf");
            let asked = bench.device.asked().last().cloned().unwrap();
            assert!(asked.folder, "a folder chooser asks for a folder");
        }

        phase an_oversized_preview_or_resize_is_refused;
        {
            let bench = Bench::new();
            let photo = bench.photo();
            let out = bench.rt.invoke_action(
                &bench.me,
                (
                    "huge",
                    vec![Value::resource(&bench.shelf), Value::file(photo.clone())],
                ),
            );
            assert!(out.is_err(), "an oversized preview is refused: {out:?}");
            let said = bench.call("nothing", &photo);
            assert!(
                said.contains("is refused"),
                "a resize to no pixels is refused with why: {said}"
            );
            let not_an_image = bench.device.keeping("plan.md", "text/markdown", b"# Plan");
            let said = bench.call("crop", &not_an_image);
            assert!(
                said.contains("is no image this device opens"),
                "a file that is no image is refused with why: {said}"
            );
        }

        phase a_dismissed_chooser_runs_nothing;
        {
            let bench = Bench::new();
            assert_eq!(bench.press("Import many"), "");
            assert_eq!(
                bench.device.asked().len(),
                1,
                "the chooser was asked"
            );
        }
    }
}
