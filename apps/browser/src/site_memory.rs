//! Site memory: what reading a site's forms found, kept in the Browser's
//! document so it is shared and synced like its bookmarks.
//!
//! A form is remembered by the site that asks it and its signature (what it
//! asks and where it sends it): its name, its questions with their kinds and
//! the answers they offer, which questions the typed path refuses and why,
//! the pages it was read on, and which form's sending led to it. A page of a
//! remembered site publishes its forms' actions from here before anyone
//! reads it ([`at_place`]).
//!
//! Only a form's shape is here. What a field holds is never read into it:
//! not what a person typed, not a secret, not its length. A secret question
//! is remembered by its name and the reason the typed path refuses it, since
//! that is what sends a caller to the secret fill; the answers a question
//! offers are left out when the page that asks holds a sign-in, where they
//! may be the person's own (their saved addresses).
//!
//! Memory is written by [`remember`], the body of the document's
//! `remember-forms` action, when a page's forms are read or answered. The
//! window reads its page's forms itself once the page has loaded, so a visit
//! to a page that asks a form remembers that form's shape. A private window
//! and one whose history is paused remember nothing.
//!
//! What is remembered is the person's to see and to forget: [`sites`] lists
//! it, and [`forget_form`] and [`forget_site`] are the bodies of the
//! document's `forget-form` and `forget-site-forms` actions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema, Model, Refusal};

/// The most forms the document remembers; the ones read longest ago go first.
pub const MAX_REMEMBERED_FORMS: usize = 300;
/// The most pages one form is remembered on; the oldest goes first.
pub const MAX_FORM_PAGES: usize = 8;
/// The most forms one read of a page remembers, as many as the engine reads.
pub const MAX_PAGE_FORMS: usize = 24;
/// The most questions of one form, as many as the engine reads.
pub const MAX_QUESTIONS: usize = 150;
/// The most answers one question offers, as many as the engine reads.
pub const MAX_CHOICES: usize = 80;
/// The most buttons that send one form.
pub const MAX_SUBMITS: usize = 16;
/// The longest name, label or offered answer, in characters.
pub const MAX_TEXT: usize = 200;
/// The longest path of a page.
pub const MAX_PAGE_BYTES: usize = 2048;
/// The longest signature, as the engine bounds the name of a form.
pub const MAX_SIGNATURE: usize = 64;
/// How stale "last read" may be before a read of an unchanged form writes
/// it again: reading the same form every minute changes nothing.
pub const SEEN_STEP: u64 = 24 * 60 * 60;

/// The kinds of question the engine reads.
pub const KINDS: [&str; 17] = [
    "text",
    "email",
    "tel",
    "url",
    "number",
    "date",
    "time",
    "datetime-local",
    "month",
    "week",
    "range",
    "color",
    "choice",
    "choices",
    "flag",
    "secret",
    "file",
];

/// The refusals of the typed path that belong to a field rather than to an
/// answer: a secret, a file upload, an editor written by typing.
pub const FALLBACKS: [&str; 3] = ["secret_field", "needs_files", "needs_typing"];

/// One question of a remembered form.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RememberedQuestion {
    /// The name the form asks it under.
    pub name: String,
    /// Its kind, as the engine reads it: text, date, choice, flag and the rest.
    pub kind: String,
    /// The answers it offers, when it offers them and they were kept.
    pub choices: Vec<String>,
    /// Whether the form requires an answer.
    pub required: bool,
    /// Why the typed path refuses it, when it does: the refusal's code.
    pub fallback: String,
}

/// One form of a site, as reading it found it.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RememberedForm {
    /// Its signature: what it asks and where it sends it.
    pub signature: String,
    /// Its own name, or the heading it stands under.
    pub label: String,
    /// The paths of the site's pages it was read on.
    pub pages: Vec<String>,
    /// What it asks.
    pub questions: Vec<RememberedQuestion>,
    /// The labels of the buttons that send it.
    pub submit: Vec<String>,
    /// The signature of the form whose sending led to the page this one was
    /// read on, in a flow of several pages.
    pub follows: String,
    /// The signature this form had before an answer made it ask more.
    pub grew_from: String,
    /// When it was last read, in Unix seconds.
    pub seen: u64,
}

