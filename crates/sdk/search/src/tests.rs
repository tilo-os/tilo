use std::sync::Arc;

use crate::{
    BodyPiece, BodySource, Document, FUZZY_RANK, FieldWeight, MatchTier, Matching, Page,
    SPANNING_RANK, SearchIndex,
};

fn committed(matching: Matching, documents: impl IntoIterator<Item = Document>) -> SearchIndex {
    let mut index = SearchIndex::new(matching);
    index.replace_all(documents);
    index.commit().unwrap();
    index
}

fn titles(titles: &[&str]) -> SearchIndex {
    committed(
        Matching::default(),
        titles
            .iter()
            .enumerate()
            .map(|(key, title)| Document::new(key as u64).field(*title, FieldWeight::TITLE)),
    )
}

fn keys(index: &SearchIndex, query: &str) -> Vec<u64> {
    index
        .search(query, Page::first(64))
        .hits
        .iter()
        .map(|hit| hit.key)
        .collect()
}

fn rank(index: &SearchIndex, query: &str) -> Option<i32> {
    index.search(query, Page::first(1)).hits.first()?.rank
}

#[test]
fn tiers_rank_exact_prefix_word_prefix_and_infix_matches() {
    let index = titles(&["dune", "dune notes", "the-dune-notes.md", "redundant"]);
    let hits = index.search("dune", Page::first(8)).hits;
    let ranked: Vec<_> = hits.iter().map(|hit| (hit.key, hit.rank)).collect();
    assert_eq!(
        ranked,
        [
            (0, Some(MatchTier::Exact.percent())),
            (1, Some(MatchTier::PrefixWord.percent())),
            (2, Some(MatchTier::Word.percent())),
        ]
    );
    // The start of a longer word ranks below a whole word.
    let words = titles(&["dunes", "sand dunes"]);
    assert_eq!(rank(&words, "dune"), Some(MatchTier::Prefix.percent()));
    assert_eq!(
        words
            .search("dune", Page::first(8))
            .hits
            .get(1)
            .and_then(|hit| hit.rank),
        Some(MatchTier::WordPrefix.percent())
    );
    // Letters and digits are separate words.
    assert_eq!(
        rank(&titles(&["report2026"]), "2026"),
        Some(MatchTier::Word.percent())
    );
    assert_eq!(rank(&index, "dun"), Some(MatchTier::Prefix.percent()));
    let infix = index.search("undan", Page::first(8)).hits;
    assert_eq!(infix.len(), 1);
    assert_eq!(infix[0].rank, Some(MatchTier::Contains.percent()));
}

#[test]
fn a_later_word_start_beats_an_earlier_infix() {
    let index = titles(&["redundant dune"]);
    assert_eq!(rank(&index, "dun"), Some(MatchTier::WordPrefix.percent()));
}

#[test]
fn short_queries_match_only_at_word_starts() {
    let index = titles(&["abc", "xyz", "zab", "Enroll another device", "Notes"]);
    assert_eq!(keys(&index, "ab"), [0]);
    assert_eq!(keys(&index, "z"), [2]);
    assert!(keys(&index, "b").is_empty());
    // "not" is not looking for "an·not·her".
    assert_eq!(keys(&index, "not"), [4]);
    // From four letters a remembered fragment matches inside a word.
    assert_eq!(keys(&index, "othe"), [3]);
    assert_eq!(keys(&index, "abcd"), Vec::<u64>::new());
    // Only an empty query browses; spaces are text like any other.
    assert!(keys(&index, "   ").is_empty());
    // A script without word spaces still matches inside a run.
    assert_eq!(keys(&titles(&["東京都の地図"]), "地図"), [0]);
}

#[test]
fn descriptions_and_keywords_never_match_inside_a_word() {
    let index = committed(
        Matching::default(),
        [
            Document::new(0)
                .field("Display brightness", FieldWeight::TITLE)
                .field("darker brighter", FieldWeight::KEYWORD)
                .field("Make the screen darker", FieldWeight::SUBTITLE),
            Document::new(1).field("Dark theme", FieldWeight::TITLE),
        ],
    );
    assert_eq!(
        keys(&index, "dark"),
        [1, 0],
        "a word start in a keyword still counts"
    );
    assert!(keys(&index, "arker").is_empty());
    assert!(keys(&index, "ightn").len() == 1, "inside a name it does");
}

