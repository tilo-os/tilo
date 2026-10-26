use serde_json::json;

use super::*;

#[test]
fn changes_name_each_value_that_differs_by_its_path() {
    let v1 = json!({
        "duration": 9.0,
        "shots": [
            { "source": "shot-a.mp4", "in": 1.0, "out": 4.0 },
            { "source": "shot-b.mp4", "in": 1.0, "out": 4.0 },
            { "source": "shot-c.mp4", "in": 1.0, "out": 4.0, "fade_out": 1.0 },
        ],
        "titles": [{ "text": "HARBOUR DAYS", "end": 2.0 }],
    });
    let v2 = json!({
        "duration": 8.5,
        "shots": [
            { "source": "shot-a.mp4", "in": 1, "out": 4 },
            { "source": "shot-b2.mp4", "in": 1.0, "out": 4.0 },
            { "source": "shot-c.mp4", "in": 1.0, "out": 4.0 },
        ],
        "titles": [{ "text": "OPENS 7 NOV", "end": 2.0 }],
        "gaps": [{ "from": 5.5 }],
    });
    assert_eq!(
        changes(&v1, &v2),
        vec![
            "duration: 9.0 -> 8.5",
            "shots[1].source: \"shot-b.mp4\" -> \"shot-b2.mp4\"",
            "shots[2].fade_out: removed (was 1.0)",
            "titles[0].text: \"HARBOUR DAYS\" -> \"OPENS 7 NOV\"",
            "gaps: added [{\"from\":5.5}]",
        ]
    );
    assert_eq!(changes_text(&v2, &v2), "nothing changed");
}

#[test]
fn lists_compare_item_by_item_and_long_changes_are_counted() {
    assert_eq!(
        changes(
            &json!({ "slides": ["Intro"] }),
            &json!({ "slides": ["Intro", "Plan"] })
        ),
        vec!["slides[1]: added \"Plan\""]
    );
    let before = json!((0..50).collect::<Vec<u32>>());
    let after = json!((1..51).collect::<Vec<u32>>());
    let found = changes(&before, &after);
    assert_eq!(found.len(), MOST_CHANGES + 1);
    assert_eq!(found.first().map(String::as_str), Some("[0]: 0 -> 1"));
    assert_eq!(found.last().map(String::as_str), Some("… and 10 more"));
}
