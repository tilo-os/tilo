//! The library's verbs, the places derived from it, and the app.
//!
//! Plain fields change through the built-in field edits every path offers
//! (`PhotoLibrary::paths().photos.item(&id).favorite.set(true)`): starring,
//! filing in a folder, people and tags, the capture time, a shared place,
//! shared analysis, the crop and the turn, an album's name and photographs.
//! The methods below are the operations that are more than setting a field.
//! Albums are items of their own; folders, people, tags, places and the
//! timeline are derived from the photographs and published as places that
//! offer the views and verbs that act on them.

use std::path::PathBuf;
use std::rc::Rc;

use tilo_content::ContentStore;
use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::effect::Answers;
use ui::tilo_resource::{self, Derived, Effect as _, Id, Invoke, Refusal, resource};
use ui::widget::{ContentKind, Element, NavList, NavShell};

use crate::actions::PhotosEffect;
use crate::app::PhotosApp;
use crate::model::{
    ImageAnalysis, LibraryPhoto, MAX_LABELS, OpenImage, PhotoLibrary, PhotoLocation, Rect01,
};

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US"]);

/// Bring a library written before every picture was named by its content
/// up to date. A picture's origin path is no longer stored, and a photograph
/// kept only by path never had bytes another device could read, so it goes.
fn upgrade_library(
    doc: &mut tilo_resource::Doc,
    from: u32,
) -> Result<(), tilo_resource::ModelError> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjId, ObjType, ROOT, ReadDoc as _, Value};
    fn map(
        doc: &tilo_resource::Doc,
        parent: &ObjId,
        key: &str,
    ) -> Result<Option<ObjId>, tilo_resource::ModelError> {
        Ok(match doc.get(parent, key)? {
            Some((Value::Object(ObjType::Map), id)) => Some(id),
            _ => None,
        })
    }
    if from != 1 {
        return Err(tilo_resource::ModelError::invalid(format!(
            "no migration from {from}"
        )));
    }
    let Some(photos) = map(doc, &ROOT, "photos")? else {
        return Ok(());
    };
    let ids: Vec<String> = doc.keys(&photos).collect();
    for id in ids {
        // A collection keeps each item under its value key, beside its order.
        let Some(photo) = map(doc, &photos, &id)? else {
            continue;
        };
        let Some(value) = map(doc, &photo, "v")? else {
            continue;
        };
        let Some(image) = map(doc, &value, "image")? else {
            continue;
        };
        if doc.get(&image, "path")?.is_some() {
            doc.delete(&image, "path")?;
        }
        let content: Option<String> = tilo_resource::read_key(doc, &image, "content")?;
        if content.is_none_or(|hash| hash.is_empty()) {
            doc.delete(&photos, id.as_str())?;
        }
    }
    Ok(())
}

/// The most photographs one verb changes at once.
pub(crate) const MAX_BATCH_PHOTOS: usize = 256;

/// The view a photograph opens in: this window, showing it. Declaring it
/// is what gives each photograph its link (`share-link`).
fn item_views(field: &str, _item: &Id) -> Vec<ui::tilo_actions::ResourceViewSpec> {
    match field {
        "photos" => vec![
            ui::tilo_actions::ResourceViewSpec::new("photo", "Photo", PhotosEffect::VIEW.id)
                .with_primary(true),
        ],
        _ => Vec::new(),
    }
}