#[test]
fn weights_order_title_over_subtitle_over_body() {
    let index = committed(
        Matching::default(),
        [
            Document::new(0).field("an atlas of stars", FieldWeight::BODY),
            Document::new(1).field("Atlas Press", FieldWeight::SUBTITLE),
            Document::new(2).field("Atlas", FieldWeight::TITLE),
        ],
    );
    assert_eq!(keys(&index, "atlas"), [2, 1, 0]);
}

#[test]
fn folding_and_unicode() {
    let index = titles(&[
        "Crème Brûlée",
        "Ｔｏｋｙｏ",
        "東京都の地図",
        "大阪の地図",
        "a\0\0",
    ]);
    assert_eq!(keys(&index, "creme brulee"), [0]);
    assert_eq!(rank(&index, "CRÈME BRÛLÉE"), Some(100));
    assert_eq!(keys(&index, "tokyo"), [1]);
    assert_eq!(keys(&index, "東京"), [2]);
    assert_eq!(keys(&index, "地図"), [2, 3]);
    assert_eq!(keys(&index, "a\0\0"), [4]);
}

#[test]
fn fields_never_match_across_each_other() {
    let index = committed(
        Matching::default(),
        [Document::new(0)
            .field("Travel notes", FieldWeight::TITLE)
            .field("an atlas", FieldWeight::BODY)],
    );
    assert!(keys(&index, "notes an").is_empty());
    assert_eq!(keys(&index, "notes"), [0]);
}

#[test]
fn unranked_fields_keep_key_order() {
    let index = committed(
        Matching::default(),
        [
            Document::new(1).field("xx atlas", FieldWeight::UNRANKED),
            Document::new(2).field("atlas", FieldWeight::UNRANKED),
        ],
    );
    let hits = index.search("atlas", Page::first(8)).hits;
    assert_eq!(hits.iter().map(|hit| hit.key).collect::<Vec<_>>(), [1, 2]);
    assert!(hits.iter().all(|hit| hit.rank == Some(0)));
}

#[test]
fn equal_ranks_come_in_key_order() {
    let index = committed(
        Matching::default(),
        [
            Document::new(0).field("report plan budget", FieldWeight::TITLE),
            Document::new(1).field("plan", FieldWeight::TITLE),
            Document::new(2).field("roadmap plan", FieldWeight::TITLE),
        ],
    );
    // The exact title first, then both word-prefix hits by key.
    assert_eq!(keys(&index, "plan"), [1, 0, 2]);
}

#[test]
fn empty_queries_browse_in_key_order_and_page() {
    let index = titles(&["c", "a", "b"]);
    let first = index.search("", Page::first(2));
    assert_eq!(
        first.hits.iter().map(|hit| hit.key).collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(first.more);
    let rest = index.search(
        "",
        Page {
            offset: 2,
            limit: 2,
        },
    );
    assert_eq!(rest.hits.iter().map(|hit| hit.key).collect::<Vec<_>>(), [2]);
    assert!(!rest.more);
}

#[test]
fn incremental_and_bulk_commits_agree() {
    let mut index = SearchIndex::default();
    index.replace_all(
        (0..100).map(|key| Document::new(key).field("common row", FieldWeight::TITLE)),
    );
    index.commit().unwrap();
    // A small delta commits incrementally.
    index.upsert(Document::new(7).field("rare needle", FieldWeight::TITLE));
    assert!(index.remove(8));
    assert!(!index.remove(1_000));
    assert!(index.is_dirty());
    assert_eq!(
        keys(&index, "needle"),
        Vec::<u64>::new(),
        "searches read the last commit"
    );
    index.commit().unwrap();
    assert!(!index.is_dirty());
    assert_eq!(keys(&index, "needle"), [7]);
    assert_eq!(index.search("common", Page::first(1_000)).hits.len(), 98);
    // A large one rebuilds.
    for key in 0..50 {
        index.upsert(Document::new(key).field("other", FieldWeight::TITLE));
    }
    index.commit().unwrap();
    assert_eq!(index.search("common", Page::first(1_000)).hits.len(), 50);
    assert_eq!(index.len(), 100);

    let copy = {
        let mut copy = index.detached();
        copy.upsert(Document::new(500).field("only in the copy", FieldWeight::TITLE));
        copy.commit().unwrap();
        copy
    };
    assert_eq!(keys(&copy, "only in"), [500]);
    assert!(keys(&index, "only in").is_empty());
}

struct Paragraphs(Vec<&'static str>);

impl BodySource for Paragraphs {
    fn segment_count(&self) -> usize {
        self.0.len()
    }
    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment < self.0.len())
    }
    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        (piece == 0).then(|| BodyPiece {
            text: self.0.get(segment).copied().unwrap_or_default(),
            space_before: segment > 0,
        })
    }
    fn locates(&self, segment: usize) -> bool {
        segment > 0
    }
}