/// Where a form is asked: a site, by its exact origin, and a page of it, by
/// its path. A query and a fragment are no part of it, so nothing a visit
/// carried in its address is remembered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub origin: String,
    pub page: String,
}

impl Place {
    /// The place the address `url` is, when it is a web page.
    pub fn of(url: &str) -> Option<Self> {
        let url = url.trim();
        let origin = crate::site_controls::origin(url)?;
        let page = url::Url::parse(url).ok()?.path().to_owned();
        (page.len() <= MAX_PAGE_BYTES).then_some(Self { origin, page })
    }
}

/// The key the form `signature` of the site `origin` is remembered under.
/// Neither an origin nor a signature holds a space.
pub fn key(origin: &str, signature: &str) -> String {
    format!("{origin} {signature}")
}

/// Whether `signature` is one a form can have: what the engine prints, a
/// short run of letters and digits.
pub fn is_signature(signature: &str) -> bool {
    (1..=MAX_SIGNATURE).contains(&signature.len())
        && signature.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn text_fits(text: &str) -> bool {
    text.chars().count() <= MAX_TEXT && !text.chars().any(char::is_control)
}

fn question_valid(question: &RememberedQuestion) -> bool {
    !question.name.is_empty()
        && text_fits(&question.name)
        && KINDS.contains(&question.kind.as_str())
        && question.choices.len() <= MAX_CHOICES
        && question.choices.iter().all(|choice| text_fits(choice))
        && (question.fallback.is_empty() || FALLBACKS.contains(&question.fallback.as_str()))
}

fn form_valid(form: &RememberedForm) -> bool {
    let link = |signature: &str| signature.is_empty() || is_signature(signature);
    is_signature(&form.signature)
        && text_fits(&form.label)
        && form.questions.len() <= MAX_QUESTIONS
        && form.questions.iter().all(question_valid)
        && form.submit.len() <= MAX_SUBMITS
        && form.submit.iter().all(|label| text_fits(label))
        && link(&form.follows)
        && link(&form.grew_from)
}

/// The remembered form `signature` of the site `origin`.
pub fn get<'a>(
    memory: &'a BTreeMap<String, RememberedForm>,
    origin: &str,
    signature: &str,
) -> Option<&'a RememberedForm> {
    memory.get(&key(origin, signature))
}

/// The forms of the site `origin`, by signature.
pub fn of_site<'a>(
    memory: &'a BTreeMap<String, RememberedForm>,
    origin: &str,
) -> impl Iterator<Item = (&'a String, &'a RememberedForm)> {
    let prefix = format!("{origin} ");
    memory
        .range(prefix.clone()..)
        .take_while(move |(key, _)| key.starts_with(&prefix))
}

/// The forms `place` is remembered to ask when it opens: the ones read on
/// that page, without the shapes a form only takes once an answer made it
/// ask more.
pub fn at_place<'a>(
    memory: &'a BTreeMap<String, RememberedForm>,
    place: &Place,
) -> Vec<&'a RememberedForm> {
    of_site(memory, &place.origin)
        .map(|(_, form)| form)
        .filter(|form| form.grew_from.is_empty() && form.pages.contains(&place.page))
        .collect()
}

/// The forms the site `origin` asked after `signature` was sent.
pub fn followers<'a>(
    memory: &'a BTreeMap<String, RememberedForm>,
    origin: &str,
    signature: &str,
) -> Vec<&'a RememberedForm> {
    of_site(memory, origin)
        .map(|(_, form)| form)
        .filter(|form| form.follows == signature)
        .collect()
}

/// The site a key of memory is of.
fn site_of(key: &str) -> &str {
    key.split_once(' ').map_or(key, |(origin, _)| origin)
}

/// Every site forms are remembered of, each with its forms, by origin and
/// then by signature.
pub fn sites(memory: &BTreeMap<String, RememberedForm>) -> Vec<(&str, Vec<&RememberedForm>)> {
    let mut sites: Vec<(&str, Vec<&RememberedForm>)> = Vec::new();
    for (key, form) in memory {
        let origin = site_of(key);
        match sites.last_mut() {
            Some((last, forms)) if *last == origin => forms.push(form),
            _ => sites.push((origin, vec![form])),
        }
    }
    sites
}

