//! Titles and clip edits by name, on the project itself: a title added
//! (`add-title`) or restyled (`set-title`), a clip's fades, levels and head
//! transition set (`set-clip`), and a clip removed (`remove-clip`).
//!
//! The same edits exist on each title's bin row and each clip's row, but a
//! caller with no screen reads what an empty project offers and a capped
//! list of operations, so it never saw them. Declared on the project, they
//! are there before the first clip is, and each builds the editor's own
//! action — the media write the title inspector makes, the field patch a
//! slider makes, the delete the trash button makes — so it replicates,
//! undoes and replays like the edit a person makes.

use automerge::{Patch, PatchAction, Prop};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use ui::tilo_resource::{ActionDecl, Doc};
use uuid::Uuid;

use crate::actions::{
    TRANSITION_CHOICES, VideoEditorAction, clip_field_patch_action, delete_clips_action,
    parse_colour, parse_transition, upsert_media_action,
};
use crate::doc::VideoDoc;
use crate::model::{Clip, MediaItem, MediaKind, Project, TextAlign, TextSource};
use crate::param::Param;
use crate::placement::{apply_fades, clip_named, fade_params, media_named, touched};
use crate::resource::VideoProject;

/// The id of the declaration that adds a title to the bin.
pub const ADD_TITLE: &str = "add-title";
/// The id of the declaration that changes a title's words or look.
pub const SET_TITLE: &str = "set-title";
/// The id of the declaration that sets a placed clip's fades and levels.
pub const SET_CLIP: &str = "set-clip";
/// The id of the declaration that removes a placed clip.
pub const REMOVE_CLIP: &str = "remove-clip";

/// The smallest and largest title size: a cap height a viewer can still
/// read, and one that still fits the frame.
const TITLE_SIZE: (f64, f64) = (0.02, 1.0);

/// The declarations bound to `project`.
pub(crate) fn declarations(project: &Project) -> Vec<ActionDecl<VideoProject>> {
    vec![
        add_title(project),
        set_title(project),
        set_clip(project),
        remove_clip(project),
    ]
}

/// What `action`, one of these declarations, did once it applied to `doc`
/// as `changes`.
pub(crate) fn answer(doc: &Doc, action: &str, changes: &[Patch]) -> Option<serde_json::Value> {
    let project = || VideoDoc::load_project(doc);
    match action {
        ADD_TITLE | SET_TITLE => {
            let project = project();
            let media = touched(changes, "media")
                .into_iter()
                .find_map(|id| project.media(id))?;
            Some(serde_json::json!({
                "media": crate::timeline::media_json(&project, media),
            }))
        }
        SET_CLIP => {
            let project = project();
            let clip = touched(changes, "clips")
                .into_iter()
                .find_map(|id| project.clip(id))?;
            Some(serde_json::json!({
                "clip": crate::timeline::clip_json(&project, clip),
                "timeline": crate::timeline::timeline_json(&project),
            }))
        }
        REMOVE_CLIP => {
            let project = project();
            let removed: Vec<String> = deleted(changes, "clips")
                .iter()
                .map(ToString::to_string)
                .collect();
            let moved: Vec<serde_json::Value> = touched(changes, "clips")
                .into_iter()
                .filter_map(|id| project.clip(id))
                .map(|clip| crate::timeline::clip_json(&project, clip))
                .collect();
            Some(serde_json::json!({
                "removed": removed,
                "moved": moved,
                "timeline": crate::timeline::timeline_json(&project),
            }))
        }
        _ => None,
    }
}