#[test]
fn bodies_follow_fields_and_locate_their_segments() {
    let index = committed(
        Matching::default(),
        [
            Document::new(0)
                .field("Unrelated", FieldWeight::TITLE)
                .body(Arc::new(Paragraphs(vec![
                    "harbour lights in the opening",
                    "the harbour lights at dusk, harbour lights again",
                ]))),
            Document::new(1).field("Harbour lights", FieldWeight::UNRANKED),
        ],
    );
    let hits = index.search("harbour lights", Page::first(8)).hits;
    let found: Vec<_> = hits
        .iter()
        .map(|hit| (hit.key, hit.segment, hit.rank))
        .collect();
    // The field hit first; then the located paragraph, which holds the
    // phrase twice, above the record's other body text by BM25.
    assert_eq!(
        found,
        [(1, None, Some(0)), (0, Some(1), None), (0, None, None)]
    );
}

#[test]
fn loose_matching_finds_spread_words_and_typos_below_substrings() {
    let documents = [
        Document::new(0)
            .field("LibreOffice Calc", FieldWeight::TITLE)
            .field("office spreadsheet program", FieldWeight::BODY),
        Document::new(1).field("Calculator", FieldWeight::TITLE),
        Document::new(2).field("Firefox", FieldWeight::TITLE),
    ];
    let strict = committed(Matching::default(), documents.clone());
    let loose = committed(Matching::LOOSE, documents);
    assert!(keys(&strict, "calc spreadsheet").is_empty());
    assert_eq!(rank(&loose, "calc spreadsheet"), Some(SPANNING_RANK));
    assert!(keys(&strict, "fierfox").is_empty());
    assert_eq!(keys(&loose, "fierfox"), [2]);
    assert_eq!(rank(&loose, "fierfox"), Some(FUZZY_RANK));
    // As-you-type: the last word may be an unfinished, misspelt prefix.
    assert_eq!(keys(&loose, "calcul"), [1]);
    assert_eq!(keys(&loose, "calcuk"), [1]);
    // A substring match always outranks a looser one.
    assert!(rank(&loose, "spreadsheet").unwrap() > SPANNING_RANK);
    // Short words never match by typo; four letters take one.
    assert!(keys(&loose, "clc").is_empty());
    assert_eq!(keys(&loose, "fier"), [2]);
}

#[test]
fn a_four_letter_typo_and_typos_among_spread_words_still_find_it() {
    let documents = [
        Document::new(0)
            .field("Notes", FieldWeight::TITLE)
            .field("notizen", FieldWeight::KEYWORD),
        Document::new(1)
            .field("New note", FieldWeight::TITLE)
            .field("Start an empty note", FieldWeight::SUBTITLE),
    ];
    let loose = committed(Matching::LOOSE, documents);
    assert_eq!(keys(&loose, "nots"), [0, 1]);
    assert!(rank(&loose, "nots").unwrap() <= FUZZY_RANK);
    // "empty" spans into the subtitle, "nots" is a typo of the title word.
    assert_eq!(keys(&loose, "empty nots"), [1]);
    assert!(rank(&loose, "empty nots").unwrap() < SPANNING_RANK);
}