/// The forms of the site `origin` as a person counts them: one for each
/// form, whatever shapes it is remembered in. A shape a form only takes once
/// an answer made it ask more stands for no form of its own while the shape
/// it grew from is remembered.
pub fn forms_of<'a>(
    memory: &'a BTreeMap<String, RememberedForm>,
    origin: &str,
) -> Vec<&'a RememberedForm> {
    of_site(memory, origin)
        .map(|(_, form)| form)
        .filter(|form| form.grew_from.is_empty() || get(memory, origin, &form.grew_from).is_none())
        .collect()
}

/// Forget the form `signature` of the site `origin` in every shape it is
/// remembered in: the one its page opens with and the ones it grew into.
/// A form that followed it follows nothing any more. Answers how many
/// shapes went: none when no such form is remembered.
pub fn forget_form(
    memory: &mut BTreeMap<String, RememberedForm>,
    origin: &str,
    signature: &str,
) -> usize {
    if get(memory, origin, signature).is_none() {
        return 0;
    }
    // The shapes of one form name each other by what each grew from.
    let shapes: Vec<(String, String)> = of_site(memory, origin)
        .map(|(_, form)| (form.signature.clone(), form.grew_from.clone()))
        .collect();
    let mut gone = vec![signature.to_owned()];
    loop {
        let before = gone.len();
        for (shape, grew_from) in &shapes {
            let known = gone.contains(shape);
            if !known && gone.contains(grew_from) {
                gone.push(shape.clone());
            } else if known && !grew_from.is_empty() && !gone.contains(grew_from) {
                gone.push(grew_from.clone());
            }
        }
        if gone.len() == before {
            break;
        }
    }
    let mut forgotten = 0;
    for shape in &gone {
        forgotten += usize::from(memory.remove(&key(origin, shape)).is_some());
    }
    let followed: Vec<String> = of_site(memory, origin)
        .filter(|(_, form)| gone.contains(&form.follows))
        .map(|(key, _)| key.clone())
        .collect();
    for key in followed {
        if let Some(form) = memory.get_mut(&key) {
            form.follows.clear();
        }
    }
    forgotten
}

/// Forget every form of the site `origin`. Answers how many went.
pub fn forget_site(memory: &mut BTreeMap<String, RememberedForm>, origin: &str) -> usize {
    let keys: Vec<String> = of_site(memory, origin)
        .map(|(key, _)| key.clone())
        .collect();
    for key in &keys {
        memory.remove(key);
    }
    keys.len()
}

/// `incoming` as it is kept beside what was remembered of the same form:
/// read on `page` too, keeping what answering taught and the answers a
/// read that left them out did not see.
fn merged(
    kept: Option<&RememberedForm>,
    mut incoming: RememberedForm,
    page: &str,
    at: u64,
) -> RememberedForm {
    let mut pages = kept.map(|kept| kept.pages.clone()).unwrap_or_default();
    if !pages.iter().any(|known| known == page) {
        pages.push(page.to_owned());
    }
    while pages.len() > MAX_FORM_PAGES {
        pages.remove(0);
    }
    incoming.pages = pages;
    let Some(kept) = kept else {
        incoming.seen = at;
        return incoming;
    };
    for question in &mut incoming.questions {
        let Some(known) = kept
            .questions
            .iter()
            .find(|known| known.name == question.name && known.kind == question.kind)
        else {
            continue;
        };
        if question.fallback.is_empty() {
            question.fallback.clone_from(&known.fallback);
        }
        if question.choices.is_empty() {
            question.choices.clone_from(&known.choices);
        }
    }
    if incoming.follows.is_empty() {
        incoming.follows.clone_from(&kept.follows);
    }
    if incoming.grew_from.is_empty() {
        incoming.grew_from.clone_from(&kept.grew_from);
    }
    incoming.seen = if at.saturating_sub(kept.seen) >= SEEN_STEP {
        at
    } else {
        kept.seen
    };
    incoming
}