#[resource(
    kind = "photos.library",
    label = "Photos",
    children = PhotoLibrary::places,
    item_views = item_views,
    search = crate::search::library_search_entries,
    read_content = PhotoLibrary::content,
    effects = PhotosEffect,
    version = 2,
    migrate = upgrade_library
)]
impl PhotoLibrary {
    /// Import photograph.
    ///
    /// Adds a photograph already kept in the content store. A photograph the
    /// library already has keeps what it was given.
    #[action]
    pub fn import_photo(
        &mut self,
        #[param("The photograph: its name, size in pixels and content hash")] image: OpenImage,
        #[param("When it was taken, as its file says")] captured_at: Option<String>,
        #[param("Where it was taken, as its file says")] location: Option<PhotoLocation>,
    ) -> Result<(), Refusal> {
        let mut photo = LibraryPhoto::from_image(image);
        photo.captured_at = captured_at
            .map(|value| {
                crate::model::canonical_capture_at_value(&value)
                    .ok_or("the capture time is not valid EXIF or ISO 8601")
            })
            .transpose()?;
        photo.location = location;
        photo.importable()?;
        self.add_photo(photo);
        Ok(())
    }

    /// Read again from its file.
    ///
    /// Refreshes what can be re-read from the original without overwriting
    /// deliberate organization. A capture time fills an empty one only; a
    /// location read from the file replaces only another read from a file.
    #[action(item = photos)]
    pub fn refresh_imported_metadata(
        &mut self,
        photo: &Id,
        #[param("When it was taken, as its file says")] captured_at: Option<String>,
        #[param("Where it was taken, as its file says")] location: Option<PhotoLocation>,
    ) -> Result<(), Refusal> {
        let captured_at = captured_at
            .map(|value| {
                crate::model::canonical_capture_at_value(&value)
                    .ok_or("the capture time is not valid EXIF or ISO 8601")
            })
            .transpose()?;
        if location
            .as_ref()
            .is_some_and(|value| value.provenance.source != "exif")
        {
            return Err(Refusal::new("a refreshed location is read from the file"));
        }
        let photo = self.photos.get_mut(photo).ok_or(GONE)?;
        if photo.captured_at.is_none() {
            photo.captured_at = captured_at;
        }
        if photo
            .location
            .as_ref()
            .is_none_or(|current| current.provenance.source == "exif")
        {
            photo.location = location;
        }
        Ok(())
    }

    /// Remove from library.
    ///
    /// Removes the photograph and every album's mention of it.
    #[action(item = photos, destructive)]
    pub fn remove_photo(&mut self, photo: &Id) -> Result<(), Refusal> {
        self.photos.remove(photo).ok_or(GONE)?;
        for (_, album) in self.albums.iter_mut() {
            album.photo_ids.remove(&photo.as_str().to_owned());
        }
        Ok(())
    }

    /// Rotate right.
    ///
    /// Turns the photograph a quarter turn clockwise.
    #[action(item = photos)]
    pub fn rotate_right(&mut self, photo: &Id) -> Result<(), Refusal> {
        let photo = self.photos.get_mut(photo).ok_or(GONE)?;
        photo.edits = photo.edits.rotated_right();
        Ok(())
    }

    /// Rotate left.
    ///
    /// Turns the photograph a quarter turn anticlockwise.
    #[action(item = photos)]
    pub fn rotate_left(&mut self, photo: &Id) -> Result<(), Refusal> {
        let photo = self.photos.get_mut(photo).ok_or(GONE)?;
        photo.edits = photo.edits.rotated_left();
        Ok(())
    }

    /// Crop.
    ///
    /// Keeps a region of what is shown now, turned as it is.
    #[action(item = photos)]
    pub fn crop(
        &mut self,
        photo: &Id,
        #[param("The region kept, in normalized coordinates (0 to 1) of the photograph as shown")]
        region: Rect01,
    ) -> Result<(), Refusal> {
        if !region.is_valid_selection() {
            return Err(Refusal::new(
                "the region is x, y, w, h between 0 and 1, inside the photograph",
            ));
        }
        let photo = self.photos.get_mut(photo).ok_or(GONE)?;
        photo.edits = photo.edits.cropped_from_view(region);
        Ok(())
    }

