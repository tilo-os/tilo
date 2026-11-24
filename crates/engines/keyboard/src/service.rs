//! What the keyboard's app asks this device, as one JSON answer to one JSON ask.
//!
//! Every ask carries what a person's typing taught the keyboard, so the answers follow them
//! without this crate keeping a copy: a replay asks the same thing and hears the same words.
//! The asks are the keyboard's whole interface from a package (`engine.call`), so the app draws
//! and decides, and the device computes.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::Deserialize;
use serde_json::json;
use vello::kurbo::{Point, Rect};

use crate::autocorrect::{Autocorrector, KeyAdjacency};
use crate::dict::{Dictionary, Language};
use crate::layout::{InputPurpose, KeyAction, KeyboardLayout, ShiftState, key_rects};
use crate::predict::Predictor;
use crate::swipe::{GeometricSwipeModel, SwipeModel};
use crate::touch_model::{KeyOffset, NextKeyDistribution, TouchKey, TouchModel};

/// The most words one answer offers.
const MOST_WORDS: usize = 8;

/// One ask answered: `op` names it, `input` is its JSON, and the answer is JSON.
///
/// # Errors
///
/// The ask does not read, the op is none of this keyboard's, or the work cannot be done.
pub fn call(op: &str, input: &str) -> Result<String, String> {
    match op {
        "layouts" => Ok(layouts().to_string()),
        "suggest" => ask::<Suggest>(input).and_then(suggest),
        "correct" => ask::<Correct>(input).and_then(correct),
        "learn" => ask::<Learn>(input).and_then(learn),
        "accents" => ask::<Accents>(input).and_then(accents),
        "emoji" => ask::<Emoji>(input).and_then(emoji),
        "swipe" => ask::<Swipe>(input).and_then(swipe),
        "touch" => ask::<Touch>(input).and_then(touch),
        other => Err(format!("`{other}` is no ask the on-screen keyboard makes")),
    }
}

fn ask<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

fn language(code: &str) -> Language {
    Language::from_code(code).unwrap_or_default()
}

fn shift(name: &str) -> ShiftState {
    match name {
        "shift" => ShiftState::Shift,
        "caps" | "caps-lock" => ShiftState::CapsLock,
        _ => ShiftState::Off,
    }
}

/// The dictionaries, parsed once each: reading one is the whole word table.
fn dictionary(language: Language) -> &'static Dictionary {
    static DICTS: OnceLock<Mutex<HashMap<Language, &'static Dictionary>>> = OnceLock::new();
    let dicts = DICTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut held = held(dicts);
    held.entry(language)
        .or_insert_with(|| Box::leak(Box::new(Dictionary::for_language(language))))
}

fn predictor(language: Language) -> &'static Mutex<Predictor> {
    static PREDICTORS: OnceLock<Mutex<HashMap<Language, &'static Mutex<Predictor>>>> =
        OnceLock::new();
    let predictors = PREDICTORS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut held = held(predictors);
    held.entry(language).or_insert_with(|| {
        Box::leak(Box::new(Mutex::new(Predictor::for_language(language))))
    })
}

fn corrector(language: Language) -> &'static Mutex<Autocorrector> {
    static CORRECTORS: OnceLock<Mutex<HashMap<Language, &'static Mutex<Autocorrector>>>> =
        OnceLock::new();
    let correctors = CORRECTORS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut held = held(correctors);
    held.entry(language).or_insert_with(|| {
        let adjacency = KeyAdjacency::from_layout(&KeyboardLayout::qwerty());
        Box::leak(Box::new(Mutex::new(Autocorrector::new(
            dictionary(language),
            adjacency,
        ))))
    })
}