/// Take `page` away from the forms remembered on it that it no longer asks.
/// A form the page asks in another state of the same form (before or after
/// an answer made it ask more) is still its own.
fn forget_what_left(
    memory: &mut BTreeMap<String, RememberedForm>,
    origin: &str,
    page: &str,
    present: &[String],
) {
    let on_page: Vec<(String, String, String)> = of_site(memory, origin)
        .filter(|(_, form)| form.pages.iter().any(|known| known == page))
        .map(|(key, form)| (key.clone(), form.signature.clone(), form.grew_from.clone()))
        .collect();
    let mut stays: Vec<&str> = present.iter().map(String::as_str).collect();
    loop {
        let before = stays.len();
        for (_, signature, grew_from) in &on_page {
            if stays.contains(&signature.as_str()) {
                continue;
            }
            let grew_from_one = !grew_from.is_empty() && stays.contains(&grew_from.as_str());
            let one_grew_from_it = on_page
                .iter()
                .any(|(_, other, from)| from == signature && stays.contains(&other.as_str()));
            if grew_from_one || one_grew_from_it {
                stays.push(signature);
            }
        }
        if stays.len() == before {
            break;
        }
    }
    for (key, signature, _) in &on_page {
        if stays.contains(&signature.as_str()) {
            continue;
        }
        let emptied = memory.get_mut(key).is_some_and(|form| {
            form.pages.retain(|known| known != page);
            form.pages.is_empty()
        });
        if emptied {
            memory.remove(key);
        }
    }
}

/// Remember `forms` as forms the page `page` of the site `origin` asks,
/// read at `at` (Unix seconds). With `whole` they are every form the page
/// asks now: one remembered for the page and not among them has changed or
/// gone, and leaves it.
///
/// # Errors
/// A site that is not one exact http(s) origin, a page that is not a path,
/// or a form outside what reading a page finds.
pub fn remember(
    memory: &mut BTreeMap<String, RememberedForm>,
    origin: &str,
    page: &str,
    forms: Vec<RememberedForm>,
    whole: bool,
    at: u64,
) -> Result<(), Refusal> {
    if crate::site_controls::origin(origin).as_deref() != Some(origin) {
        return Err(Refusal::new("a site is named by one exact http(s) origin"));
    }
    if !page.starts_with('/') || page.len() > MAX_PAGE_BYTES || page.chars().any(char::is_control) {
        return Err(Refusal::new("a page is named by its path on the site"));
    }
    if forms.len() > MAX_PAGE_FORMS || !forms.iter().all(form_valid) {
        return Err(Refusal::new(
            "a remembered form is a form's shape as reading a page finds it",
        ));
    }
    let mut present = Vec::with_capacity(forms.len());
    for form in forms {
        if present.contains(&form.signature) {
            // Two forms of one page that ask the same are one shape.
            continue;
        }
        present.push(form.signature.clone());
        let key = key(origin, &form.signature);
        let next = merged(memory.get(&key), form, page, at);
        if memory.get(&key) != Some(&next) {
            memory.insert(key, next);
        }
    }
    if whole {
        forget_what_left(memory, origin, page, &present);
    }
    while memory.len() > MAX_REMEMBERED_FORMS {
        let Some(oldest) = memory
            .iter()
            .min_by(|a, b| a.1.seen.cmp(&b.1.seen).then_with(|| a.0.cmp(b.0)))
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        memory.remove(&oldest);
    }
    Ok(())
}

/// The site a screenshot shows remembered forms of, and the page they were
/// read on.
pub const SHOT_SITE: &str = "https://visas.example.org";
pub const SHOT_PAGE: &str = "/apply";

