use ui::framework::views::QuerySpec;

use super::*;

/// Rows as the catalogue writes them: a subtitle on its own line inside a
/// quoted title, authors with their dates, an illustrator, an audio book.
const CATALOGUE: &str = "Text#,Type,Issued,Title,Language,Authors,Subjects,LoCC,Bookshelves\r
2,Text,1972-12-01,\"The United States Bill of Rights
The Ten Original Amendments to the Constitution of the United States\",en,United States,Civil rights,JK,Politics\r
84,Text,1993-10-01,\"Frankenstein; Or, The Modern Prometheus\",en,\"Shelley, Mary Wollstonecraft, 1797-1851\",Horror tales,PR,\"Gothic Fiction; Movie Books\"\r
2701,Text,2001-07-01,\"Moby Dick; Or, The Whale\",en,\"Melville, Herman, 1819-1891\",Whaling -- Fiction,PS,Best Books Ever Listings\r
15,Text,1991-01-01,\"Moby-Dick; or, The Whale\",en,\"Melville, Herman, 1819-1891\",Whaling,PS,\r
9001,Sound,2005-01-01,Moby Dick (audio),en,\"Melville, Herman, 1819-1891\",Whaling,PS,\r
41445,Text,2012-11-21,\"Frankenstein \"\"Illustrated\"\"\",en,\"Ward, Lynd, 1905-1985 [Illustrator]; Shelley, Mary Wollstonecraft, 1797-1851\",Horror,PR,\r
";

fn words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(ui::search::normalize_search_text)
        .collect()
}

fn ids(books: &[StoreBook]) -> Vec<u64> {
    books.iter().map(|book| book.id).collect()
}

#[test]
fn the_catalogue_reads_as_its_texts_with_their_authors_as_people_write_them() {
    let index = CatalogueIndex::parse(CATALOGUE);
    assert_eq!(index.len(), 5, "the audio book is not one Books reads");
    let rights = &index.search(&words("bill rights"), 10)[0];
    assert_eq!(
        rights.title,
        "The United States Bill of Rights: The Ten Original Amendments to the Constitution of the United States"
    );
    let frankenstein = index.search(&words("frankenstein"), 10);
    assert_eq!(ids(&frankenstein), [41445, 84]);
    assert_eq!(frankenstein[0].title, "Frankenstein \"Illustrated\"");
    assert_eq!(
        frankenstein[0].author, "Mary Wollstonecraft Shelley",
        "an illustrator is not the author"
    );
    assert_eq!(frankenstein[1].author, "Mary Wollstonecraft Shelley");
}

#[test]
fn every_word_must_match_and_a_title_match_comes_before_an_author_match() {
    let index = CatalogueIndex::parse(CATALOGUE);
    assert_eq!(
        ids(&index.search(&words("moby whale"), 10)),
        [15, 2701],
        "titles as long as each other in catalogue order"
    );
    assert_eq!(ids(&index.search(&words("melville"), 10)), [15, 2701]);
    assert_eq!(
        ids(&index.search(&words("moby melville"), 10)),
        [15, 2701],
        "found partly by their author"
    );
    assert!(index.search(&words("moby austen"), 10).is_empty());
    assert!(index.search(&[], 10).is_empty());
    assert_eq!(index.search(&words("the"), 2).len(), 2);
}

#[test]
fn the_compressed_copy_reads_like_the_catalogue() {
    use std::io::Write as _;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(CATALOGUE.as_bytes()).unwrap();
    let index = CatalogueIndex::from_gzip(&gzip.finish().unwrap()).unwrap();
    assert_eq!(index.len(), 5);
    assert!(CatalogueIndex::from_gzip(b"not gzip").is_none());
}

#[test]
fn a_copy_is_kept_on_a_computer_and_not_in_a_browser_unless_the_person_says() {
    use ui::storage::Settings;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.json");
    let on = |device| {
        Settings::<CatalogueCopies>::for_app("so.tilo.ebook")
            .in_file(&file)
            .on_device(device)
    };
    assert!(on(DeviceClass::Native).get().keep_catalogues);
    assert!(!on(DeviceClass::Web).get().keep_catalogues);
    assert!(
        !on(DeviceClass::Web).available(),
        "a browser keeps no copy, so it offers no switch"
    );
    on(DeviceClass::Web)
        .set(&CatalogueCopies {
            keep_catalogues: true,
        })
        .unwrap();
    assert!(on(DeviceClass::Web).get().keep_catalogues);
}

/// The daemon's copy as a test holds it: kept by Books or not, here or
/// still on its way.
#[derive(Default)]
struct Stand {
    kept: Mutex<bool>,
    arrived: Mutex<bool>,
    calls: Mutex<Vec<&'static str>>,
}

impl Stand {
    fn gzip() -> Vec<u8> {
        use std::io::Write as _;
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(CATALOGUE.as_bytes()).unwrap();
        gzip.finish().unwrap()
    }

    fn arrive(&self) {
        *self.arrived.lock().unwrap() = true;
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

impl PublicFile for Stand {
    fn keep(&self) -> std::io::Result<()> {
        self.calls.lock().unwrap().push("keep");
        *self.kept.lock().unwrap() = true;
        Ok(())
    }

    fn release(&self) -> std::io::Result<()> {
        self.calls.lock().unwrap().push("release");
        *self.kept.lock().unwrap() = false;
        Ok(())
    }

    fn here(&self) -> std::io::Result<Option<String>> {
        let here = *self.arrived.lock().unwrap();
        Ok(here.then(|| blake3::hash(&Self::gzip()).to_hex().to_string()))
    }

    fn read(&self) -> std::io::Result<PublicState> {
        Ok(match self.here()? {
            Some(hash) => PublicState::Here {
                bytes: Self::gzip(),
                hash,
            },
            None if *self.kept.lock().unwrap() => PublicState::Arriving,
            None => PublicState::NotKept,
        })
    }
}

fn store_search(store: &crate::store::Gutenberg, text: &str) -> Vec<String> {
    use ui::framework::views::{Cancel, RemoteSource as _};
    let mut titles = Vec::new();
    store
        .search(&QuerySpec::text(text), &Cancel::default(), &mut |page| {
            titles.extend(page.into_iter().map(|item| item.id));
        })
        .unwrap();
    titles
}

#[test]
fn the_store_answers_from_the_copy_while_it_is_kept_and_here() {
    let stand = Arc::new(Stand::default());
    let catalogue = DeviceCatalogue::over(Some(Arc::clone(&stand) as Arc<dyn PublicFile>));
    assert!(
        catalogue.index().is_none(),
        "nothing is read before it is kept"
    );

    catalogue.keep(true);
    assert!(
        catalogue.index().is_none(),
        "on its way: the network answers"
    );
    stand.arrive();
    let store = crate::store::Gutenberg::with_catalogue(Arc::clone(&catalogue));
    assert_eq!(
        store_search(&store, "moby whale"),
        ["gutenberg:15", "gutenberg:2701"],
        "the copy answers, nothing goes over the network"
    );

    // Turned off: the copy is released, left for the device to evict, and
    // the network answers again.
    catalogue.keep(false);
    assert_eq!(stand.calls(), ["keep", "release"]);
    assert!(catalogue.index().is_none());
    catalogue.keep(false);
    assert_eq!(stand.calls(), ["keep", "release"], "told once");
}

#[test]
fn without_a_copy_nothing_is_answered_from_the_device() {
    let catalogue = DeviceCatalogue::over(None);
    catalogue.keep(true);
    assert!(catalogue.index().is_none());
}