    /// Share analysis.
    ///
    /// Merges one analyzer's bounded result into what the photograph already
    /// shares. Text recognition and image analysis are independent:
    /// completing either keeps the other.
    #[action(item = photos)]
    pub fn merge_image_analysis(
        &mut self,
        photo: &Id,
        #[param("What the analyzer found, with its provenance")] analysis: ImageAnalysis,
    ) -> Result<(), Refusal> {
        if !crate::model::valid_analysis(&analysis)
            || !matches!(
                analysis.provenance.source.as_str(),
                "local-ocr" | "local-vision"
            )
        {
            return Err(Refusal::new(
                "image analysis is compact, explicitly shared, and carries analyzer provenance",
            ));
        }
        let photo = self.photos.get_mut(photo).ok_or(GONE)?;
        if !crate::model::analysis_names_exact_content(photo, &analysis) {
            return Err(Refusal::new(
                "analysis components do not name this photograph's content",
            ));
        }
        let merged = crate::model::merge_image_analysis(
            photo.analysis().as_ref(),
            analysis,
            &photo.image.content,
        );
        photo.set_analysis(Some(merged));
        Ok(())
    }

    /// Set person membership.
    ///
    /// The person is carried by exactly these photographs. An empty list
    /// removes the person.
    #[action]
    pub fn set_person_membership(
        &mut self,
        #[param("The person's name")] person: String,
        #[param("Every photograph showing them, by id")] photos: Vec<String>,
    ) -> Result<(), Refusal> {
        let person = person_label(&person)?;
        if photos.len() > MAX_BATCH_PHOTOS || !crate::model::unique_strings(&photos) {
            return Err(Refusal::new(
                "a person's photographs are each named once, at most 256",
            ));
        }
        if photos.iter().any(|id| self.photo(id).is_none()) {
            return Err(Refusal::new("a photograph is no longer in the library"));
        }
        for (id, photo) in self.photos.iter_mut() {
            if photos.iter().any(|chosen| chosen == id.as_str()) {
                photo.people.insert(person.clone());
            } else {
                photo.people.remove(&person);
            }
        }
        self.within_label_limits()
    }

    /// Rename person.
    ///
    /// Renames the person, or merges them into another person by that name.
    #[action]
    pub fn rename_person(
        &mut self,
        #[param("The person's name")] person: String,
        #[param("Their new name")] to: String,
    ) -> Result<(), Refusal> {
        let (from, to) = (person_label(&person)?, person_label(&to)?);
        let mut found = false;
        for (_, photo) in self.photos.iter_mut() {
            if photo.people.remove(&from) {
                photo.people.insert(to.clone());
                found = true;
            }
        }
        if !found {
            return Err(Refusal::new("that person is no longer in the library"));
        }
        self.within_label_limits()
    }

    /// Remove person.
    ///
    /// Their name comes off every photograph. No photograph is removed.
    #[action(destructive)]
    pub fn remove_person(
        &mut self,
        #[param("The person's name")] person: String,
    ) -> Result<(), Refusal> {
        let person = person_label(&person)?;
        let mut found = false;
        for (_, photo) in self.photos.iter_mut() {
            found |= photo.people.remove(&person);
        }
        if found {
            Ok(())
        } else {
            Err(Refusal::new("that person is no longer in the library"))
        }
    }

    /// Remove album.
    ///
    /// Its photographs stay in the library.
    #[action(item = albums, destructive)]
    pub fn remove_album(&mut self, album: &Id) -> Result<(), Refusal> {
        self.albums
            .remove(album)
            .map(drop)
            .ok_or_else(|| Refusal::new("that album is no longer in the library"))
    }
}

/// Why a verb on a photograph is refused when it is gone.
const GONE: &str = "that photograph is no longer in the library";

/// `name` as a person's label, trimmed; refused when it cannot be one.
fn person_label(name: &str) -> Result<String, Refusal> {
    if crate::model::valid_person_label(name) {
        Ok(name.trim().to_owned())
    } else {
        Err(Refusal::new(
            "a person's name is short, not blank, and has no control characters",
        ))
    }
}