/// The ids of the entries of the root map `map` that `changes` deleted.
fn deleted(changes: &[Patch], map: &str) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for patch in changes {
        if let ([(_, Prop::Map(root))], PatchAction::DeleteMap { key }) =
            (patch.path.as_slice(), &patch.action)
            && root == map
            && let Ok(id) = key.parse::<Uuid>()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

/// The look parameters `add-title` and `set-title` share.
fn style_params() -> [ActionParam; 3] {
    [
        ActionParam::optional(
            "colour",
            ParamType::Text,
            "The words' colour as #rrggbb, e.g. #FFD400; default white",
        ),
        ActionParam::optional(
            "size",
            ParamType::Number,
            "Cap height as a fraction of the frame's height, 0.02 to 1; default 0.12",
        ),
        ActionParam::optional(
            "align",
            ParamType::Text,
            "Where the lines sit across the frame; default center",
        )
        .with_choices(TextAlign::ALL.map(TextAlign::as_str)),
    ]
}

/// Set the look `args` give on `text`. A colour or size that cannot be read
/// is refused by name rather than left as it was: a caller that asked for
/// yellow and got white has no way to tell.
fn apply_style(text: &mut TextSource, args: &ActionArgs) -> Result<(), String> {
    if let Some(colour) = args.text("colour") {
        text.color = parse_colour(colour)
            .ok_or_else(|| format!("`colour` is #rrggbb, e.g. #FFD400, not `{colour}`"))?;
    }
    if let Some(size) = args.number("size") {
        let (least, most) = TITLE_SIZE;
        if !(least..=most).contains(&size) {
            return Err(format!("`size` is from {least} to {most}, not {size}"));
        }
        text.size = size;
    }
    if let Some(align) = args.text("align") {
        text.align = TextAlign::from_name(align);
    }
    Ok(())
}

/// A title's bin name: the first line of its words.
fn title_name(words: &str) -> String {
    words.lines().next().unwrap_or("Title").to_owned()
}

/// `add-title`: words set over the picture, a source in the bin. The id is
/// minted here, at authoring time, so the action replays byte-identically on
/// a peer.
fn add_title(project: &Project) -> ActionDecl<VideoProject> {
    let palette = (project.media.len() % 7) as u8;
    ActionDecl::new(
        ADD_TITLE,
        "Add title",
        std::iter::once(ActionParam::required(
            "text",
            ParamType::Text,
            "The words the title shows; one line per line break; empty reads as “Title”",
        ))
        .chain(style_params())
        .collect(),
        move |args, _now| {
            let words = args.text("text").unwrap_or_default().trim();
            let content = if words.is_empty() { "Title" } else { words };
            let mut text = TextSource::new(content);
            apply_style(&mut text, args)?;
            let item = MediaItem {
                id: Uuid::new_v4(),
                name: title_name(content),
                kind: MediaKind::Text,
                // Long enough to be trimmed down rather than stretched.
                duration: 5.0,
                palette,
                // A title has no bytes: nothing to hydrate, nothing to
                // fetch, and it renders on every peer immediately.
                path: String::new(),
                hydration: crate::media::Hydration::Local,
                text: Some(text),
                ..Default::default()
            };
            Ok(upsert_media_action(&item))
        },
    )
    .describe(
        "Add a title (words set over the picture) to the bin, with its colour, size and \
         alignment. Nothing is placed: place it with add-clip on a track above the footage (V2). \
         Answers the title's id and name.",
    )
    .answering()
}

/// The title an argument names: a bin source that is one.
fn title_named<'a>(project: &'a Project, named: &str) -> Result<&'a MediaItem, String> {
    let media = media_named(project, named)?;
    if media.kind == MediaKind::Text {
        Ok(media)
    } else {
        Err(format!(
            "{} is {}, not a title",
            media.name,
            media.kind.label().to_lowercase()
        ))
    }
}

/// `set-title`: a title's words, colour, size or alignment, on every clip
/// that places it.
fn set_title(project: &Project) -> ActionDecl<VideoProject> {
    let project = project.clone();
    ActionDecl::new(
        SET_TITLE,
        "Change a title",
        [
            ActionParam::required(
                "title",
                ParamType::Text,
                "The title: its id (as add-title answered or read-timeline lists it) or its name \
                 in the bin",
            ),
            ActionParam::optional(
                "text",
                ParamType::Text,
                "New words; the title is renamed after their first line",
            ),
        ]
        .into_iter()
        .chain(style_params())
        .collect(),
        move |args, _now| {
            let title = title_named(&project, args.text("title").unwrap_or_default())?;
            let mut next = title.clone();
            let mut text = next.text.clone().unwrap_or_default();
            if let Some(words) = args.text("text").map(str::trim) {
                if words.is_empty() {
                    return Err("`text` is the title's new words; it may not be empty".into());
                }
                words.clone_into(&mut text.content);
                next.name = title_name(words);
            }
            apply_style(&mut text, args)?;
            next.text = Some(text);
            if next == *title {
                return Err(
                    "give what changes: `text`, `colour`, `size` or `align`, different from \
                     what the title has"
                        .into(),
                );
            }
            Ok(upsert_media_action(&next))
        },
    )
    .describe(
        "Change a title's words, colour, size or alignment; every clip placing it shows the \
         change. Answers the title.",
    )
    .answering()
}

