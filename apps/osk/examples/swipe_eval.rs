//! Evaluate the production geometric decoder on a bounded, pinned MIT gesture
//! validation prefix. Run on an approved remote builder; never use private typing.
#[path = "swipe_eval/report.rs"]
mod report;

use std::collections::HashSet;
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
    #[serde(default)]
    session: Option<u32>,
    #[serde(default)]
    group: String,
    #[serde(default)]
    orientation: String,
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
    if directory == "--source-info" {
        return run_importer(&["--source-info"]);
    }
    if directory == "--compare" {
        let inputs: Vec<_> = args.collect();
        if inputs.len() != 3 {
            return Err("--compare BASELINE CANDIDATE OUTPUT".into());
        }
        let mut child = Command::new("python3")
            .arg("-")
            .args(inputs)
            .stdin(Stdio::piped())
            .spawn()?;
        std::io::Write::write_all(
            child.stdin.take().as_mut().ok_or("Python stdin missing")?,
            include_bytes!("../tools/compare-swipe-reports.py"),
        )?;
        return if child.wait()?.success() {
            Ok(())
        } else {
            Err("paired report comparison failed".into())
        };
    }
    let mut elastic = false;
    let mut prior = 100;
    let mut group = None;
    while let Some(option) = args.next() {
        match option.as_str() {
            mode @ ("--fetch-validation" | "--fetch-full-validation" | "--fetch-full-test") => {
                let extra = if mode == "--fetch-full-test" {
                    vec!["--full-test".to_string()]
                } else if mode == "--fetch-full-validation" {
                    vec!["--full-validation".to_string()]
                } else {
                    vec![
                        "--limit".into(),
                        args.next().unwrap_or_else(|| "256".into()),
                    ]
                };
                let mut child = Command::new("python3")
                    .args(["-", &directory])
                    .args(extra)
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
            "--elastic" => elastic = true,
            "--elastic-prior" => {
                prior = args.next().ok_or("missing elastic prior")?.parse::<u8>()?;
                elastic = true;
            }
            "--analysis-only" => group = Some("analysis"),
            "--qualification-only" => group = Some("qualification"),
            _ => return Err("unknown evaluation option".into()),
        }
    }
    evaluate(Path::new(&directory), elastic, prior, group)
}

fn run_importer(args: &[&str]) -> Result<(), Box<dyn Error>> {
    let mut child = Command::new("python3")
        .arg("-")
        .args(args)
        .stdin(Stdio::piped())
        .spawn()?;
    std::io::Write::write_all(
        child.stdin.take().as_mut().ok_or("Python stdin missing")?,
        include_bytes!("../tools/fetch-swipe-validation.py"),
    )?;
    if !child.wait()?.success() {
        return Err("fixture verification/import failed".into());
    }
    Ok(())
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

fn evaluate(
    directory: &Path,
    elastic: bool,
    prior: u8,
    group: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    run_importer(&[
        directory.to_str().ok_or("non-UTF8 fixture path")?,
        "--verify-only",
    ])?;
    let source: SourceLayout = serde_json::from_slice(&fs::read(directory.join("layout.json"))?)?;
    let layout = KeyboardLayout::qwerty();
    let model = GeometricSwipeModel::english();
    let model = if elastic {
        model.with_elastic_prior(prior)
    } else {
        model.with_classic_alignment()
    };
    let alignment = if elastic { "elastic" } else { "classic" };
    let label = if elastic {
        format!("elastic-{prior}-{}", group.unwrap_or("all"))
    } else {
        format!("classic-{}", group.unwrap_or("all"))
    };
    let lexicon: HashSet<_> = model
        .dictionary()
        .words()
        .iter()
        .map(|entry| report::lexical(&entry.word))
        .collect();
    let mut stats = report::Report::default();
    let mut errors = File::create(directory.join(format!("analysis-errors-{label}.jsonl")))?;
    let mut durations = Vec::new();
    for line in BufReader::new(File::open(directory.join("validation.jsonl"))?).lines() {
        let gesture: Gesture = serde_json::from_str(&line?)?;
        if group.is_some_and(|group| group != gesture.group) {
            continue;
        }
        let path: Vec<_> = gesture
            .points
            .iter()
            .map(|p| Point::new(p.x * gesture.width, p.y * gesture.height))
            .collect();
        let geom = geometry(&layout, &source, gesture.width, gesture.height);
        let now = Instant::now();
        let candidates = model.decode_top(&path, &geom, &layout, 64);
        if path.len() >= 2 && gesture.word.chars().count() >= 2 {
            durations.push(now.elapsed().as_micros());
        }
        stats.observe(
            &report::Case::new(&gesture, &model, &lexicon, &candidates),
            &mut errors,
        )?;
    }
    if stats.overall.literal.eligible == 0 {
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
    let report = json!({"decoder":"production geometric", "diagnostic_top_n":64, "alignment":alignment, "elastic_prior_hundredths":prior, "evaluation_group":group,
        "statistics":stats,
        "decode_p50_us":p50,"decode_p95_us":p95,
        "normalization":"literal lowercase and separately ASCII alphabetic after accent folding; OOV count as misses",
        "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "manifest":serde_json::from_slice::<serde_json::Value>(&fs::read(directory.join("manifest.json"))?)?});
    fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(
        directory.join(format!("report-{label}.json")),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