#[test]
fn a_short_word_does_not_find_another_word_one_letter_off() {
    let documents = [
        Document::new(0).field(
            "Paris Protests Erupt Over High Fuel Costs & Trump Cuts Diesel",
            FieldWeight::TITLE,
        ),
        Document::new(1).field("Cats being cats", FieldWeight::TITLE),
        Document::new(2).field("Notes", FieldWeight::TITLE),
    ];
    for index in [committed(Matching::LOOSE, documents.clone()), {
        let mut index = SearchIndex::new(Matching::LOOSE).through_tantivy();
        index.replace_all(documents);
        index.commit().unwrap();
        index
    }] {
        // "cats" is not "cuts" mistyped, though it is one letter away.
        assert_eq!(keys(&index, "cats"), [1]);
        // A dropped or swapped letter still finds the word.
        assert_eq!(keys(&index, "nots"), [2]);
        assert_eq!(keys(&index, "ntoes"), [2]);
    }
}

#[test]
fn numbers_carry_no_typos_and_punctuation_opens_a_word() {
    let documents = (0..12).map(|at| {
        Document::new(at)
            .field(format!("Library volume {}", 2990 + at), FieldWeight::TITLE)
            .field(format!("src/volume_{at}.rs"), FieldWeight::SUBTITLE)
    });
    for index in [committed(Matching::LOOSE, documents.clone()), {
        let mut index = SearchIndex::new(Matching::LOOSE).through_tantivy();
        index.replace_all(documents);
        index.commit().unwrap();
        index
    }] {
        // "2999" is one record, not the ten numbers a typo away from it.
        assert_eq!(keys(&index, "volume 2999"), [9]);
        assert_eq!(keys(&index, "2999"), [9]);
        // An extension matches from the word after its dot, in a subtitle
        // too, where nothing matches inside a word.
        assert_eq!(keys(&index, ".rs").len(), 12);
    }
}

#[test]
fn a_typo_in_a_name_outranks_the_same_typo_in_a_keyword() {
    let loose = committed(
        Matching::LOOSE,
        [
            Document::new(0)
                .field("Calendar", FieldWeight::TITLE)
                .field("termin", FieldWeight::KEYWORD),
            Document::new(1).field("Terminal", FieldWeight::TITLE),
        ],
    );
    assert_eq!(keys(&loose, "terminl"), [1, 0]);
    assert!(rank(&loose, "terminl").unwrap() <= FUZZY_RANK);
}

#[test]
fn many_records_find_a_late_one() {
    let mut index = SearchIndex::default();
    for key in 0..20_000 {
        let text = if key == 19_999 {
            "row cobalt-orchid-needle".to_owned()
        } else {
            format!("row {key}")
        };
        index.upsert(Document::new(key).field(text, FieldWeight::UNRANKED));
    }
    index.commit().unwrap();
    assert_eq!(keys(&index, "cobalt-orchid-needle"), [19_999]);
    let broad = index.search("row", Page::first(10));
    assert_eq!(broad.hits.len(), 10);
    assert!(broad.more);
    assert!(index.index_bytes() > 0);
}

#[test]
fn custom_ranks_and_scores_reorder_hits() {
    fn whole_words_first(tier: MatchTier, weight: FieldWeight) -> Option<i32> {
        match tier {
            MatchTier::Word => Some(90),
            MatchTier::Prefix => Some(50),
            MatchTier::WordPrefix => Some(40),
            MatchTier::Contains => Some(30),
            _ => tier.rank_in(weight),
        }
    }
    let documents = || {
        [
            Document::new(0)
                .field("opening", FieldWeight::TITLE)
                .signals(0, 7),
            Document::new(1)
                .field("xdg-open", FieldWeight::TITLE)
                .signals(-40, 3),
            Document::new(2)
                .field("open", FieldWeight::TITLE)
                .signals(5, 4),
        ]
    };
    let mut index = SearchIndex::default().with_ranks(whole_words_first);
    index.replace_all(documents());
    index.commit().unwrap();
    assert_eq!(keys(&index, "open"), [2, 1, 0]);
    let scored = index.search_scored("open", Page::first(8), |signals| {
        signals.rank.unwrap_or(0) + signals.boost - i32::try_from(signals.length).unwrap_or(0)
    });
    let order: Vec<_> = scored.hits.iter().map(|hit| (hit.key, hit.score)).collect();
    assert_eq!(order, [(2, 101), (1, 47), (0, 43)]);
}