/// A level argument `name` (`opacity`, `volume`): absent, or 0 to 1.
fn level_arg(args: &ActionArgs, name: &str) -> Result<Option<f64>, String> {
    match args.number(name) {
        None => Ok(None),
        Some(value) if (0.0..=1.0).contains(&value) => Ok(Some(value)),
        Some(value) => Err(format!("`{name}` is from 0 to 1, not {value}")),
    }
}

/// Set a clip's level `param` to `value`. A keyframed level is refused: the
/// curve wins at every instant, so a constant written under it would report
/// success and change nothing.
fn set_level(clip: &mut Clip, param: Param, name: &str, value: f64) -> Result<(), String> {
    if clip.anim.is_animated(param) {
        return Err(format!(
            "the clip's {name} is keyframed; change it on the clip's own row at a time"
        ));
    }
    clip.set_param_const(param, value);
    Ok(())
}

/// Build `set-clip` from its arguments against `project`.
fn edit_clip(project: &Project, args: &ActionArgs) -> Result<VideoEditorAction, String> {
    let clip = clip_named(project, args.text("clip").unwrap_or_default())?;
    if !project.clip_editable(clip.id) {
        return Err("the clip's track is locked".into());
    }
    let mut next = clip.clone();
    apply_fades(&mut next, args)?;
    if let Some(opacity) = level_arg(args, "opacity")? {
        set_level(&mut next, Param::Opacity, "opacity", opacity)?;
    }
    if let Some(volume) = level_arg(args, "volume")? {
        set_level(&mut next, Param::Volume, "volume", volume)?;
    }
    if let Some(name) = args.text("transition") {
        next.transition = parse_transition(name);
    }
    match clip_field_patch_action(clip, &next) {
        VideoEditorAction::PatchClipFields { changes, .. } if changes.is_empty() => Err(
            "give what changes: `fade_in`, `fade_out`, `opacity`, `volume` or `transition`, \
             different from what the clip has"
                .into(),
        ),
        action => Ok(action),
    }
}

/// `set-clip`: a placed clip's fades, levels and head transition.
fn set_clip(project: &Project) -> ActionDecl<VideoProject> {
    let project = project.clone();
    ActionDecl::new(
        SET_CLIP,
        "Set a clip's fades and levels",
        std::iter::once(ActionParam::required(
            "clip",
            ParamType::Text,
            "The clip's id, as add-clip answered or read-timeline lists it",
        ))
        .chain(fade_params())
        .chain([
            ActionParam::optional(
                "opacity",
                ParamType::Number,
                "How opaque the picture is, 0 to 1 (1 is as shot)",
            ),
            ActionParam::optional(
                "volume",
                ParamType::Number,
                "How loud the sound is, 0 to 1 (1 is as recorded, 0 silent)",
            ),
            ActionParam::optional(
                "transition",
                ParamType::Text,
                "The transition into the clip from the one before it; cut for none",
            )
            .with_choices(TRANSITION_CHOICES),
        ])
        .collect(),
        move |args, _now| edit_clip(&project, args),
    )
    .describe(
        "Set a placed clip's fade in, fade out, opacity, volume or incoming transition; what \
         is not given stays as it is. Answers the clip and the timeline.",
    )
    .answering()
}

