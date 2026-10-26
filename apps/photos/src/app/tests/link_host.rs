//! The test doubles the suite needs to act through the app's real interfaces: an
//! action host that answers in-process, and a clipboard that remembers what was
//! put on it. They live apart so a test reads as what it checks.

use super::*;
/// The room's host behind the door, announcing a web entry for links.
pub(super) struct LinkHost;

impl tilo_actions::remote::ActionHost for LinkHost {
    fn query(
        &self,
        _: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
    > {
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::CataloguePage {
            locator: Some(tilo_actions::remote::ResourceViewLocator {
                origin: "https://host.example".into(),
                room: "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90".into(),
                host_device: "ab".repeat(32),
                provider: query.provider.unwrap_or_default(),
                web_entry: Some("https://tilo.so/".into()),
            }),
            exhausted: true,
            ..tilo_actions::remote::CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _: &tilo_actions::remote::Caller,
        _: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionHostError::NotPermitted,
        ))
    }

    fn observe(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionHostError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

#[derive(Default)]
pub(super) struct LinkClipboard(std::sync::Mutex<Option<String>>);

impl ui::clipboard::ClipboardBackend for LinkClipboard {
    fn set_text(&self, _: ui::clipboard::ClipboardTarget, text: &str) -> bool {
        *self.0.lock().unwrap() = Some(text.into());
        true
    }

    fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
        self.0.lock().unwrap().clone()
    }

    fn name(&self) -> &'static str {
        "photos-share-test-clipboard"
    }
}

#[test]
fn share_copies_the_open_photographs_link() {
    use ui::framework::testing::TestApp;
    let _served = ui::RemoteActions::enter(
        std::sync::Arc::new(LinkHost),
        tilo_actions::remote::Caller::local_person("owner"),
    );
    let services = ui::host::HostServices::null()
        .with_clipboard(std::sync::Arc::new(LinkClipboard::default()));
    let _scope = ui::host::enter(&services);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lake.png");
    std::fs::write(
        &path,
        crate::pixels::encode_png(&crate::shots_pattern(30, 20)).unwrap(),
    )
    .unwrap();
    let mut app =
        TestApp::<crate::resource::PhotosShell>::launched(&[path.to_str().unwrap()]).unwrap();
    let lake = app.state().all_photos().next().unwrap().id.clone();
    let item = crate::resource::photo_resource_id(&lake);
    app.call_on(&item, PhotosEffect::VIEW.id, serde_json::json!({}))
        .unwrap();
    app.settle();

    app.surface_mut::<PhotosApp>().share_open_photo();
    app.settle();
    let copied = ui::clipboard_paste().expect("the link is copied");
    assert!(copied.starts_with("https://tilo.so/app/#/"), "{copied}");
    assert_eq!(
        app.surface_mut::<PhotosApp>().notice.as_deref(),
        Some(tr("photos-share-copied").as_str())
    );
}