#[test]
fn keywords_find_a_record_below_its_name_and_by_typo() {
    let documents = [
        Document::new(0)
            .field("Tube", FieldWeight::TITLE)
            .field("youtube", FieldWeight::KEYWORD)
            .field("videos", FieldWeight::KEYWORD),
        Document::new(1).field("Youtube Notes", FieldWeight::TITLE),
        Document::new(2)
            .field("Mail", FieldWeight::TITLE)
            .field("email", FieldWeight::KEYWORD)
            .field("posteingang", FieldWeight::KEYWORD),
    ];
    let loose = committed(Matching::LOOSE, documents);
    // The keyword is an exact hit, ranked below a title prefix.
    assert_eq!(keys(&loose, "youtube"), [1, 0]);
    assert_eq!(
        rank(&loose, "youtube"),
        MatchTier::PrefixWord.rank_in(FieldWeight::TITLE)
    );
    // Only the keyword names Mail, and a typo in it still finds it.
    assert_eq!(keys(&loose, "email"), [2]);
    assert_eq!(keys(&loose, "posteingnag"), [2]);
    assert_eq!(
        keys(&loose, "yotube"),
        [1, 0],
        "a typo in a name leads one in a keyword"
    );
    assert!(
        rank(&loose, "email").unwrap() > rank(&loose, "emial").unwrap(),
        "an exact keyword outranks a misspelt one"
    );
}

#[test]
fn two_letters_begin_a_name_and_match_other_fields_only_as_a_whole_word() {
    let documents = || {
        [
            Document::new(0)
                .field("Speed Test", FieldWeight::TITLE)
                .field("ping", FieldWeight::KEYWORD)
                .field("st", FieldWeight::KEYWORD),
            Document::new(1)
                .field("Camera", FieldWeight::TITLE)
                .field("take a picture", FieldWeight::KEYWORD)
                .field("Takes photos and records video", FieldWeight::SUBTITLE),
            Document::new(2).field("Pixel Editor", FieldWeight::TITLE),
            Document::new(3)
                .field("Notes", FieldWeight::TITLE)
                .field("pi is about three", FieldWeight::BODY),
            Document::new(4)
                .field("Media", FieldWeight::TITLE)
                .field("Watch tv shows", FieldWeight::SUBTITLE),
        ]
    };
    for matching in [Matching::LOOSE, Matching::default()] {
        for index in [committed(matching, documents()), {
            let mut index = SearchIndex::new(matching).through_tantivy();
            index.replace_all(documents());
            index.commit().unwrap();
            index
        }] {
            assert_eq!(keys(&index, "pi"), [2, 3], "a name begun, or a whole word");
            assert_eq!(keys(&index, "p"), [2]);
            assert_eq!(keys(&index, "st"), [0], "initials typed whole");
            assert_eq!(keys(&index, "tv"), [4], "a whole word of a description");
            assert!(keys(&index, "sh").is_empty(), "not a word begun");
            assert_eq!(keys(&index, "pin"), [0], "three letters begin a keyword");
            assert_eq!(keys(&index, "pic"), [1]);
            assert_eq!(keys(&index, "pho"), [1]);
        }
    }
}