/// Screenshot fixture: the forms of a visa application site, as reading its
/// page found them.
pub fn shot_forms() -> Vec<RememberedForm> {
    let question = |name: &str, kind: &str, required: bool| RememberedQuestion {
        name: name.to_owned(),
        kind: kind.to_owned(),
        required,
        ..RememberedQuestion::default()
    };
    vec![
        RememberedForm {
            signature: "5d0c7a1e".into(),
            label: "Apply for a visitor visa".into(),
            questions: vec![
                question("Full name", "text", true),
                question("Date of birth", "date", true),
                question("Nationality", "choice", true),
                question("Arrival", "date", true),
                question("Passport number", "text", true),
                question("I agree", "flag", true),
            ],
            submit: vec!["Send application".into()],
            ..RememberedForm::default()
        },
        RememberedForm {
            signature: "0a11ce00".into(),
            label: "Search this site".into(),
            questions: vec![question("Words", "text", false)],
            submit: vec!["Search".into()],
            ..RememberedForm::default()
        },
        RememberedForm {
            signature: "7e57ab1e".into(),
            label: "Sign up for updates".into(),
            questions: vec![
                question("Email", "email", true),
                question("Weekly summary", "flag", false),
            ],
            submit: vec!["Sign up".into()],
            ..RememberedForm::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: &str = "https://visa.test";

    fn question(name: &str, kind: &str) -> RememberedQuestion {
        RememberedQuestion {
            name: name.into(),
            kind: kind.into(),
            ..RememberedQuestion::default()
        }
    }

    fn form(signature: &str, label: &str, questions: Vec<RememberedQuestion>) -> RememberedForm {
        RememberedForm {
            signature: signature.into(),
            label: label.into(),
            questions,
            submit: vec!["Send".into()],
            ..RememberedForm::default()
        }
    }

    fn signatures(memory: &BTreeMap<String, RememberedForm>, page: &str) -> Vec<String> {
        at_place(
            memory,
            &Place {
                origin: SITE.into(),
                page: page.into(),
            },
        )
        .into_iter()
        .map(|form| form.signature.clone())
        .collect()
    }

    /// A place is a site and a path: the query and the fragment of the
    /// address a visit used are no part of it, and only a web page has one.
    #[test]
    fn a_place_is_an_origin_and_a_path() {
        assert_eq!(
            Place::of("https://Visa.test/apply?token=secret#step"),
            Some(Place {
                origin: SITE.into(),
                page: "/apply".into()
            })
        );
        assert_eq!(Place::of("about:blank"), None);
        assert_eq!(Place::of("file:///etc/passwd"), None);
    }

    /// A page's forms are remembered by site and signature and found again
    /// for that page; a read that finds another form in place of one
    /// replaces it, and a form read on two pages stays on the other.
    #[test]
    fn a_changed_form_replaces_the_one_remembered_for_its_page() {
        let mut memory = BTreeMap::new();
        let old = form("aaaa1111", "Apply", vec![question("Name", "text")]);
        let search = form("cccc3333", "Search", vec![question("Words", "text")]);
        remember(
            &mut memory,
            SITE,
            "/apply",
            vec![old.clone(), search.clone()],
            true,
            10,
        )
        .unwrap();
        remember(&mut memory, SITE, "/help", vec![search], true, 10).unwrap();
        assert_eq!(signatures(&memory, "/apply"), ["aaaa1111", "cccc3333"]);

        let new = form(
            "bbbb2222",
            "Apply",
            vec![question("Name", "text"), question("Arrival", "date")],
        );
        remember(&mut memory, SITE, "/apply", vec![new], true, 20).unwrap();
        assert_eq!(signatures(&memory, "/apply"), ["bbbb2222"]);
        assert_eq!(
            signatures(&memory, "/help"),
            ["cccc3333"],
            "the form another page still asks is remembered there"
        );
        assert!(
            get(&memory, SITE, "aaaa1111").is_none(),
            "a form no page asks any more is forgotten"
        );
        // One form answered is not every form of its page.
        remember(&mut memory, SITE, "/help", vec![old], false, 30).unwrap();
        assert_eq!(signatures(&memory, "/help"), ["aaaa1111", "cccc3333"]);
    }

    /// What answering taught about a question stays when the form is read
    /// again, and a read that left the offered answers out does not erase
    /// the ones a read that kept them saw.
    #[test]
    fn a_later_read_keeps_what_answering_taught() {
        let mut memory = BTreeMap::new();
        let mut country = question("Country", "choice");
        country.choices = vec!["Kenya".into(), "Other".into()];
        let mut notes = question("Notes", "text");
        notes.fallback = "needs_typing".into();
        remember(
            &mut memory,
            SITE,
            "/apply",
            vec![form("aaaa1111", "Apply", vec![country, notes])],
            true,
            10,
        )
        .unwrap();
        let bare = form(
            "aaaa1111",
            "Apply for a visa",
            vec![question("Country", "choice"), question("Notes", "text")],
        );
        remember(&mut memory, SITE, "/apply", vec![bare], true, 20).unwrap();
        let kept = get(&memory, SITE, "aaaa1111").unwrap();
        assert_eq!(kept.label, "Apply for a visa");
        assert_eq!(kept.questions[0].choices, ["Kenya", "Other"]);
        assert_eq!(kept.questions[1].fallback, "needs_typing");
        assert_eq!(
            kept.seen, 10,
            "an unchanged form is not written for its date"
        );
        let again = kept.clone();
        remember(
            &mut memory,
            SITE,
            "/apply",
            vec![again],
            true,
            10 + SEEN_STEP,
        )
        .unwrap();
        assert_eq!(get(&memory, SITE, "aaaa1111").unwrap().seen, 10 + SEEN_STEP);
    }

    /// A form that asks more once it is answered is remembered in both
    /// shapes, and the page opens with the first: reading it in either
    /// shape keeps the other.
    #[test]
    fn a_form_that_grew_is_remembered_with_the_shape_the_page_opens_with() {
        let mut memory = BTreeMap::new();
        let first = form("aaaa1111", "Apply", vec![question("Nationality", "choice")]);
        let mut grown = form(
            "bbbb2222",
            "Apply",
            vec![
                question("Nationality", "choice"),
                question("Which country?", "text"),
            ],
        );
        grown.grew_from = "aaaa1111".into();
        remember(&mut memory, SITE, "/apply", vec![first.clone()], true, 10).unwrap();
        remember(&mut memory, SITE, "/apply", vec![grown], true, 20).unwrap();
        assert_eq!(signatures(&memory, "/apply"), ["aaaa1111"]);
        assert!(get(&memory, SITE, "bbbb2222").is_some());
        remember(&mut memory, SITE, "/apply", vec![first], true, 30).unwrap();
        assert!(
            get(&memory, SITE, "bbbb2222").is_some(),
            "the shape the form grows into is still its own"
        );
    }

    /// Which form followed which is remembered on the one that followed.
    #[test]
    fn the_form_that_followed_another_names_it() {
        let mut memory = BTreeMap::new();
        let details = form("aaaa1111", "Your details", vec![question("Name", "text")]);
        let mut payment = form("bbbb2222", "Payment", vec![question("Card", "secret")]);
        payment.follows = "aaaa1111".into();
        remember(&mut memory, SITE, "/apply", vec![details], true, 10).unwrap();
        remember(&mut memory, SITE, "/pay", vec![payment.clone()], true, 20).unwrap();
        let after: Vec<&str> = followers(&memory, SITE, "aaaa1111")
            .into_iter()
            .map(|form| form.label.as_str())
            .collect();
        assert_eq!(after, ["Payment"]);
        // Read again without having come from the first form, it still follows it.
        payment.follows.clear();
        remember(&mut memory, SITE, "/pay", vec![payment], true, 30).unwrap();
        assert_eq!(followers(&memory, SITE, "aaaa1111").len(), 1);
    }

    /// Memory takes a form's shape and nothing else, through any door.
    #[test]
    fn only_a_forms_shape_is_remembered() {
        let mut memory = BTreeMap::new();
        let ok = || form("aaaa1111", "Apply", vec![question("Name", "text")]);
        let refused = |origin: &str, page: &str, form: RememberedForm| {
            remember(&mut BTreeMap::new(), origin, page, vec![form], true, 1).is_err()
        };
        assert!(refused("https://visa.test/apply", "/apply", ok()));
        assert!(refused("mailto:a@b", "/apply", ok()));
        assert!(refused(SITE, "apply", ok()));
        assert!(refused(SITE, "/apply", form("not a signature", "", vec![])));
        assert!(refused(
            SITE,
            "/apply",
            form("aaaa1111", "", vec![question("Name", "essay")])
        ));
        assert!(refused(
            SITE,
            "/apply",
            form("aaaa1111", &"x".repeat(MAX_TEXT + 1), vec![])
        ));
        let mut taught = question("Name", "text");
        taught.fallback = "rejected".into();
        assert!(
            refused(SITE, "/apply", form("aaaa1111", "", vec![taught])),
            "a refusal of one answer is not a property of the field"
        );
        remember(&mut memory, SITE, "/apply", vec![ok()], true, 1).unwrap();
        assert_eq!(memory.len(), 1);
    }

    /// What is remembered is listed by site, and counted as a person counts
    /// forms: the shape a form grows into is the same form.
    #[test]
    fn memory_is_listed_by_site_with_one_entry_for_each_form() {
        let mut memory = BTreeMap::new();
        let first = form("aaaa1111", "Apply", vec![question("Nationality", "choice")]);
        let mut grown = form(
            "bbbb2222",
            "Apply",
            vec![question("Which country?", "text")],
        );
        grown.grew_from = "aaaa1111".into();
        remember(&mut memory, SITE, "/apply", vec![first], true, 10).unwrap();
        remember(&mut memory, SITE, "/apply", vec![grown], true, 20).unwrap();
        // A site whose origin starts with the other's is another site.
        let other = "https://visa.test.example";
        remember(
            &mut memory,
            other,
            "/",
            vec![form("cccc3333", "Search", vec![])],
            true,
            30,
        )
        .unwrap();
        let listed: Vec<(&str, Vec<&str>)> = sites(&memory)
            .into_iter()
            .map(|(site, forms)| {
                let signatures = forms.iter().map(|form| form.signature.as_str()).collect();
                (site, signatures)
            })
            .collect();
        assert_eq!(
            listed,
            [
                (SITE, vec!["aaaa1111", "bbbb2222"]),
                (other, vec!["cccc3333"])
            ]
        );
        let counted: Vec<&str> = forms_of(&memory, SITE)
            .into_iter()
            .map(|form| form.signature.as_str())
            .collect();
        assert_eq!(counted, ["aaaa1111"]);
        // The shape a form grew into stands for it once the first is gone.
        memory.remove(&key(SITE, "aaaa1111"));
        assert_eq!(forms_of(&memory, SITE)[0].signature, "bbbb2222");
    }

    /// Forgetting a form forgets every shape of it and nothing else: the
    /// form that followed it stays, following nothing; forgetting a site
    /// leaves the other sites.
    #[test]
    fn a_form_is_forgotten_in_every_shape_and_a_site_with_all_its_forms() {
        let mut memory = BTreeMap::new();
        let first = form("aaaa1111", "Apply", vec![question("Nationality", "choice")]);
        let mut grown = form(
            "bbbb2222",
            "Apply",
            vec![question("Which country?", "text")],
        );
        grown.grew_from = "aaaa1111".into();
        let mut payment = form("cccc3333", "Payment", vec![question("Card", "secret")]);
        payment.follows = "aaaa1111".into();
        let other = "https://shop.test";
        remember(&mut memory, SITE, "/apply", vec![first], true, 10).unwrap();
        remember(&mut memory, SITE, "/apply", vec![grown], true, 20).unwrap();
        remember(&mut memory, SITE, "/pay", vec![payment], true, 30).unwrap();
        remember(
            &mut memory,
            other,
            "/",
            vec![form("dddd4444", "Search", vec![])],
            true,
            40,
        )
        .unwrap();

        assert_eq!(forget_form(&mut memory, SITE, "ffff0000"), 0);
        assert_eq!(forget_form(&mut memory, other, "aaaa1111"), 0);
        assert_eq!(memory.len(), 4, "a form nobody remembers forgets nothing");
        // Named by the shape it grew into, the form goes in both.
        assert_eq!(forget_form(&mut memory, SITE, "bbbb2222"), 2);
        assert!(get(&memory, SITE, "aaaa1111").is_none());
        assert!(get(&memory, SITE, "bbbb2222").is_none());
        assert_eq!(
            get(&memory, SITE, "cccc3333").map(|form| form.follows.as_str()),
            Some(""),
            "the form that followed it follows nothing any more"
        );
        assert!(signatures(&memory, "/apply").is_empty());

        assert_eq!(forget_site(&mut memory, SITE), 1);
        assert_eq!(forget_site(&mut memory, SITE), 0);
        assert_eq!(
            memory.keys().map(String::as_str).collect::<Vec<_>>(),
            ["https://shop.test dddd4444"]
        );
    }

    /// Memory is bounded: the forms read longest ago go first.
    #[test]
    fn the_forms_read_longest_ago_go_first() {
        let mut memory = BTreeMap::new();
        for number in 0..=MAX_REMEMBERED_FORMS {
            let signature = format!("f{number:07}");
            remember(
                &mut memory,
                SITE,
                &format!("/page/{number}"),
                vec![form(&signature, "Form", vec![])],
                true,
                u64::try_from(number).unwrap() + 1,
            )
            .unwrap();
        }
        assert_eq!(memory.len(), MAX_REMEMBERED_FORMS);
        assert!(get(&memory, SITE, "f0000000").is_none());
        assert!(get(&memory, SITE, "f0000001").is_some());
    }
}
