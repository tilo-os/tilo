use super::*;

tilo_test_env::scenario! {
    fn rows_rank_by_strength_of_match_before_kind;

    phase strongest_group_precedes_kind_order_and_ties_preserve_provider_order;
    {
        let mut rows = vec![
            ((ResourceKind::Action, "action"), 0.25),
            ((ResourceKind::File, "file"), 1.0),
            ((ResourceKind::Application, "app z"), 1.0),
            ((ResourceKind::Application, "app a"), 1.0),
            ((ResourceKind::Application, "weak app"), 0.1),
        ];
        order_hits(&mut rows, |(kind, _)| (kind, Availability::OnDevice));
        assert_eq!(
            rows.iter().map(|((_, title), _)| *title).collect::<Vec<_>>(),
            ["app z", "app a", "weak app", "file", "action"]
        );
    }

    phase a_store_offer_follows_every_hit_the_person_has_however_strong;
    {
        let mut merged = vec![
            (
                (
                    ResourceKind::Application,
                    Availability::ThirdPartyCatalogue,
                    "exact store page",
                ),
                9.5,
            ),
            ((ResourceKind::File, Availability::OnDevice, "file"), 2.5),
            (
                (
                    ResourceKind::Application,
                    Availability::ThirdPartyCatalogue,
                    "prefix store page",
                ),
                6.0,
            ),
            (
                (
                    ResourceKind::Application,
                    Availability::OnDevice,
                    "installed app",
                ),
                8.0,
            ),
            (
                (
                    ResourceKind::Application,
                    Availability::TiloCatalogue,
                    "tilo app",
                ),
                7.0,
            ),
        ];
        order_hits(&mut merged, |(kind, availability, _)| (kind, *availability));
        let names: Vec<&str> = merged.iter().map(|((_, _, name), _)| *name).collect();
        assert_eq!(
            names,
            [
                "installed app",
                "file",
                "tilo app",
                "exact store page",
                "prefix store page"
            ]
        );
    }

    phase application_identity_and_metadata_share_native_match_bands;
    {
        let index = tilo_ui_base::text_match::ListIndex::default();
        let score = |query: &str, name, id, category| {
            index.ranks(query, [application_fields(name, id, category)])[0].map(provider_score)
        };
        // The id's last segment names exactly one app and ranks as its name.
        assert_eq!(
            score("hex-editor", "hex editor", "so.tilo.hex-editor", None),
            Some(100)
        );
        assert_eq!(
            score("office", "notes", "so.tilo.notes", Some("office")),
            Some(34)
        );
        assert_eq!(
            score("missing", "calculator", "so.tilo.calculator", None),
            None
        );
    }

    phase exact_title_outranks_prefix_across_kinds;
    {
        let exact = lexical_strength("sheets", &ResourceKind::Note, "Sheets", None);
        let prefix = lexical_strength("sheets", &ResourceKind::Note, "Sheets budget", None);
        let subtitle = lexical_strength("sheets", &ResourceKind::Note, "Budget", Some("Sheets"));
        assert!(exact > prefix && prefix > subtitle && subtitle > 0.0);
    }

    phase launch_verb_does_not_weaken_an_application_title;
    {
        assert!(
            (lexical_strength("sheets", &ResourceKind::Application, "Open Sheets", None) - 4.0).abs()
                < f32::EPSILON
        );
    }

    phase perfect_application_match_beats_an_exact_note_of_the_same_word;
    {
        let strength = |kind, title| {
            merged_strength(
                "sheets",
                &kind,
                title,
                None,
                MAX_PROVIDER_SCORE,
                MAX_PROVIDER_SCORE,
                0.0,
            )
        };
        assert!(
            strength(ResourceKind::Application, "Open Sheets") > strength(ResourceKind::Note, "Sheets")
        );
    }

    phase a_name_beats_a_passage_that_merely_holds_the_word;
    {
        let strength = |kind, title| {
            merged_strength(
                "tasks",
                &kind,
                title,
                None,
                MAX_PROVIDER_SCORE,
                MAX_PROVIDER_SCORE,
                0.0,
            )
        };
        let app = strength(ResourceKind::Application, "Open Tasks");
        let name = strength(ResourceKind::Note, "Tasks");
        // A paragraph of prose whose text holds the word, and an action on something whose title
        // holds it: what the person did not name.
        let paragraph = strength(ResourceKind::Note, "here are the tasks for today");
        let action = strength(ResourceKind::Action, "New note inside the Tasks page");
        assert!(name > paragraph, "the name leads a paragraph mentioning it");
        assert!(name > action, "the name leads an action mentioning it");
        assert!(app > name, "the app named leads the note of the same word");
        let mut merged = vec![
            ((ResourceKind::Action, Availability::OnDevice, "an action"), action),
            ((ResourceKind::Note, Availability::OnDevice, "a paragraph"), paragraph),
            ((ResourceKind::Note, Availability::OnDevice, "a name"), name),
            ((ResourceKind::Application, Availability::OnDevice, "an app"), app),
        ];
        order_hits(&mut merged, |(kind, availability, _)| (kind, *availability));
        let names: Vec<&str> = merged.iter().map(|((_, _, name), _)| *name).collect();
        assert_eq!(
            names,
            ["an app", "a name", "a paragraph", "an action"],
            "Enter opens the app named, then what it names; what merely mentions it comes after"
        );
    }

    phase an_actions_word_typed_whole_counts_as_its_title_begun;
    {
        let action = merged_strength(
            "power off",
            &ResourceKind::Action,
            "Shut down",
            None,
            MAX_PROVIDER_SCORE,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        let begun = merged_strength(
            "shut",
            &ResourceKind::Action,
            "Shut down",
            None,
            MAX_PROVIDER_SCORE,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        // A word the action is known by counts as much as its title begun, but a query that
        // begins the title names it and is never weaker ([`title_names_query`]).
        assert!(begun >= action);
        // Another row that only holds the word is not lifted above the action.
        let setting = merged_strength(
            "power off",
            &ResourceKind::Note,
            "After the power cut",
            None,
            84,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        assert!(action > setting);
        // But a row whose name begins with the query leads it.
        let named = merged_strength(
            "power off",
            &ResourceKind::Note,
            "Power off after",
            None,
            MAX_PROVIDER_SCORE,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        assert!(named > action);
        // A word only begun is no reason to lift it.
        let partial = merged_strength(
            "power",
            &ResourceKind::Action,
            "Shut down",
            None,
            60,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        assert!(partial < begun);
    }
}
tilo_test_env::scenario! {
    fn groups_share_one_budget_fairly;

    phase fair_budget_keeps_totals_and_single_kind_is_exempt;
    {
        let mut rows = vec![ResourceKind::Application];
        rows.extend((0..20).map(|_| ResourceKind::File));
        rows.extend((0..10).map(|_| ResourceKind::Note));
        let groups = group_by_kind(rows, |kind| kind, 8);
        assert_eq!(
            groups
                .iter()
                .map(|g| (g.results.len(), g.total))
                .collect::<Vec<_>>(),
            [(1, 1), (3, 20), (2, 10)],
            "the groups keep their totals; the rows they leave area for are the ones offering the rest"
        );
        // The rows shown plus the row offering each cut group's rest fill the budget: six
        // rows and two "Show all" rows, never a row cut away.
        let offered: usize = groups
            .iter()
            .map(|g| g.results.len() + usize::from(g.is_capped()))
            .sum();
        assert_eq!(offered, 8);
        assert_eq!(
            group_by_kind(vec![ResourceKind::File; 20], |kind| kind, 8)[0]
                .results
                .len(),
            20
        );
    }

    phase groups_share_the_budget_and_an_expanded_group_keeps_every_row;
    {
        let groups = || {
            let mut rows = vec![ResourceKind::Action; 66];
            rows.extend(vec![ResourceKind::Application; 3]);
            rows.extend(vec![ResourceKind::File; 19]);
            group_by_kind(rows, |kind| kind, 0)
        };
        let trimmed = trim_groups(groups(), 12, &[]);
        let shown: Vec<usize> = trimmed.iter().map(|g| g.results.len()).collect();
        assert_eq!(
            shown,
            vec![4, 3, 3],
            "one row each, then round again, leaving the cut groups the row that offers their rest"
        );
        assert!(trimmed[0].is_capped() && !trimmed[1].is_capped());
        assert_eq!(trimmed[0].total, 66, "the header still counts every row");

        let expanded = trim_groups(groups(), 12, &[ResourceKind::File]);
        let file = expanded
            .iter()
            .find(|g| g.kind == ResourceKind::File)
            .expect("files");
        assert_eq!(file.results.len(), 19, "show all means all");
        assert!(!file.is_capped());

        #[cfg(feature = "model")]
        {
            let more = show_all_entry("more:0", "Actions", 66);
            assert!(more.label.starts_with("Show all") && more.label.contains("66"));
            assert_eq!(more.group.as_deref(), Some("Actions"));
            assert_eq!(more.group_total, Some(66));
        }
    }
}
tilo_test_env::scenario! {
    fn filter_chips_fit_their_band_and_act_where_they_are_drawn;

    phase filters_targets_match_semantic_activation_after_draw;
    {
        use crate::search::system::filters::{FilterAction, FilterText, KindChip, SearchFilters};
        use tilo_ui_base::text_render::TextRenderer;
        use tilo_ui_base::theme::Theme;
        use vello::kurbo::{Affine, Rect};
        let mut filters = SearchFilters::default();
        filters.configure(
            vec![
                KindChip::english(ResourceKind::Application),
                KindChip::english(ResourceKind::File),
            ],
            vec![],
            FilterText::default(),
        );
        let mut renderer = TextRenderer::new();
        let mut scene = vello::Scene::new();
        let rows = filters.draw(
            &mut tilo_ui_base::paint::PaintCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &Theme::default(),
            ),
            crate::search::system::filters::FilterStripSlot {
                band: Rect::new(0., 0., 680., 36.),
                row_height: 36.,
            },
            None,
        );
        assert!(rows >= 1);
        let target = filters.rect_for_path(&[0, 1]).unwrap();
        assert_eq!(
            filters.hit(target.center()),
            Some(FilterAction::ToggleKind(ResourceKind::Application))
        );
        assert_eq!(filters.action_for_path(&[1, 1]), None);
        let node = filters.accessibility();
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].children.len(), 3);
    }

    phase a_narrow_band_keeps_one_row_and_a_more_chip;
    {
        use super::filters::{lay_out_chip_row_with_more, lay_out_more_menu};
        use vello::kurbo::{Point, Rect};
        let band = Rect::new(0.0, 0.0, 240.0, 36.0);
        let natural = [40.0, 80.0, 90.0, 70.0, 60.0, 80.0, 70.0];
        let row = lay_out_chip_row_with_more(&natural, 56.0, band, band.x1, 36.0, 24.0, 8.0);
        assert!(row.more.is_some(), "a crowded row must grow a More chip");
        assert!(
            !row.overflow.is_empty(),
            "at least one kind sits behind More"
        );
        assert_eq!(row.rects.len(), natural.len());
        let visible = row.rects.iter().filter(|rect| rect.area() > 0.0).count();
        assert_eq!(
            visible + row.overflow.len(),
            natural.len(),
            "every kind is on the row or behind More"
        );
        for rect in row.rects.iter().filter(|rect| rect.area() > 0.0) {
            assert!(
                (rect.center().y - band.center().y).abs() < 1.0,
                "visible chips stay on one row: {rect:?}"
            );
        }
        let more = row.more.expect("more");
        assert!((more.center().y - band.center().y).abs() < 1.0);
        let menu = lay_out_more_menu(
            &natural,
            &row.overflow,
            Point::new(more.x0, band.y1 + 8.0),
            24.0,
            8.0,
        );
        assert_eq!(menu.len(), row.overflow.len());
        for (idx, rect) in &menu {
            assert!(
                row.overflow.contains(idx),
                "menu entry {idx} was not overflow"
            );
            assert!(rect.area() > 0.0, "overflow kind {idx} is clickable");
        }
    }
}
#[test]
fn every_builtin_kind_is_named_in_the_shells_language() {
    const SOURCE: &str = include_str!("../../../../ui-base/locales/en-US.ftl");
    const GERMAN: &str = include_str!("../../../../ui-base/locales/de-DE.ftl");
    let kinds = [
        ResourceKind::Action,
        ResourceKind::Application,
        ResourceKind::Note,
        ResourceKind::Record,
        ResourceKind::File,
        ResourceKind::Folder,
        ResourceKind::Creator,
        ResourceKind::Video,
        ResourceKind::Playlist,
        ResourceKind::Tab,
        ResourceKind::WindowGroup,
    ];
    let labels = || -> Vec<String> { kinds.iter().map(ResourceKind::display_label).collect() };
    tilo_ui_base::i18n::I18nManager::install("en-US", "en-US", &[("en-US", SOURCE)])
        .expect("the source catalogue parses");
    assert_eq!(
        labels(),
        [
            "Actions",
            "Apps",
            "Notes",
            "Records",
            "Files",
            "Folders",
            "Creators",
            "Videos",
            "Playlists",
            "Tabs",
            "Window groups",
        ]
    );
    tilo_ui_base::i18n::I18nManager::install(
        "en-US",
        "de-DE",
        &[("en-US", SOURCE), ("de-DE", GERMAN)],
    )
    .expect("both catalogues parse");
    assert_eq!(
        labels(),
        [
            "Aktionen",
            "Apps",
            "Notizen",
            "Datensätze",
            "Dateien",
            "Ordner",
            "Creator",
            "Videos",
            "Wiedergabelisten",
            "Tabs",
            "Fenstergruppen",
        ]
    );
    assert_eq!(
        ResourceKind::Custom("Antworten".into()).display_label(),
        "Antworten"
    );
    tilo_ui_base::i18n::I18nManager::set_locale("en-US");
}

/// An action's own words, typed whole, lead a title that only begins with the query: the person
/// said the phrase the action is known by, and a crowded list must keep it rather than cut it for
/// the title that ties at the start of a name.
#[test]
fn an_action_known_by_a_phrase_typed_whole_outranks_a_title_begun_by_it() {
    let keyword = action_ranks(
        crate::search::MatchTier::Exact,
        tilo_ui_base::text_match::FieldWeight::KEYWORD,
    )
    .expect("a keyword typed whole names the action");
    let begun = action_ranks(
        crate::search::MatchTier::Prefix,
        tilo_ui_base::text_match::FieldWeight::TITLE,
    )
    .expect("a title begun names the action");
    assert!(
        keyword > begun,
        "the action's phrase ({keyword}) leads a title begun ({begun})"
    );
    assert_eq!(
        Some(keyword),
        action_ranks(
            crate::search::MatchTier::Exact,
            tilo_ui_base::text_match::FieldWeight::TITLE,
        ),
        "the phrase ranks as the action's title typed whole"
    );
}
