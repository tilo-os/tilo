use example_localized::{App, Shelf};
use ui::headless::{InspectionNode, SemanticExpectation};
use ui::i18n::{I18nManager, PseudoLocale};
use ui::prelude::TestApp;

/// The app in a known language, whatever the machine running the test uses.
#[expect(clippy::expect_used, reason = "a test helper fails the test loudly")]
fn open(locale: &str) -> TestApp<App> {
    let mut app = TestApp::<App>::new().expect("the app opens");
    I18nManager::set_locale(locale);
    app.harness().relayout();
    app
}

#[expect(clippy::panic, reason = "a test helper fails the test loudly")]
fn says(app: &mut TestApp<App>, target: &str, name: &str) {
    app.harness()
        .expect(&SemanticExpectation::named(target).name(name))
        .unwrap_or_else(|error| panic!("{target} should read {name:?}: {error:?}"));
}

/// Plural forms come from the catalog, per language.
#[test]
fn counts_use_each_languages_plural_forms() {
    let mut app = open("en-US");
    says(&mut app, "book-count", "The shelf is empty.");
    app.invoke(Shelf::ADD_BOOK.invoke()).expect("add");
    says(&mut app, "book-count", "One book on the shelf.");
    app.invoke(Shelf::ADD_BOOK.invoke()).expect("add");
    says(&mut app, "book-count", "2 books on the shelf.");

    I18nManager::set_locale("de-DE");
    app.harness().relayout();
    says(&mut app, "book-count", "2 Bücher im Regal.");
    app.invoke(Shelf::REMOVE_BOOK.invoke()).expect("remove");
    says(&mut app, "book-count", "Ein Buch im Regal.");
}

/// Arguments are placed by the catalog, and the view switches language
/// without being rebuilt.
#[test]
fn arguments_follow_the_language() {
    let mut app = open("en-US");
    says(&mut app, "greeting", "Welcome!");
    app.invoke(Shelf::SET_READER.with("Ada".into()))
        .expect("name the reader");
    says(&mut app, "greeting", "Welcome, Ada!");
    I18nManager::set_locale("de-DE");
    app.harness().relayout();
    says(&mut app, "greeting", "Willkommen, Ada!");
    says(&mut app, "add-book", "Buch hinzufügen");
}

fn names(node: &InspectionNode, out: &mut Vec<String>) {
    if let Some(name) = node
        .semantics
        .as_ref()
        .and_then(|semantics| semantics.name.clone())
        .filter(|name| !name.is_empty())
    {
        out.push(name);
    }
    for child in &node.children {
        names(child, out);
    }
}

/// Under the pseudo-locale every catalog message is bracketed and
/// lengthened, so a literal that bypassed the catalog stands out, and so does
/// a layout that cannot take longer words.
#[test]
fn no_text_bypasses_the_catalog() {
    let mut app = open("en-US");
    I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
    app.harness().relayout();
    let mut shown = Vec::new();
    names(&app.harness().inspection().root, &mut shown);
    I18nManager::set_pseudo_locale(PseudoLocale::Off);
    assert!(!shown.is_empty());
    let literal = shown
        .iter()
        .filter(|name| !name.contains('⟦'))
        .collect::<Vec<_>>();
    assert!(literal.is_empty(), "not from the catalog: {literal:?}");
}

#[test]
fn the_shelf_cannot_go_below_empty() {
    let mut app = open("en-US");
    assert!(app.invoke(Shelf::REMOVE_BOOK.invoke()).is_err());
    assert_eq!(app.state().books, 0);
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = open("en-US");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