/// `remove-clip`: a placed clip off the timeline, with the clips linked to
/// it, and with `ripple` the gap it leaves closed.
fn remove_clip(project: &Project) -> ActionDecl<VideoProject> {
    let project = project.clone();
    ActionDecl::new(
        REMOVE_CLIP,
        "Remove a clip",
        vec![
            ActionParam::required(
                "clip",
                ParamType::Text,
                "The clip's id, as add-clip answered or read-timeline lists it",
            ),
            ActionParam::optional(
                "ripple",
                ParamType::Boolean,
                "Close the gap: the later clips on its track move earlier by its length; \
                 default false, leaving the gap",
            ),
        ],
        move |args, _now| {
            let clip = clip_named(&project, args.text("clip").unwrap_or_default())?;
            if !project.clip_editable(clip.id) {
                return Err("the clip's track is locked".into());
            }
            if args.boolean("ripple").unwrap_or(false) {
                let mut rippled = project.clone();
                let (removed, moved_ids) = rippled.ripple_delete_linked(clip.id);
                let moved: Vec<Clip> = moved_ids
                    .iter()
                    .filter_map(|id| rippled.clip(*id).cloned())
                    .collect();
                Ok(delete_clips_action(&removed, &moved))
            } else {
                Ok(delete_clips_action(&project.link_group(clip.id), &[]))
            }
        },
    )
    .describe(
        "Remove a placed clip from the timeline, and the clips linked to it; with `ripple`, \
         the later clips on its track move up to close the gap. Its source stays in the bin. \
         Answers the removed clip ids, the clips moved and the timeline.",
    )
    .destructive()
    .answering()
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "times built from exact literals compare exactly"
)]
mod tests {
    use super::*;
    use crate::placement::{ADD_CLIP, MOVE_CLIP};
    use tilo_actions::Action as _;

