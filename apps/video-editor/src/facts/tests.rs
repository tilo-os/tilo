use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use uuid::Uuid;

use super::*;
use crate::VideoEditor;
use crate::model::{MediaItem, TextSource};
use crate::resource::load_project;

fn source(project: &mut Project, name: &str, kind: MediaKind, text: Option<TextSource>) -> Uuid {
    let id = Uuid::new_v4();
    project.media.push(MediaItem {
        id,
        name: name.into(),
        kind,
        duration: 4.0,
        text,
        ..MediaItem::default()
    });
    id
}

fn lane(project: &mut Project, name: &str, kind: TrackKind, index: u32) -> Uuid {
    let id = Uuid::new_v4();
    project.tracks.push(Track {
        id,
        name: name.into(),
        kind,
        index,
        ..Track::default()
    });
    id
}

fn place(project: &mut Project, track: Uuid, media: Uuid, start: f64, window: (f64, f64)) -> Uuid {
    let id = Uuid::new_v4();
    project.clips.push(Clip {
        id,
        track,
        media,
        start,
        in_point: window.0,
        out_point: window.1,
        ..Clip::default()
    });
    id
}

/// Two shots back to back on V1, the second fading out over its last
/// second and placed after a third of a second's gap; a yellow title on V2
/// over the first two seconds; the first shot's sound on A1.
fn promo() -> Project {
    let mut project = Project::new("Harbour Days promo");
    project.set_frame_size(1280, 720);
    let a = source(&mut project, "shot-a.mp4", MediaKind::Video, None);
    let c = source(&mut project, "shot-c.mp4", MediaKind::Video, None);
    let mut words = TextSource::new("HARBOUR DAYS");
    words.color = [1.0, 212.0 / 255.0, 0.0, 1.0];
    words.size = 0.12;
    let title = source(&mut project, "Title", MediaKind::Text, Some(words));
    let v1 = lane(&mut project, "V1", TrackKind::Video, 1);
    let v2 = lane(&mut project, "V2", TrackKind::Video, 2);
    let a1 = lane(&mut project, "A1", TrackKind::Audio, 1);
    place(&mut project, v1, a, 0.0, (1.0, 4.0));
    let last = place(&mut project, v1, c, 3.333, (0.5, 3.5));
    if let Some(clip) = project.clips.iter_mut().find(|clip| clip.id == last) {
        clip.fade_out = 1.0;
    }
    place(&mut project, v2, title, 0.0, (0.0, 2.0));
    place(&mut project, a1, a, 0.0, (1.0, 4.0));
    project
}

#[test]
fn a_cut_states_its_shots_titles_sound_and_gaps_to_two_decimals() {
    assert_eq!(
        of(&promo()),
        json!({
            "project": "Harbour Days promo",
            "frame": { "width": 1280, "height": 720, "fps": 30.0 },
            "duration": 6.33,
            "shots": [
                {
                    "source": "shot-a.mp4", "track": "V1", "start": 0.0, "end": 3.0,
                    "in": 1.0, "out": 4.0, "duration": 3.0, "fade_in": 0.0, "fade_out": 0.0,
                },
                {
                    "source": "shot-c.mp4", "track": "V1", "start": 3.33, "end": 6.33,
                    "in": 0.5, "out": 3.5, "duration": 3.0, "fade_in": 0.0, "fade_out": 1.0,
                },
            ],
            "gaps": [{ "track": "V1", "from": 3.0, "to": 3.33 }],
            "titles": [{
                "text": "HARBOUR DAYS", "colour": "#ffd400", "size": 0.12, "track": "V2",
                "layer": 2, "start": 0.0, "end": 2.0, "fade_in": 0.0, "fade_out": 0.0,
            }],
            "audio": [{
                "track": "A1",
                "clips": [{
                    "source": "shot-a.mp4", "track": "A1", "start": 0.0, "end": 3.0,
                    "in": 1.0, "out": 4.0, "duration": 3.0, "fade_in": 0.0, "fade_out": 0.0,
                }],
            }],
        })
    );
}

#[test]
fn an_empty_project_states_no_length_and_a_render_range_is_stated() {
    let mut project = Project::new("cut");
    assert_eq!(of(&project)["duration"], 0.0);
    assert!(of(&project).get("render_range").is_none());
    project.set_mark_in(1.0);
    assert_eq!(
        of(&project)["render_range"],
        json!({ "start": 1.0, "end": 1.0 })
    );
}

#[test]
fn the_project_publishes_its_facts_as_a_read() {
    let mut app = TestApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&promo())).unwrap())
        .unwrap();
    let answer = app.call("facts", json!({})).unwrap().unwrap();
    assert_eq!(answer, of(&promo()));
}