/// The swipe decoders, one per language, kept by the thread that asks: the FUTO model when its
/// runtime is on the device (English only, since that is the model it carries), else the
/// geometric one. Both answer the same candidates.
fn with_decoder<T>(language: Language, ask: impl FnOnce(&dyn SwipeModel) -> T) -> T {
    thread_local! {
        static DECODERS: std::cell::RefCell<HashMap<Language, Box<dyn SwipeModel>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    DECODERS.with(|decoders| {
        let mut held = decoders.borrow_mut();
        let model = held.entry(language).or_insert_with(|| {
            match crate::futo::FutoSwipeModel::default_load() {
                Some(model) if language == Language::English => Box::new(model),
                _ => Box::new(GeometricSwipeModel::for_language(language)),
            }
        });
        ask(&**model)
    })
}

/// A lock poisoned by a panic in one ask still serves the next: what it guards is a cache of
/// answers, and no half-written one is read.
fn held<T>(lock: &'static Mutex<T>) -> MutexGuard<'static, T> {
    lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------- layouts

/// The layouts the keyboard types on, as its app draws them: every row's keys with the label,
/// the action and the width the key takes, plus where each purpose starts and what the app
/// offers as settings.
fn layouts() -> serde_json::Value {
    let names = [
        "qwerty", "azerty", "symbols", "numeric", "decimal", "phone", "pin", "email", "url",
    ];
    let built: Vec<serde_json::Value> = names
        .iter()
        .filter_map(|name| KeyboardLayout::by_name(name))
        .map(|layout| {
            let rows: Vec<Vec<serde_json::Value>> = layout
                .rows
                .iter()
                .map(|row| row.iter().map(key_json).collect())
                .collect();
            json!({ "name": layout.name, "rows": rows })
        })
        .collect();
    let purposes: serde_json::Map<String, serde_json::Value> = [
        ("text", InputPurpose::Text),
        ("terminal", InputPurpose::Terminal),
        ("number", InputPurpose::Number),
        ("decimal", InputPurpose::Decimal),
        ("telephone", InputPurpose::Telephone),
        ("url", InputPurpose::Url),
        ("email", InputPurpose::Email),
        ("pin", InputPurpose::Pin),
        ("password", InputPurpose::Password),
        ("date", InputPurpose::Date),
        ("time", InputPurpose::Time),
    ]
    .into_iter()
    .map(|(name, purpose)| {
        (
            name.to_owned(),
            json!(KeyboardLayout::for_purpose(purpose)),
        )
    })
    .collect();
    json!({
        "layouts": built,
        "order": ["qwerty", "azerty"],
        "names": { "qwerty": "English (QWERTY)", "azerty": "French (AZERTY)" },
        "purposes": purposes,
        "sizes": ["compact", "standard", "large"],
        "typing_languages": ["layout", "en", "fr"],
        "languages": { "en": "English", "fr": "French" },
    })
}

fn key_json(key: &crate::layout::Key) -> serde_json::Value {
    let action = match &key.action {
        KeyAction::Char(text) => json!({ "a": "char", "t": text }),
        KeyAction::Backspace => json!({ "a": "backspace" }),
        KeyAction::Enter => json!({ "a": "enter" }),
        KeyAction::Space => json!({ "a": "space" }),
        KeyAction::Shift => json!({ "a": "shift" }),
        KeyAction::SymbolsToggle => json!({ "a": "symbols" }),
        KeyAction::Layout(name) => json!({ "a": "layout", "t": name }),
        KeyAction::Emoji => json!({ "a": "emoji" }),
    };
    json!({
        "label": key.label,
        "action": action,
        "width": key.width,
        "icon": key.icon.unwrap_or_default(),
    })
}

// ---------------------------------------------------------------- prediction

#[derive(Deserialize)]
#[serde(default)]
struct Suggest {
    word: String,
    previous: String,
    language: String,
    /// The words the person added to their own dictionary, most useful first.
    personal: Vec<String>,
    personalization: bool,
    /// What typing taught: each context, then each word it led to and how often.
    learned: BTreeMap<String, BTreeMap<String, u32>>,
    /// Words the person kept as they typed them: never corrected again.
    kept: Vec<String>,
    limit: usize,
}

impl Default for Suggest {
    fn default() -> Self {
        Self {
            word: String::new(),
            previous: String::new(),
            language: String::new(),
            personal: Vec::new(),
            personalization: true,
            learned: BTreeMap::new(),
            kept: Vec::new(),
            limit: 5,
        }
    }
}

fn suggest(ask: Suggest) -> Result<String, String> {
    let language = language(&ask.language);
    let dict = dictionary(language);
    let limit = ask.limit.clamp(1, MOST_WORDS);
    let previous = (!ask.previous.is_empty()).then_some(ask.previous.as_str());
    let mut out = Vec::new();
    {
        let mut predictor = held(predictor(language));
        predictor.adopt_learned(ask.learned.clone());
        if !ask.word.is_empty() {
            for (word, score) in predictor.predict_next_n(previous, &ask.word, limit) {
                out.push(json!({ "word": word, "score": score }));
            }
        } else {
            for (word, score) in predictor.predict_next_n(previous, "", limit) {
                out.push(json!({ "word": word, "score": score }));
            }
        }
    }
    // The person's own words come first, and close what the dictionary offers of them.
    let typed = ask.word.to_lowercase();
    let mut personal: Vec<String> = ask
        .personal
        .iter()
        .filter(|word| typed.is_empty() || word.to_lowercase().starts_with(&typed))
        .take(limit)
        .cloned()
        .collect();
    personal.dedup();
    for word in personal.into_iter().rev() {
        if !out.iter().any(|chip| chip["word"] == word) {
            out.insert(0, json!({ "word": word, "score": 1.0, "personal": true }));
        }
    }
    let mut next = Vec::new();
    if ask.word.is_empty() {
        let predictor = held(predictor(language));
        for (word, score) in predictor.predict_next_n(previous, "", limit) {
            next.push(json!({ "word": word, "score": score }));
        }
    }
    // An emoji the composing word names comes at the end of the strip.
    let mut emojis = Vec::new();
    if !typed.is_empty() {
        let catalogue = emoji::Catalogue::english();
        emojis = catalogue
            .suggest(&typed, 2)
            .into_iter()
            .map(|found| found.text().to_owned())
            .collect();
    }
    out.truncate(limit.saturating_sub(emojis.len()));
    for text in emojis {
        out.push(json!({ "word": text, "score": 1.0, "emoji": true }));
    }
    let _ = dict;
    Ok(json!({ "completions": out, "next": next }).to_string())
}

#[derive(Deserialize)]
#[serde(default)]
struct Learn {
    word: String,
    previous: String,
}

impl Default for Learn {
    fn default() -> Self {
        Self {
            word: String::new(),
            previous: String::new(),
        }
    }
}

/// What committing `word` after `previous` teaches the predictor: the transition, or none for a
/// word that is not letters (a number, a smile).
fn learn(ask: Learn) -> Result<String, String> {
    let previous = (!ask.previous.is_empty()).then_some(ask.previous.as_str());
    Ok(match Predictor::transition(previous, &ask.word) {
        Some((context, word)) => json!({ "transition": [context, word] }).to_string(),
        None => json!({ "transition": null }).to_string(),
    })
}

// ---------------------------------------------------------------- autocorrection

#[derive(Deserialize)]
#[serde(default)]
struct Correct {
    word: String,
    language: String,
    layout: String,
    /// Words the person kept as they typed them: never corrected again.
    kept: Vec<String>,
}

impl Default for Correct {
    fn default() -> Self {
        Self {
            word: String::new(),
            language: String::new(),
            layout: "qwerty".to_owned(),
            kept: Vec::new(),
        }
    }
}

fn correct(ask: Correct) -> Result<String, String> {
    let language = language(&ask.language);
    let layout =
        KeyboardLayout::by_name(&ask.layout).unwrap_or_else(KeyboardLayout::qwerty);
    let mut corrector = held(corrector(language));
    corrector.set_adjacency(KeyAdjacency::from_layout(&layout));
    corrector.adopt_kept(&ask.kept);
    Ok(json!({ "correction": corrector.correct(&ask.word) }).to_string())
}

// ---------------------------------------------------------------- accents

#[derive(Deserialize)]
#[serde(default)]
struct Accents {
    key: String,
    language: String,
    shift: String,
}

impl Default for Accents {
    fn default() -> Self {
        Self {
            key: String::new(),
            language: String::new(),
            shift: String::new(),
        }
    }
}

/// What a long press offers on `key`: the letter first, then its accented forms cased for the
/// shift, the ones the typing language uses first.
fn accents(ask: Accents) -> Result<String, String> {
    let language = language(&ask.language);
    let usage = dictionary(language).accent_usage();
    let key = crate::layout::Key::ch(&ask.key);
    let forms = key.alternates(shift(&ask.shift), |c| {
        usage.get(&c).copied().unwrap_or_default()
    });
    Ok(json!({ "forms": forms }).to_string())
}

// ---------------------------------------------------------------- emoji

#[derive(Deserialize)]
#[serde(default)]
struct Emoji {
    word: String,
    limit: usize,
}

impl Default for Emoji {
    fn default() -> Self {
        Self {
            word: String::new(),
            limit: 2,
        }
    }
}

fn emoji(ask: Emoji) -> Result<String, String> {
    let catalogue = emoji::Catalogue::english();
    let found: Vec<&str> = catalogue
        .suggest(&ask.word.to_lowercase(), ask.limit.clamp(1, MOST_WORDS))
        .into_iter()
        .map(emoji::Emoji::text)
        .collect();
    Ok(json!({ "emoji": found }).to_string())
}

// ---------------------------------------------------------------- swipe

#[derive(Deserialize)]
#[serde(default)]
struct Swipe {
    /// The finger's path as flat `x, y` pairs in logical pixels of the surface.
    path: Vec<f64>,
    /// The key band the layout fills, `[x0, y0, x1, y1]`.
    area: [f64; 4],
    layout: String,
    language: String,
    shift: String,
    limit: usize,
}

impl Default for Swipe {
    fn default() -> Self {
        Self {
            path: Vec::new(),
            area: [0.0, 0.0, 0.0, 0.0],
            layout: "qwerty".to_owned(),
            language: String::new(),
            shift: String::new(),
            limit: 5,
        }
    }
}

fn swipe(ask: Swipe) -> Result<String, String> {
    let layout =
        KeyboardLayout::by_name(&ask.layout).unwrap_or_else(KeyboardLayout::qwerty);
    let area = Rect::new(ask.area[0], ask.area[1], ask.area[2], ask.area[3]);
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return Ok(json!({ "swipe": false, "candidates": [] }).to_string());
    }
    let geom = key_rects(&layout, area, ShiftState::Off);
    let path: Vec<Point> = ask
        .path
        .chunks_exact(2)
        .map(|pair| Point::new(pair[0], pair[1]))
        .collect();
    if path.len() < 2 || !crate::swipe::is_swipe(&path, key_scale(&geom, &layout)) {
        return Ok(json!({ "swipe": false, "candidates": [] }).to_string());
    }
    let language = language(&ask.language);
    let shift = shift(&ask.shift);
    let candidates: Vec<serde_json::Value> = with_decoder(language, |decoder| {
        decoder.decode(&path, &geom, &layout, ask.limit.clamp(1, MOST_WORDS))
    })
        .into_iter()
        .map(|candidate| {
            json!({
                "word": case_word(&candidate.word, shift),
                "score": candidate.score,
                "raw": candidate.word,
            })
        })
        .collect();
    Ok(json!({ "swipe": true, "candidates": candidates }).to_string())
}

fn case_word(word: &str, shift: ShiftState) -> String {
    shift.case_word(word)
}

/// A key's size, in logical pixels, as the width the keyboard lays out: the first letter key's
/// width, so a path is measured against the keys it crossed.
fn key_scale(geom: &[(crate::layout::KeyId, Rect)], layout: &KeyboardLayout) -> f64 {
    crate::swipe::letter_key_scale(geom, layout).unwrap_or(1.0).max(1.0)
}

// ---------------------------------------------------------------- taps

#[derive(Deserialize)]
#[serde(default)]
struct Touch {
    /// Where the finger landed, in logical pixels of the surface.
    point: (f64, f64),
    /// The key band the layout fills, `[x0, y0, x1, y1]`.
    area: (f64, f64, f64, f64),
    layout: String,
    /// The word before this one, and what of this one is typed: they say which letter comes next.
    previous: String,
    word: String,
    language: String,
    /// Where this person tends to hit each letter, as the keyboard learned.
    offsets: Vec<(char, f32, f32, u32)>,
    personalization: bool,
}

impl Default for Touch {
    fn default() -> Self {
        Self {
            point: (0.0, 0.0),
            area: (0.0, 0.0, 0.0, 0.0),
            layout: "qwerty".to_owned(),
            previous: String::new(),
            word: String::new(),
            language: String::new(),
            offsets: Vec::new(),
            personalization: true,
        }
    }
}

/// The key a finger meant, and what its landing taught this person's tap positions.
fn touch(ask: Touch) -> Result<String, String> {
    let layout =
        KeyboardLayout::by_name(&ask.layout).unwrap_or_else(KeyboardLayout::qwerty);
    let area = Rect::new(ask.area.0, ask.area.1, ask.area.2, ask.area.3);
    let geom = key_rects(&layout, area, ShiftState::Off);
    let keys: Vec<TouchKey> = geom
        .iter()
        .filter_map(|&(id, rect)| {
            let key = layout.key(id).filter(|key| key.is_letter())?;
            let ch = key.label.chars().next()?.to_lowercase().next()?;
            Some(TouchKey { id, ch, rect })
        })
        .collect();
    let mut model = TouchModel::new();
    let offsets: BTreeMap<char, KeyOffset> = ask
        .offsets
        .iter()
        .map(|(ch, dx, dy, samples)| {
            (
                *ch,
                KeyOffset {
                    dx: *dx,
                    dy: *dy,
                    samples: *samples,
                },
            )
        })
        .collect();
    model.adopt(&offsets);
    let language = language(&ask.language);
    let previous = (!ask.previous.is_empty()).then_some(ask.previous.as_str());
    let next = held(predictor(language)).next_key_distribution(previous, &ask.word);
    let point = Point::new(ask.point.0, ask.point.1);
    let Some(target) = crate::touch_model::resolve(point, &keys, &next, &model) else {
        return Ok(json!({ "key": null }).to_string());
    };
    let mut learnt = None;
    if ask.personalization && let Some(key) = keys.iter().find(|key| key.id == target) {
        model.learn(key.ch, point, key.rect);
        if let Some(offset) = model.offset(key.ch) {
            learnt = Some(json!({
                "ch": key.ch.to_string(),
                "dx": offset.dx,
                "dy": offset.dy,
                "samples": offset.samples,
            }));
        }
    }
    let _ = NextKeyDistribution::uniform();
    Ok(json!({ "key": [target.row, target.col], "taught": learnt }).to_string())
}
