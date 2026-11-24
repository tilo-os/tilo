//! The engine's own surface: a job is taken, and an op it does not know is
//! refused in its words.

use super::*;

#[test]
fn a_job_nobody_started_answers_nothing() {
    assert!(!cancel("no-such-job"));
    assert!(call("take", r#"{"job":"no-such-job","url":"https://x.test/f"}"#).is_err());
}

#[test]
fn an_op_the_engine_does_not_know_is_refused_in_its_words() {
    let refused = call("paint", "{}").unwrap_err();
    assert!(refused.contains("is no op this device runs"), "{refused}");
    let refused = start("job-1", "paint", "{}", Arc::new(|_, _| {})).unwrap_err();
    assert!(refused.contains("is no op this device runs"), "{refused}");
}

#[test]
fn an_opml_file_is_read_and_written_through_the_engine() {
    let text = r#"<?xml version="1.0"?>
<opml version="2.0"><body>
  <outline text="Reading">
    <outline text="LWN" xmlUrl="https://lwn.net/headlines/rss" htmlUrl="https://lwn.net"/>
  </outline>
  <outline text="Top" xmlUrl="https://example.org/feed"/>
</body></opml>"#;
    let input = serde_json::json!({ "text": text }).to_string();
    let read = call("opml", &input).expect("an opml file reads");
    let read: serde_json::Value = serde_json::from_str(&read).expect("json");
    let outlines: Vec<serde_json::Value> = read["outlines"]
        .as_array()
        .expect("outlines")
        .iter()
        .map(|said| {
            serde_json::from_str(said.as_str().expect("an outline's JSON text")).expect("json")
        })
        .collect();
    assert_eq!(outlines.len(), 2);
    assert_eq!(outlines[0]["title"], "LWN");
    assert_eq!(outlines[0]["folder"], "Reading");
    assert_eq!(outlines[1]["folder"], "");

    let export = serde_json::json!({ "feeds": [
        { "title": "LWN", "url": "https://lwn.net/headlines/rss", "site": "https://lwn.net", "format": "rss", "folder": "Reading" },
        { "title": "Top", "url": "https://example.org/feed", "format": "atom", "folder": "" },
    ]})
    .to_string();
    let written = call("opml-export", &export).expect("an export writes");
    let written: serde_json::Value = serde_json::from_str(&written).expect("json");
    let text = written["text"].as_str().expect("text");
    let again = opml::parse(text);
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].title, "LWN");
    assert_eq!(again[0].folder, "Reading");
    assert_eq!(again[1].title, "Top");
    assert!(text.contains(r#"type="atom""#), "{text}");
}