/// Records of every weight, some repeating a text, for comparing the
/// direct path with tantivy.
fn alike_documents() -> Vec<Document> {
    let texts = [
        "Wi-Fi: turn off",
        "Switch the Wi-Fi radio of this device",
        "wifi, wi-fi, wlan, wireless network",
        "Keyboard layout",
        "Tastatur & Maus",
        "so.tilo.word-processor",
        "Word Processor",
        "Einstellungen",
        "Screenshot or float part of the screen",
        "report2026-final",
        "dark mode",
        "Bildschirmfoto",
        "posteingang",
        "e-mail",
        "",
    ];
    let weights = [
        FieldWeight::TITLE,
        FieldWeight::new(90),
        FieldWeight::KEYWORD,
        FieldWeight::IDENTITY,
        FieldWeight::SUBTITLE,
        FieldWeight::METADATA,
        FieldWeight::UNRANKED,
    ];
    (0..40u64)
        .map(|key| {
            let at = usize::try_from(key).unwrap();
            (0..=(at % 4)).fold(
                Document::new(key).signals(
                    i32::try_from(at % 3).unwrap(),
                    u32::try_from(at % 5).unwrap(),
                ),
                |document, field| {
                    document.field(
                        *texts
                            .iter()
                            .cycle()
                            .nth(at.wrapping_mul(7).wrapping_add(field.wrapping_mul(3)))
                            .unwrap(),
                        *weights
                            .iter()
                            .cycle()
                            .nth(at.wrapping_add(field.wrapping_mul(5)))
                            .unwrap(),
                    )
                },
            )
        })
        .collect()
}

#[test]
fn direct_and_tantivy_answer_alike() {
    let documents = alike_documents();
    let queries = [
        "",
        "s",
        "w",
        "wi",
        "wi-fi",
        "wifi",
        "wlan",
        "turn",
        "turn off",
        "keyboard layout",
        "keybord",
        "tastatur",
        "word",
        "wp",
        "proc",
        "einstellungen",
        "einstelungen",
        "screenshot",
        "scrennshot",
        "dark",
        "dark mode",
        "mode dark",
        "posteingnag",
        "e-mail",
        "email",
        "2026",
        "2025",
        ".rs",
        "final report",
        "o",
        "of",
        "radio device",
        "keybord layout",
        "layout keybord",
        "scrennshot part",
        "turn of",
        "nots",
        "dar",
        "zzz",
    ];
    for matching in [Matching::LOOSE, Matching::default()] {
        let direct = committed(matching, documents.clone());
        let tantivy = {
            let mut index = SearchIndex::new(matching).through_tantivy();
            index.replace_all(documents.clone());
            index.commit().unwrap();
            index
        };
        for query in queries {
            for page in [
                Page::first(64),
                Page {
                    offset: 3,
                    limit: 5,
                },
            ] {
                assert_eq!(
                    direct.search(query, page),
                    tantivy.search(query, page),
                    "{query:?} {matching:?} {page:?}"
                );
            }
            let score = |signals: crate::Signals| {
                signals.boost * 100 - i32::try_from(signals.length).unwrap()
            };
            assert_eq!(
                direct.search_scored(query, Page::first(64), score),
                tantivy.search_scored(query, Page::first(64), score),
                "{query:?} {matching:?} scored"
            );
        }
    }
}

#[test]
fn a_highlight_marks_exactly_what_the_index_matched() {
    let texts = [
        "Choose an emoji",
        "Insert it where you're typing",
        "Pixel Editor",
        "Speed Test",
        "redundant dune",
        "pi",
        "Le Café Crème",
        "Watch tv shows",
        "src/volume_9.rs",
    ];
    let queries = [
        "pi", "p", "ty", "typi", "dun", "dune", "edit", "crème", "cafe", "afé", "tv", "sh", ".rs",
    ];
    let weights = [
        FieldWeight::TITLE,
        FieldWeight::KEYWORD,
        FieldWeight::SUBTITLE,
        FieldWeight::BODY,
    ];
    for text in texts {
        for weight in weights {
            let index = committed(Matching::default(), [Document::new(0).field(text, weight)]);
            for query in queries {
                let matched = !keys(&index, query).is_empty();
                let marked = crate::locate(query, text, weight);
                assert_eq!(
                    marked.is_some(),
                    matched,
                    "{query:?} in {text:?} at {weight:?}: marked {marked:?}"
                );
                if let Some(range) = marked {
                    assert_eq!(
                        text.get(range).map(crate::fold),
                        Some(crate::fold(query)),
                        "{query:?} in {text:?}: the mark is the query"
                    );
                }
            }
        }
    }
}
