//! Evaluate the production geometric decoder on a bounded, pinned MIT gesture
//! validation prefix. Run on an approved remote builder; never use private typing.
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use osk::{GeometricSwipeModel, KeyAction, KeyboardLayout};
use serde::Deserialize;
use serde_json::json;
use vello::kurbo::{Point, Rect};

#[derive(Deserialize)]
struct Gesture {
    word: String,
    width: f64,
    height: f64,
    points: Vec<Sample>,
}
#[derive(Deserialize)]
struct Sample {
    x: f64,
    y: f64,
}
#[derive(Deserialize)]
struct SourceLayout {
    keys: Vec<SourceKey>,
}
#[derive(Deserialize)]
struct SourceKey {
    letter: String,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let directory = args
        .next()
        .ok_or("usage: swipe_eval DIRECTORY [--fetch-validation] [LIMIT]")?;
    if args.next().as_deref() == Some("--fetch-validation") {
        let limit = args.next().unwrap_or_else(|| "256".into());
        let mut child = Command::new("python3")
            .args(["-", &directory, "--limit", &limit])
            .stdin(Stdio::piped())
            .spawn()?;
        std::io::Write::write_all(
            child.stdin.take().as_mut().ok_or("Python stdin missing")?,
            include_bytes!("../tools/fetch-swipe-validation.py"),
        )?;
        if !child.wait()?.success() {
            return Err("gesture fixture import failed".into());
        }
    }
    evaluate(Path::new(&directory))
}

fn geometry(
    layout: &KeyboardLayout,
    source: &SourceLayout,
    width: f64,
    height: f64,
) -> Vec<(osk::KeyId, Rect)> {
    layout
        .rows
        .iter()
        .enumerate()
        .flat_map(|(row, keys)| {
            keys.iter().enumerate().filter_map(move |(col, key)| {
                let KeyAction::Char(letter) = &key.action else {
                    return None;
                };
                let k = source.keys.iter().find(|k| &k.letter == letter)?;
                Some((
                    osk::KeyId::new(row, col),
                    Rect::new(
                        (k.cx - k.rx) * width,
                        (k.cy - k.ry) * height,
                        (k.cx + k.rx) * width,
                        (k.cy + k.ry) * height,
                    ),
                ))
            })
        })
        .collect()
}

fn evaluate(directory: &Path) -> Result<(), Box<dyn Error>> {
    let source: SourceLayout = serde_json::from_slice(&fs::read(directory.join("layout.json"))?)?;
    let layout = KeyboardLayout::qwerty();
    let model = GeometricSwipeModel::english();
    let mut total = 0u32;
    let mut eligible = 0u32;
    let mut vocabulary = 0u32;
    let mut top = [0u32; 3];
    let mut durations = Vec::new();
    for line in BufReader::new(File::open(directory.join("validation.jsonl"))?).lines() {
        let gesture: Gesture = serde_json::from_str(&line?)?;
        total = total.saturating_add(1);
        if gesture.word.chars().count() < 2 || gesture.points.len() < 2 {
            continue;
        }
        eligible = eligible.saturating_add(1);
        vocabulary =
            vocabulary.saturating_add(u32::from(model.dictionary().contains(&gesture.word)));
        let path: Vec<_> = gesture
            .points
            .iter()
            .map(|p| Point::new(p.x * gesture.width, p.y * gesture.height))
            .collect();
        let geom = geometry(&layout, &source, gesture.width, gesture.height);
        let now = Instant::now();
        let candidates = model.decode_top(&path, &geom, &layout, 8);
        durations.push(now.elapsed().as_micros());
        for (count, n) in top.iter_mut().zip([1, 3, 8]) {
            if candidates.iter().take(n).any(|c| c.word == gesture.word) {
                *count = count.saturating_add(1);
            }
        }
    }
    if eligible == 0 {
        return Err("no eligible gestures observed".into());
    }
    durations.sort_unstable();
    let p50 = durations
        .get(durations.len() / 2)
        .copied()
        .unwrap_or_default();
    let p95 = durations
        .get(durations.len().saturating_mul(95) / 100)
        .copied()
        .unwrap_or_default();
    let report = json!({"decoder":"production geometric", "records":total,
        "eligible":eligible,"in_vocabulary":vocabulary,"top1_correct":top.first(),
        "top3_correct":top.get(1),"top8_correct":top.get(2),
        "decode_p50_us":p50,"decode_p95_us":p95,
        "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "manifest":serde_json::from_slice::<serde_json::Value>(&fs::read(directory.join("manifest.json"))?)?});
    fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