/// Making thumbnails is published as an offloadable transform of the
/// thumbnails package, and its local run makes exactly what that package's
/// entrypoint makes on another device: content named twice is one input,
/// and an input that is not an image fails the whole batch.
#[test]
fn make_thumbnails_runs_the_offloadable_batch_and_matches_the_package_entrypoint() {
    let spec = <PhotosEffect as ui::tilo_resource::Effect>::specs()
        .into_iter()
        .find(|spec| spec.id == "make-thumbnails")
        .expect("the library makes thumbnails");
    let offload = spec
        .contract
        .offload
        .clone()
        .expect("making thumbnails declares its offload");
    assert_eq!(offload.app, thumbnail::batch::PACKAGE);
    assert_eq!(offload.entrypoint, thumbnail::batch::ENTRYPOINT);
    assert!(
        spec.contract.validate_declaration().is_empty(),
        "{:?}",
        spec.contract.validate_declaration()
    );
    assert!(spec.params()[0].is_content_list());

    let store = tilo_content::ContentStore::memory();
    let photographs: Vec<Vec<u8>> = [(3, 2), (40, 17)]
        .into_iter()
        .map(|(width, height)| {
            let rgba = (0..width * height * 4)
                .map(|index| u8::try_from(index % 251).expect("a byte"))
                .collect();
            pixels::encode_png(&pixels::Rgba::new(width, height, rgba).expect("an image"))
                .expect("a PNG")
        })
        .collect();
    let hashes: Vec<String> = photographs
        .iter()
        .map(|bytes| store.put(bytes).expect("stage a photograph"))
        .collect();
    let mut app = PhotosApp::with_content(store.clone());
    let doc = crate::test_doc::new_doc();
    let library = crate::test_doc::library(&doc);
    let output = app
        .answer_effect(
            PhotosEffect::MakeThumbnails {
                photos: vec![
                    hashes[0].clone(),
                    tilo_actions::content_resource(&hashes[1]).expect("an address"),
                    hashes[0].clone(),
                ],
            },
            tilo_actions::InvocationId::new(),
            &library,
        )
        .expect("thumbnails are made")
        .expect("the batch answers at once");
    let expected = thumbnail::batch::thumbnail_batch(&photographs).expect("the package's batch");
    let made: Vec<Vec<u8>> = output
        .resources
        .iter()
        .map(|resource| {
            store
                .read(tilo_actions::content_hash(resource).expect("content"))
                .expect("a stored thumbnail")
        })
        .collect();
    assert_eq!(made, expected, "byte for byte the entrypoint's thumbnails");

    let broken = store.put(b"not a photograph").expect("stage bytes");
    assert_eq!(
        app.answer_effect(
            PhotosEffect::MakeThumbnails {
                photos: vec![hashes[0].clone(), broken],
            },
            tilo_actions::InvocationId::new(),
            &library,
        ),
        Err(thumbnail::batch::BatchError::Undecodable { index: 1 }.to_string())
    );
    assert!(
        app.answer_effect(
            PhotosEffect::MakeThumbnails {
                photos: vec!["/home/someone/photo.png".into()],
            },
            tilo_actions::InvocationId::new(),
            &library,
        )
        .is_err(),
        "a path is never content"
    );
}

#[test]
fn the_empty_library_open_button_asks_the_host_chooser() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = ui::framework::testing::TestApp::<crate::resource::PhotosShell>::new().unwrap();
    app.settle();
    app.harness()
        .perform_named("photos-empty-open", &AccessAction::Activate)
        .unwrap();
    app.settle();
    let asked = recorder.pick_requests();
    assert_eq!(
        asked.len(),
        1,
        "one chooser: {:?}",
        app.harness().control_failures()
    );
    assert_eq!(asked[0].title(), tr("photos-open-title"));
}