impl PhotoLibrary {
    /// Refuse a change that left a photograph with more names than it may
    /// carry, rather than dropping one.
    fn within_label_limits(&self) -> Result<(), Refusal> {
        if self
            .all_photos()
            .any(|photo| photo.people.len() > MAX_LABELS)
        {
            return Err(Refusal::new(
                "a photograph carries at most 32 people; the change is refused",
            ));
        }
        Ok(())
    }

    /// What the library reads as to a caller with no screen: how many
    /// photographs and albums it holds, or one photograph or album whole —
    /// its name, size and content address, its labels and its edits.
    fn content(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        use ui::tilo_resource::Model as _;
        match item {
            None => Ok(serde_json::json!({
                "photos": self.photos.len(),
                "albums": self.albums.len(),
            })),
            Some(id) => self
                .photos
                .get(id)
                .map(LibraryPhoto::to_json)
                .or_else(|| self.albums.get(id).map(crate::model::Album::to_json))
                .ok_or_else(|| format!("there is no photograph or album {id}")),
        }
    }

    /// The places derived from the library: favorites, albums, places,
    /// people, the timeline, folders, tags, and what analysis found; and the
    /// photo analysis this device runs.
    fn places(&self) -> Vec<Derived<Self>> {
        crate::derived::derived(self)
    }
}

/// The edit that imports `photo` as an import reads it from a file.
#[must_use]
pub fn import(photo: &LibraryPhoto) -> Invoke<PhotoLibrary> {
    PhotoLibrary::IMPORT_PHOTO.with((
        photo.image.clone(),
        photo.captured_at.clone(),
        photo.location.clone(),
    ))
}

/// The edit that puts `photo`, as it is, at the end of the library: a
/// library written whole, such as a capture's.
#[must_use]
pub fn insert(photo: &LibraryPhoto) -> ui::tilo_resource::Edit<PhotoLibrary> {
    PhotoLibrary::paths()
        .photos
        .insert_as(&photo.item(), usize::MAX, photo)
}

/// The derived place `id` names among `library`'s, as published.
#[must_use]
pub fn derived_resource_id(id: &str) -> String {
    format!(
        "{}/derived/{id}",
        <PhotoLibrary as tilo_resource::Resource>::KIND
    )
}

/// A photograph's item resource id.
#[must_use]
pub fn photo_resource_id(photo: &str) -> String {
    tilo_resource::items::item_resource_id::<PhotoLibrary>("photos", &Id::from_raw(photo))
}

/// The photograph an item resource id names, if it names one.
#[must_use]
pub fn photo_of_resource(resource_id: &str) -> Option<String> {
    match tilo_resource::items::item_of_resource::<PhotoLibrary>(resource_id)? {
        ("photos", id) => Some(id.as_str().to_owned()),
        _ => None,
    }
}

/// An album's item resource id.
#[must_use]
pub fn album_resource_id(album: &str) -> String {
    tilo_resource::items::item_resource_id::<PhotoLibrary>("albums", &Id::from_raw(album))
}

/// Framework app: the photos widget is the surface over [`PhotoLibrary`].
pub struct PhotosShell;