    /// A blank project document, edited through the project's declarations.
    struct Fixture {
        doc: Doc,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                doc: automerge::AutoCommit::new(),
            }
        }

        fn project(&self) -> Project {
            VideoDoc::load_project(&self.doc)
        }

        /// Run declaration `id` with `arguments`, applying what it builds.
        fn run(&mut self, id: &str, arguments: serde_json::Value) -> Result<(), String> {
            let project = self.project();
            let decl = crate::placement::declarations(&project)
                .into_iter()
                .find(|decl| decl.id == id)
                .ok_or("not declared")?;
            let serde_json::Value::Object(arguments) = arguments else {
                return Err("arguments are an object".into());
            };
            let args = ActionArgs::bind_json(&decl.params, &arguments)
                .map_err(|error| format!("{error:?}"))?;
            let action = (decl.build)(&args, 0)?;
            action.apply(&mut self.doc).map_err(|e| e.to_string())
        }

        /// Run `id` and answer as the resource answers it.
        fn answered(&mut self, id: &str, arguments: serde_json::Value) -> serde_json::Value {
            let heads = self.doc.get_heads();
            self.run(id, arguments).expect("the declaration applies");
            let after = self.doc.get_heads();
            let changes = self.doc.diff(&heads, &after);
            crate::placement::answer(&self.doc, id, &changes).expect("it answers")
        }

        /// A video source `name` lasting `duration` seconds, in the bin.
        fn footage(&mut self, name: &str, duration: f64) {
            let item = MediaItem {
                id: Uuid::new_v4(),
                name: name.into(),
                kind: MediaKind::Video,
                duration,
                ..Default::default()
            };
            upsert_media_action(&item)
                .apply(&mut self.doc)
                .expect("the media is written");
        }

        /// Place `media` on V1 after what is there, reading `out` seconds;
        /// the new clip's id.
        fn place(&mut self, media: &str, out: f64) -> String {
            let placed = self.answered(ADD_CLIP, serde_json::json!({"media": media, "out": out}));
            placed["clip"].as_str().expect("a clip id").to_owned()
        }

        fn clip(&self, id: &str) -> Clip {
            self.project()
                .clip(id.parse().expect("a clip id"))
                .cloned()
                .expect("the clip is on the timeline")
        }
    }

    #[test]
    fn a_coloured_title_is_added_answered_by_id_and_placed_with_a_fade() {
        let mut fixture = Fixture::new();
        fixture.footage("Harbour", 8.0);
        fixture.place("Harbour", 4.0);

        let added = fixture.answered(
            ADD_TITLE,
            serde_json::json!({"text": "HARBOUR DAYS", "colour": "#FFD400", "size": 0.1,
                "align": "left"}),
        );
        let media = &added["media"];
        assert_eq!(media["name"], "HARBOUR DAYS", "{added}");
        assert_eq!(media["kind"], "text");
        assert_eq!(media["title"]["color"], "#ffd400", "{added}");
        assert_eq!(media["title"]["size"], 0.1);
        assert_eq!(media["title"]["align"], "left");

        let placed = fixture.answered(
            ADD_CLIP,
            serde_json::json!({"media": media["id"], "track": "V2", "at": 0.0, "out": 2.0,
                "fade_in": 0.25, "fade_out": 0.5}),
        );
        let title = &placed["timeline"]["tracks"][0]["clips"][0];
        assert_eq!(title["title"], "HARBOUR DAYS", "{placed}");
        assert_eq!(
            (title["fade_in"].as_f64(), title["fade_out"].as_f64()),
            (Some(0.25), Some(0.5))
        );
        let bin = placed["timeline"]["media"].as_array().expect("the bin");
        assert!(
            bin.iter().any(|item| item["title"]["color"] == "#ffd400"),
            "the timeline shows the colour: {bin:?}"
        );
    }

    #[test]
    fn a_title_with_an_unreadable_colour_or_size_is_refused_by_name() {
        let mut fixture = Fixture::new();
        let yellow = fixture
            .run(
                ADD_TITLE,
                serde_json::json!({"text": "Hi", "colour": "yellow"}),
            )
            .unwrap_err();
        assert!(yellow.contains("#rrggbb"), "{yellow}");
        let huge = fixture
            .run(ADD_TITLE, serde_json::json!({"text": "Hi", "size": 3.0}))
            .unwrap_err();
        assert!(huge.contains("`size`"), "{huge}");
        assert!(fixture.project().media.is_empty(), "nothing was added");
    }

    #[test]
    fn a_fade_longer_than_the_clip_is_refused_when_placing_or_moving_it() {
        let mut fixture = Fixture::new();
        fixture.footage("Harbour", 8.0);
        let long = fixture
            .run(
                ADD_CLIP,
                serde_json::json!({"media": "Harbour", "out": 2.0, "fade_out": 3.0}),
            )
            .unwrap_err();
        assert!(long.contains("2.000s"), "{long}");

        let clip = fixture.place("Harbour", 2.0);
        fixture
            .run(
                MOVE_CLIP,
                serde_json::json!({"clip": clip, "start": 1.0, "fade_out": 1.0}),
            )
            .expect("the move applies");
        let moved = fixture.clip(&clip);
        assert_eq!(
            (moved.start, moved.fade_out),
            (1.0, 1.0),
            "moved and faded in one change"
        );
    }

    #[test]
    fn set_title_restyles_a_title_named_by_its_name_or_id() {
        let mut fixture = Fixture::new();
        let added = fixture.answered(ADD_TITLE, serde_json::json!({"text": "HARBOUR WEEKEND"}));
        let id = added["media"]["id"].as_str().expect("an id").to_owned();

        let restyled = fixture.answered(
            SET_TITLE,
            serde_json::json!({"title": "HARBOUR WEEKEND", "text": "OPENS 7 NOV",
                "colour": "#ffd400"}),
        );
        assert_eq!(restyled["media"]["id"], id.as_str());
        assert_eq!(
            restyled["media"]["name"], "OPENS 7 NOV",
            "renamed after its words"
        );
        assert_eq!(restyled["media"]["title"]["text"], "OPENS 7 NOV");
        assert_eq!(restyled["media"]["title"]["color"], "#ffd400");

        let resized = fixture.answered(SET_TITLE, serde_json::json!({"title": id, "size": 0.2}));
        assert_eq!(resized["media"]["title"]["size"], 0.2);
        assert_eq!(
            resized["media"]["title"]["color"], "#ffd400",
            "the colour is kept"
        );

        let nothing = fixture
            .run(SET_TITLE, serde_json::json!({"title": "OPENS 7 NOV"}))
            .unwrap_err();
        assert!(nothing.contains("give what changes"), "{nothing}");
    }

    #[test]
    fn set_title_refuses_a_source_that_is_not_a_title() {
        let mut fixture = Fixture::new();
        fixture.footage("Harbour", 8.0);
        let footage = fixture
            .run(
                SET_TITLE,
                serde_json::json!({"title": "Harbour", "colour": "#ffffff"}),
            )
            .unwrap_err();
        assert!(footage.contains("not a title"), "{footage}");
    }

    #[test]
    fn set_clip_sets_fades_levels_and_transition_and_keeps_the_rest() {
        let mut fixture = Fixture::new();
        fixture.footage("Harbour", 8.0);
        fixture.place("Harbour", 3.0);
        let last = fixture.place("Harbour", 3.0);

        let set = fixture.answered(
            SET_CLIP,
            serde_json::json!({"clip": last, "fade_out": 1.0, "opacity": 0.5,
                "transition": "dissolve"}),
        );
        assert_eq!(set["clip"]["id"], last.as_str(), "{set}");
        assert_eq!(set["clip"]["fade_out"], 1.0);
        assert_eq!(set["clip"]["opacity"], 0.5);
        assert_eq!(set["clip"]["transition"]["kind"], "dissolve");

        fixture
            .run(SET_CLIP, serde_json::json!({"clip": last, "volume": 0.25}))
            .expect("the volume is set");
        let clip = fixture.clip(&last);
        assert_eq!((clip.fade_out, clip.opacity, clip.volume), (1.0, 0.5, 0.25));

        let loud = fixture
            .run(SET_CLIP, serde_json::json!({"clip": last, "volume": 2.0}))
            .unwrap_err();
        assert!(loud.contains("0 to 1"), "{loud}");
        let long = fixture
            .run(SET_CLIP, serde_json::json!({"clip": last, "fade_in": 4.0}))
            .unwrap_err();
        assert!(long.contains("3.000s"), "{long}");
        let same = fixture
            .run(SET_CLIP, serde_json::json!({"clip": last, "fade_out": 1.0}))
            .unwrap_err();
        assert!(same.contains("give what changes"), "{same}");
    }

    #[test]
    fn remove_clip_leaves_the_gap_and_with_ripple_closes_it() {
        let mut fixture = Fixture::new();
        fixture.footage("Harbour", 8.0);
        let first = fixture.place("Harbour", 2.0);
        let second = fixture.place("Harbour", 2.0);
        let third = fixture.place("Harbour", 2.0);

        let gap = fixture.answered(REMOVE_CLIP, serde_json::json!({"clip": first}));
        assert_eq!(gap["removed"], serde_json::json!([first]), "{gap}");
        assert_eq!(gap["moved"], serde_json::json!([]));
        assert_eq!(fixture.clip(&second).start, 2.0, "the gap is left");

        let closed = fixture.answered(
            REMOVE_CLIP,
            serde_json::json!({"clip": format!("video.clip:{second}"), "ripple": true}),
        );
        assert_eq!(closed["removed"], serde_json::json!([second]), "{closed}");
        assert_eq!(closed["moved"][0]["id"], third.as_str());
        assert_eq!(
            fixture.clip(&third).start,
            2.0,
            "moved up by the clip's length"
        );
        let project = fixture.project();
        assert_eq!(project.clips.len(), 1);
        assert_eq!(project.media.len(), 1, "the source stays in the bin");

        let gone = fixture
            .run(REMOVE_CLIP, serde_json::json!({"clip": first}))
            .unwrap_err();
        assert!(gone.contains("no clip"), "{gone}");
    }

    #[test]
    fn an_empty_project_offers_the_title_and_clip_edits() {
        let offered: Vec<&str> = crate::placement::declarations(&Project::new("Untitled"))
            .iter()
            .map(|decl| decl.id)
            .collect();
        for id in [
            ADD_TITLE,
            SET_TITLE,
            SET_CLIP,
            REMOVE_CLIP,
            ADD_CLIP,
            MOVE_CLIP,
        ] {
            assert!(offered.contains(&id), "{id}: {offered:?}");
        }
    }
}