/// The sidebar is the window shell's list, declared from the library: the
/// destinations with the Timeline's and Favorites' counts, then the albums
/// under their heading; what is shown is the chosen row, and a row the
/// person chooses opens what it names.
#[test]
fn the_sidebar_declares_the_librarys_places_and_opens_the_chosen_one() {
    use ui::chrome::NavCount;
    let mut app = test_app();
    let mut doc = crate::test_doc::new_doc();
    let mut library = crate::model::PhotoLibrary::default();
    let mut first = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
    first.favorite = true;
    let second = LibraryPhoto::from_image(OpenImage::fixture("/photos/two.png", 20, 10));
    library.add_photo(first.clone());
    library.add_photo(second);
    library.add_album(Album {
        id: "photos.album:trip".into(),
        name: "Trip".into(),
        photo_ids: crate::model::labels([first.id]),
    });
    crate::test_doc::set_library(&mut doc, &library);
    sync(&mut app, &mut doc);
    app.show_library();
    sync(&mut app, &mut doc);
    app.place_chrome();
    let (sections, selected) = app.chrome.places.shown().expect("the places are shown");
    let row = |id: &str| {
        sections
            .iter()
            .flat_map(|section| &section.items)
            .find(|item| item.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is a row"))
    };
    assert_eq!(selected.as_deref(), Some("photos:library:timeline"));
    assert_eq!(
        row("photos:library:timeline").count,
        Some(NavCount::Neutral(2))
    );
    assert_eq!(
        row("photos:library-favorites").count,
        Some(NavCount::Neutral(1))
    );
    assert_eq!(sections.len(), 1, "one list of places, no albums heading");
    let albums = row("photos:library:albums");
    assert_eq!(albums.label, tr("photos-albums"));
    assert_eq!(albums.children.len(), 1, "the albums nest under Albums");
    let trip = &albums.children[0];
    assert_eq!(trip.label, "Trip");
    assert_eq!(
        trip.art.as_ref().map(|art| art.name.as_str()),
        Some("Trip"),
        "an album stands in the icon's place by its cover or initials"
    );

    // The person chooses the album in the shell's list.
    let shell = ui::widget::NavShell::new()
        .sidebar(Box::new(
            ui::widget::NavList::new(Vec::new()).fed_by(&app.chrome.places),
        ))
        .content(Box::new(ui::widget::Spacer::new()));
    let size = (1280, 800);
    let mut harness =
        ui::headless::FrameHarness::for_root(ui::State::new(), shell, size, Theme::dark());
    harness.relayout_at(size).expect("the shell lays out");
    let trip = harness
        .root::<ui::widget::NavShell>()
        .sidebar_ref::<ui::widget::NavList>()
        .and_then(|list| list.row_rect("photos:library-album:photos.album:trip"))
        .expect("the album is a row");
    harness.click_at(trip.center().x, trip.center().y);
    sync(&mut app, &mut doc);
    assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
    app.place_chrome();
    let (_, selected) = app.chrome.places.shown().expect("the places are shown");
    assert_eq!(
        selected.as_deref(),
        Some("photos:library-album:photos.album:trip")
    );
}

/// The album panel never hides the way to fill an empty library: where the
/// timeline keeps its readable width beside it, it docks; narrower, it rises
/// as a sheet from the window's foot and the empty state's Open stands in the
/// part above it.
#[test]
fn the_album_panel_leaves_an_empty_librarys_open_in_view() {
    let _settled = ui::motion::testing::settled();
    for (width, docks) in [(1_160.0, true), (716.0, false)] {
        let size = (width as u32, 800);
        let mut harness =
            ui::headless::FrameHarness::for_root(ui::State::new(), test_app(), size, Theme::dark());
        harness.relayout_at(size).expect("Photos lays out");
        harness.root_mut::<PhotosApp>().begin_new_album();
        harness.relayout_at(size).expect("Photos lays out");
        let app = harness.root::<PhotosApp>();
        assert!(
            app.chrome.inspector.is_open(),
            "{width}px: the album panel is open"
        );
        assert_eq!(
            app.chrome.inspector.is_docked(),
            docks,
            "{width}px: the panel docks only beside a readable timeline"
        );
        let open = app
            .empty_state
            .as_ref()
            .expect("an empty library shows its empty state")
            .action_rect();
        assert!(open.width() > 0.0, "{width}px: the empty state offers Open");
        if docks {
            let panel_left = app.chrome.inspector.sidebar_rect().x0;
            assert!(
                open.x1 <= panel_left,
                "{width}px: Open ({open:?}) stands clear of the panel from x {panel_left}"
            );
        } else {
            let sheet = app
                .chrome
                .inspector
                .sheet()
                .expect("a narrow window's panel is a sheet")
                .placed()
                .panel;
            assert!(
                sheet.height() > 0.0,
                "{width}px: the sheet stands at the foot"
            );
            assert!(
                open.y1 <= sheet.y0,
                "{width}px: Open ({open:?}) stands above the sheet from y {}",
                sheet.y0
            );
        }
    }
}

/// Photos opened in a narrow window draws its timeline on the first frame,
/// as it does when a wide window narrows, with its places listed for the
/// screen behind Back.
#[test]
fn photos_opened_narrow_draws_its_grid_on_the_first_frame() {
    for viewport in [(1280, 800), (420, 800)] {
        let mut app = ui::framework::testing::TestApp::<crate::resource::PhotosShell>::capture_at(
            Some("library"),
            viewport,
        )
        .expect("the library captures");
        let photos = app
            .harness()
            .widget_mut::<PhotosApp>()
            .unwrap_or_else(|| panic!("{viewport:?}: the window shows the library"));
        assert!(
            !photos.library_cards.is_empty(),
            "{viewport:?}: the timeline shows photographs on the first frame"
        );
        let (_, selected) = photos
            .chrome
            .places
            .shown()
            .unwrap_or_else(|| panic!("{viewport:?}: the places are listed"));
        assert!(
            selected.is_some(),
            "{viewport:?}: the open place is chosen in the list"
        );
    }
}

/// In a narrow window the album editor rises as a sheet at half height, and
/// the timeline above it stays live: a press on a photograph there adds it
/// to the album, or takes it out.
#[test]
fn a_narrow_album_editor_leaves_the_timeline_above_it_live() {
    let _settled = ui::motion::testing::settled();
    let mut app = ui::framework::testing::TestApp::<crate::resource::PhotosShell>::capture_at(
        Some("album-manage"),
        (420, 800),
    )
    .expect("the album editor captures");
    // A narrow window shows one pane: the timeline, under the editor.
    app.with_widget(|shell: &mut ui::widget::NavShell| {
        shell.show(ui::widget::NavPane::Content);
    })
    .expect("Photos stands in a NavShell");
    app.harness().relayout();
    let photos = app.surface_mut::<PhotosApp>();
    assert_eq!(
        photos.chrome.inspector.sheet_detent(),
        Some(ui::widget::SheetDetent::Half),
        "the editor rises to half height"
    );
    let sheet = photos
        .chrome
        .inspector
        .sheet()
        .expect("the editor is a sheet")
        .placed()
        .panel;
    assert!(sheet.height() > 0.0 && sheet.y0 > 0.0, "{sheet:?}");
    let (id, card) = photos
        .library_cards
        .iter()
        .find(|(_, rect, _)| rect.y1 < sheet.y0 && rect.height() > 0.0)
        .map(|(id, rect, _)| (id.clone(), *rect))
        .expect("a photograph shows above the sheet");
    let was_member = photos.album_editor_members.contains(&id);
    let at = card.center();
    app.harness().click_at(at.x, at.y);
    let photos = app.surface_mut::<PhotosApp>();
    assert!(photos.album_editor, "the editor stays open");
    assert_ne!(
        photos.album_editor_members.contains(&id),
        was_member,
        "the press above the sheet chose the photograph"
    );
}

/// What takes Back's step: the window bar's Back, Escape, or the system's
/// back gesture.
#[derive(Clone, Copy, Debug)]
enum BackInput {
    Bar,
    Escape,
    Gesture,
}

/// In a narrow window Back, Escape and the system's back gesture each take
/// the album editor's sheet down from the whole window to half, and from
/// half or peeking put the editor away, before anything navigates.
#[test]
fn back_lowers_or_puts_away_the_narrow_album_editor_before_it_goes_anywhere() {
    use ui::widget::SheetDetent;
    let _settled = ui::motion::testing::settled();
    for input in [BackInput::Bar, BackInput::Escape, BackInput::Gesture] {
        for (from, after) in [
            (SheetDetent::Full, Some(SheetDetent::Half)),
            (SheetDetent::Half, None),
            (SheetDetent::Peek, None),
        ] {
            let mut app =
                ui::framework::testing::TestApp::<crate::resource::PhotosShell>::capture_at(
                    Some("album-manage"),
                    (420, 800),
                )
                .expect("the album editor captures");
            app.with_widget(|shell: &mut ui::widget::NavShell| {
                shell.show(ui::widget::NavPane::Content);
            })
            .expect("Photos stands in a NavShell");
            app.surface_mut::<PhotosApp>()
                .chrome
                .inspector
                .snap_sheet(from);
            app.harness().relayout();
            let photos = app.surface_mut::<PhotosApp>();
            assert_eq!(photos.chrome.inspector.sheet_detent(), Some(from));
            assert!(
                photos
                    .window_bar()
                    .leading
                    .iter()
                    .any(|action| action.id == ui::widget::WINDOW_BACK),
                "the bar offers Back while the editor's sheet shows"
            );
            match input {
                BackInput::Bar => {
                    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
                        id: ui::widget::WINDOW_BACK.into(),
                    });
                    photos.apply_window_toolbar();
                }
                BackInput::Escape => app.harness().key("Escape"),
                BackInput::Gesture => assert!(
                    app.harness().nav_gesture(ui::gesture::NavDirection::Back),
                    "the sheet takes the back gesture"
                ),
            }
            app.harness().relayout();
            let photos = app.surface_mut::<PhotosApp>();
            let shown = photos
                .album_editor
                .then(|| photos.chrome.inspector.sheet_detent())
                .flatten();
            assert_eq!(shown, after, "{input:?} from {from:?}");
            let front = app
                .with_widget(|shell: &mut ui::widget::NavShell| shell.front_pane())
                .expect("Photos stands in a NavShell");
            assert_eq!(
                front,
                ui::widget::NavPane::Content,
                "{input:?} from {from:?} takes the sheet's step, not a screen's"
            );
        }
    }
}