impl ResourceApp for PhotosShell {
    type Resource = PhotoLibrary;
    const ID: &'static str = "so.tilo.photos";
    const TITLE: &'static str = "Photos";
    const ICON: Option<&'static str> = Some("photos");
    const OPENS: &'static [&'static str] = &["image/png", "image/jpeg", "image/webp"];
    /// A photograph to open, or a folder of them to import, into the library.
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<PhotoLibrary>) -> Element {
        let content = cx.content().unwrap_or_else(ContentStore::memory);
        let app = if cx.mode().is_live() {
            PhotosApp::new_persistent(content)
                .launched_on(cx.launch_arguments().first().map(PathBuf::from))
        } else {
            PhotosApp::with_content(content)
        };
        // A search hit, or an agent's `open`, shows the photograph or album
        // it names.
        cx.on_open_item(app.opener());
        // Any other place a hit names opens through the view it offers.
        let (live, dispatcher) = (cx.live().clone(), cx.dispatcher());
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                let effect = live
                    .with(|library| crate::search::opened(library, anchor))
                    .ok_or_else(|| {
                        ui::tilo_resource::Refusal::new(format!("`{anchor}` names nothing to show"))
                    })?;
                dispatcher.effect(&effect);
                Ok(())
            },
        );
        // The shell keeps the sidebar: the library's places as a list
        // (their counts and the albums under a heading make it one), its
        // width, its drawer in a narrow window and its keys.
        let places = ui::widget::NavFeed::new();
        let shell = NavShell::new()
            .with_id("photos-shell")
            .content_kind(ContentKind::Grid)
            .with_window_toolbar();
        let app = app.in_shell(&places, shell.window_bar_link());
        let sidebar = NavList::new(Vec::new())
            .with_id("photos.places")
            .with_accessible_name(crate::strings::tr("photos-library"))
            .with_headings()
            .fed_by(&places);
        Box::new(shell.sidebar(Box::new(sidebar)).content(cx.surface(app)))
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// A photograph handed to the launch is kept in the content store and
    /// imported into the library; the window shows it, and imports a folder
    /// ([`PhotosApp::launched_on`]).
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<PhotoLibrary>> {
        let (Some(file), Some(content)) = (input.file(), input.content()) else {
            return Vec::new();
        };
        let Some(image) = crate::pixels::decode(file.bytes()) else {
            tracing::warn!(
                name = file.name(),
                "photos: the launched file is not an image"
            );
            return Vec::new();
        };
        let hash = match content.put(file.bytes()) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(%error, "photos: could not keep the launched photograph");
                return Vec::new();
            }
        };
        let open = OpenImage::new(file.name(), image.w, image.h, hash);
        vec![import(&LibraryPhoto::from_image(open))]
    }

    fn scenarios() -> &'static [&'static str] {
        crate::sample::SCENARIOS
    }

    /// The photographs a scenario shows live in the content store, not the
    /// document: their bytes are put there before the view is built.
    fn fixture(fixture: &mut Fixture<PhotoLibrary>) {
        crate::sample::put_content(fixture.content());
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<PhotoLibrary>> {
        crate::sample::scenario(name)
    }
}

impl ResourceSurface<PhotoLibrary, PhotosEffect> for PhotosApp {
    fn present(&mut self, state: &Rc<PhotoLibrary>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<PhotoLibrary>) {
        self.attach_framework(cx.dispatcher(), cx.actions());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("opened"));
    }

    fn effect(
        &mut self,
        effect: PhotosEffect,
        cx: &mut EffectCx<PhotoLibrary, Self>,
    ) -> EffectAnswer {
        let job = PhotosEffect::declarations()
            .iter()
            .any(|decl| decl.id == effect.id() && matches!(decl.answers, Answers::Later { .. }));
        let invocation = cx.invocation().unwrap_or_default();
        // A job's work reports under its invocation from the moment it
        // starts, so its answer is kept before the work begins.
        if job {
            let reply = cx.reply();
            self.register_reply(invocation, reply);
        }
        let state = cx.live().get();
        match self.answer_effect(effect, invocation, &state) {
            Ok(Some(output)) if job => {
                self.settle_now(invocation, Ok(output));
                Ok(Effected::Pending)
            }
            Ok(Some(output)) => Ok(Effected::Done(output)),
            Ok(None) if job => Ok(Effected::Pending),
            Ok(None) => Ok(Effected::done()),
            Err(error) => {
                self.forget_reply(invocation);
                Err(error)
            }
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.search_status_row().into_iter().collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.search_status_row().filter(|row| row.id() == id)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::PhotosShell::scenarios() {
            let mut app = TestApp::<super::PhotosShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
