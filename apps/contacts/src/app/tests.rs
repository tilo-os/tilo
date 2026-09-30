use super::*;
use automerge::ROOT;
use automerge::ReadDoc;
use taffy::{AvailableSpace, Dimension, Size, Style};
use tilo_automerge::ApplyIncoming;
use ui::headless::Framed;
use vello::kurbo::Point;

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Sample seeding, view, edit, popup and importing frames render without panicking and with legible chips and a busy announcement.
#[test]
fn every_view_mode_renders_legibly() {
    // --- phase: shared_chip_style_keeps_selected_chips_legible_in_every_app_theme ---
    {
        let quiet = contact_chip_style(&Theme::dark(), false);
        assert!(
            quiet.tint.is_none(),
            "inactive chips use the shared quiet state"
        );
        assert!(
            quiet.resolved.is_none(),
            "inactive chips do not accidentally inherit the selected tint"
        );

        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            let selected = contact_chip_style(&theme, true)
                .resolved
                .expect("selected contacts chips resolve their shared accent treatment");
            assert!(
                ui::a11y::contrast_ratio(selected.ink, selected.fill) >= ON_FILL_MIN_CONTRAST,
                "selected chip label must remain readable on its resolved fill"
            );
            assert!(
                ui::a11y::contrast_ratio(selected.border, ground)
                    >= ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "selected chip edge must remain distinguishable from its pane"
            );
        }
    }

    // --- phase: seeds_sample_contacts_and_selects_one ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.store.len(), 7);
        assert!(app.selected.is_some(), "a contact must be auto-selected");
        // The seeded selection points at a real, richly-populated record.
        let sel = app.store.get(app.selected.unwrap()).unwrap();
        assert!(!sel.phones.is_empty());
    }

    // --- phase: view_mode_frame_does_not_panic ---
    {
        let mut app = ContactsApp::sample();
        drive_frame(&mut app, 1280, 800);
    }

    // --- phase: edit_mode_with_repeated_fields_frame_does_not_panic ---
    {
        let mut app = ContactsApp::sample();
        // Seed first.
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        // Enter edit on the selected contact and add repeated fields.
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(c, false);
        app.form_add(|d| d.phones.push(Labeled::new("work", "")));
        app.form_add(|d| {
            d.addresses.push(Address {
                label: "work".into(),
                ..Default::default()
            });
        });
        drive_frame(&mut app, 1280, 800);
        assert_eq!(app.mode, Mode::Edit);
    }

    // --- phase: calendar_and_type_popups_frame_does_not_panic ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(c, false);
        // Calendar popup.
        app.popup = Some(Popup::Calendar {
            year: 1815,
            month: 12,
            anchor: Rect::new(600.0, 400.0, 700.0, 432.0),
        });
        drive_frame(&mut app, 1280, 800);
        // Type menu popup.
        app.popup = Some(Popup::TypeMenu {
            target: TypeTarget::Phone(0),
            anchor: Rect::new(600.0, 200.0, 678.0, 238.0),
        });
        drive_frame(&mut app, 1280, 800);
    }

    // --- phase: importing_state_is_visible_and_announced_as_busy ---
    {
        let mut app = ContactsApp::sample();
        drive_frame(&mut app, 1280, 800);
        app.shots_show_loading();
        let tree = app.accessibility().expect("contacts accessibility tree");
        let import = access_target(&tree, "contacts.import").expect("import action");
        assert_eq!(import.name.as_deref(), Some("Importing contacts"));
        assert!(import.state.busy);
        assert!(import.state.disabled);
        assert!(!import.state.focusable);
    }
}

/// Resource and search providers stay bounded on large books, resolve exactly, and their activations reveal the addressed contact or group.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn resource_and_search_providers_bound_large_books() {
    // --- phase: real_resource_provider_bounds_large_book_snapshot_and_exact_lookup ---
    {
        const LARGE: usize = 5_000;

        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let template = app.store.contacts[0].clone();
        app.store.contacts = (0..LARGE)
            .map(|index| {
                let mut contact = template.clone();
                contact.id = uuid::Uuid::from_u128(7_000_000 + index as u128);
                contact.first = format!("Contact {index}");
                contact.last.clear();
                contact.groups.clear();
                contact
            })
            .collect();
        let late_id = app.store.contacts.last().expect("late contact").id;
        app.store
            .contacts
            .last_mut()
            .expect("late contact")
            .groups
            .push(GroupEntry::new("Late group"));
        app.rebuild_resource_index();

        let root = ContactsApp::publish_resources(&app, &doc);
        assert_eq!(root.len(), 1);
        assert!(
            app.resource_snapshot_materialized.get()
                <= crate::actions::MAX_CONTEXT_CONTACT_RESOURCES,
            "root snapshot materialised {} contacts",
            app.resource_snapshot_materialized.get()
        );

        let contact_id = crate::actions::contact_resource_id(late_id);
        let contact = ContactsApp::publish_resource(&app, &doc, &contact_id)
            .expect("late contact resolves through the real provider");
        assert_eq!(contact.kind, "contacts.contact");
        assert_eq!(app.resource_exact_materialized.get(), 1);

        let group_id = crate::actions::group_resource_id("Late group");
        let group = ContactsApp::publish_resource(&app, &doc, &group_id)
            .expect("late group resolves through the real provider");
        assert_eq!(group.kind, "contacts.group");
        assert_eq!(app.resource_exact_materialized.get(), 0);

        app.store.contacts.pop();
        assert!(
            ContactsApp::publish_resource(&app, &doc, &contact_id).is_none(),
            "a stale exact id must not redirect to the preceding contact"
        );
        assert_eq!(app.resource_exact_materialized.get(), 0);
    }

    // --- phase: ordinary_local_update_syncs_a_large_book_without_full_retained_rebuild ---
    {
        const LARGE: usize = 5_000;

        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let template = app.store.contacts[0].clone();
        let mut contacts = Vec::with_capacity(LARGE);
        contacts.extend((0..LARGE - 1).map(|index| Contact {
            id: uuid::Uuid::from_u128(9_000_000 + index as u128),
            first: format!("Directory {index}"),
            ..Contact::default()
        }));
        // Keep the one document-backed row at the late position so this test
        // detects any accidental linear local lookup during `sync`.
        contacts.push(template.clone());
        app.store.contacts = contacts;
        app.rebuild_resource_index();
        app.refresh_search_provider();
        app.search_heads.clone_from(&app.model_heads);
        app.resource_index_rebuilds.set(0);
        app.local_change_index_lookups.set(0);
        let refreshes_before = app.search_provider.full_refreshes();

        let id = template.id;
        let action = crate::actions::set_favorite_action(id.to_string(), !template.favorite);
        app.try_perform_action(&mut doc, &action)
            .expect("current local row accepts an action");
        let position = *app
            .resource_index
            .contacts
            .get(&id)
            .expect("late contact has a retained stable-id position");
        assert_eq!(position, LARGE - 1, "the edited record is the late row");
        app.store.contacts[position].favorite = !template.favorite;
        app.sync(&mut doc, &ROOT);

        assert_eq!(
            app.resource_index_rebuilds.get(),
            0,
            "a one-contact local update must not sort/rebuild the large resource index"
        );
        assert_eq!(
            app.search_provider.full_refreshes(),
            refreshes_before,
            "sync applies a one-contact search delta instead of rebuilding the catalogue"
        );
        assert!(
            app.local_change_index_lookups.get() <= 2,
            "the local change inspected {} records instead of using the stable-id map",
            app.local_change_index_lookups.get()
        );
        assert!(
            ui::search::SearchProvider::resolve_action_resource(
                &app.search_provider,
                &crate::actions::open_contact_address(id),
            )
            .is_some(),
            "the incrementally maintained generation still resolves the late edited record"
        );
    }

    // --- phase: search_provider_row_invokes_the_exported_open_action ---
    // Contacts folds its address book into the unified Ctrl+K palette. A
    // field-only query ensures the selected row is the provider hit (not the
    // separately listed action), then activation leaves the palette as the
    // same brokered `open` view operation agents and accessibility consume.
    {
        let mut app = ContactsApp::new();
        for c in sample_contacts() {
            app.store.upsert(c);
        }
        app.rebuild_resource_index();
        app.refresh_search_provider();
        let id = app
            .store
            .contacts
            .iter()
            .find(|contact| contact.display_name() == "Grace Hopper")
            .expect("Grace is seeded")
            .id;
        let mut doc = AutoCommit::new();
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&ui::resources::resources(&app, &doc));
        palette.set_providers(ui::search::capability::search_providers(&app));
        palette.open();
        palette.set_query("navy");
        assert_eq!(palette.result_labels(), vec!["Grace Hopper"]);

        assert!(app.selected.is_none(), "nothing selected yet");
        palette.activate_selected();
        let operation = palette
            .take_operation()
            .expect("canonical search hit queued a broker operation");
        assert_eq!(operation.kind, tilo_actions::OperationKind::View);
        assert_eq!(
            operation
                .context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some(crate::actions::contact_resource_id(id).as_str())
        );
        assert!(
            palette.take_navigation().is_none(),
            "legacy navigation was not used"
        );
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    operation.kind,
                    &operation
                        .encoded()
                        .expect("test requires prepared encoding")
                        .type_id,
                    &operation
                        .encoded()
                        .expect("test requires prepared encoding")
                        .payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("open operation succeeds"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.selected, Some(id), "the open action selected Grace");
    }

    // --- phase: search_activation_reveals_a_contact_hidden_by_the_group_filter ---
    // Opening a contact from the global palette must *reveal* it: if the list
    // is filtered to a group the target is not in, activating the search row
    // drops that filter so the contact is actually visible, not just selected
    // behind a filter. This drives the canonical action address exported to
    // search, accessibility and agents.
    {
        let mut app = ContactsApp::new();
        for c in sample_contacts() {
            app.store.upsert(c);
        }
        app.rebuild_resource_index();
        // Grace Hopper is in "Work" only; narrowing the list to "Friends"
        // (Alan Turing's group) filters her out of the visible set.
        app.group_filter = Some("Friends".to_string());

        let id = app
            .store
            .contacts
            .iter()
            .find(|contact| contact.display_name() == "Grace Hopper")
            .expect("Grace is seeded")
            .id;

        // Precondition: under the Friends filter, Grace is not in the list.
        assert!(
            !app.visible_ids().contains(&id),
            "the Friends group filter hides Grace before activation"
        );

        let mut doc = AutoCommit::new();
        let resources = ui::resources::resources(&app, &doc);
        let address = crate::actions::open_contact_address(id);
        let spec = tilo_actions::door::resolve(&resources, &address)
            .expect("the search address resolves in the agent-visible tree");
        let encoded = match &spec.input {
            tilo_actions::ActionInput::Immediate(encoded) => encoded,
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("open takes no arguments")
            }
        };
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    spec.kind,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("open operation succeeds"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));

        assert_eq!(app.selected, Some(id), "activation selected Grace");
        assert!(
            app.group_filter.is_none(),
            "opening from search dropped the hiding filter"
        );
        assert!(
            app.visible_ids().contains(&id),
            "Grace is now visible in the list — opened, not just selected behind a filter"
        );
    }

    // --- phase: group_open_operation_reveals_its_canonical_filter ---
    // A group is durable collaborative data, not merely a local dropdown
    // string. Its canonical View operation must clear an unrelated search,
    // select the filter, and leave a real matching list for keyboard/pointer
    // users as well as Ctrl+K and agents.
    {
        let mut app = ContactsApp::new();
        for contact in sample_contacts() {
            app.store.upsert(contact);
        }
        app.search.set_text("nothing will match this");
        app.group_filter = Some("Friends".into());
        let group = app
            .store
            .all_groups()
            .into_iter()
            .find(|group| group == "Work")
            .expect("samples expose Work");
        let operation = crate::actions::ContactsOperation::OpenGroup {
            name: group.clone(),
        };
        let payload = serde_json::to_vec(&operation).expect("view operation serializes");

        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::CONTACTS_OPERATION_TYPE,
                    &payload
                ),
                &mut AutoCommit::new(),
                &ROOT
            )
            .expect("group open succeeds"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.group_filter.as_deref(), Some(group.as_str()));
        assert!(
            app.search.value().is_empty(),
            "open reveals a complete group"
        );
        assert!(
            !app.visible_ids().is_empty()
                && app.visible_ids().iter().all(|id| app
                    .store
                    .get(*id)
                    .is_some_and(|c| c.groups.iter().any(|g| group_names_equal(&g.value, &group)))),
            "every revealed row belongs to the canonical group"
        );
    }
}

/// Drive one full five-pass frame against `app` at the given size,
/// encoding the scene on the CPU so any panic in sync/layout/draw
/// surfaces. (GPU rasterization is verified separately by `--shots`.)
fn drive_frame(app: &mut ContactsApp, w: u32, h: u32) {
    drive_frame_with_theme(app, w, h, &Theme::dark());
}

fn drive_frame_with_theme(app: &mut ContactsApp, w: u32, h: u32, theme: &Theme) {
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut doc = AutoCommit::new();

    app.sync(&mut doc, &ROOT);
    let child = app.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        &presence,
        &permission,
    );
    // Encoding the scene is enough to exercise every draw path; the
    // GPU blit is covered by the `--shots` harness.
    let _ = &scene;
}

/// Narrow windows push the detail over the list (by selection, row press or Enter) and back; wide windows stay docked; resizing reflows the same instance.
#[test]
fn single_pane_push_navigation_follows_width() {
    // --- phase: narrow_frame_resolves_single_pane_push ---
    {
        let mut app = ContactsApp::sample();
        // A narrow viewport drives the single-pane master-detail push.
        drive_frame(&mut app, 480, 860);
        assert!(app.narrow, "a narrow viewport resolves to single-pane push");
        assert_eq!(
            app.push.pane(),
            PushPane::Master,
            "a fresh narrow frame opens to the list"
        );
    }

    // --- phase: narrow_selection_pushes_detail_then_back_returns_to_list ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        drive_frame(&mut app, 480, 860);
        assert!(app.narrow);
        // Snap the push to the detail pane (as a row selection would) and
        // confirm the pushed-detail frame renders without panicking.
        app.shots_show_detail();
        assert_eq!(app.push.pane(), PushPane::Detail);
        drive_frame(&mut app, 480, 860);
        // The Back affordance must be laid out (hittable) in the pushed pane.
        assert!(
            app.hit.back.width() > 0.0,
            "pushed detail shows a Back affordance"
        );
    }

    // --- phase: wide_frame_stays_docked_side_by_side ---
    {
        let mut app = ContactsApp::sample();
        drive_frame(&mut app, 1280, 800);
        assert!(
            !app.narrow,
            "a desktop-width viewport keeps the docked split"
        );
        // The list pane sits left of the detail pane (classic split).
        assert!(app.list_rect.x1 <= app.detail_rect.x0 + 0.001);
    }

    // --- phase: narrow_row_press_pushes_detail_then_back_returns_to_list ---
    {
        fn narrow_row_press_pushes_detail_then_back_returns_to_list() {
            let mut h = Harness::new(460, 880);
            assert!(h.app.narrow);
            assert_eq!(h.app.push.pane(), PushPane::Master);
            // Press a list row → the detail pane is pushed in.
            let row = h.row_center(1);
            assert!(h.press_left(row));
            assert_eq!(
                h.app.push.pane(),
                PushPane::Detail,
                "selecting pushes the detail in"
            );
            h.relayout();
            assert!(
                h.app.hit.back.width() > 0.0,
                "the Back affordance is laid out"
            );
            // Press Back → return to the list.
            let back = h.app.hit.back.center();
            assert!(h.press_left(back));
            assert_eq!(
                h.app.push.pane(),
                PushPane::Master,
                "Back returns to the full-width list"
            );
        }
        narrow_row_press_pushes_detail_then_back_returns_to_list();
    }

    // --- phase: enter_opens_the_selected_contact_in_narrow_push_navigation ---
    {
        let mut h = Harness::new(420, 800);
        assert!(h.app.narrow, "phone width uses push navigation");
        assert_eq!(h.app.push.pane(), PushPane::Master);

        assert!(
            h.press_named(NamedKey::Enter),
            "Enter opens the selected contact"
        );
        assert_eq!(h.app.push.pane(), PushPane::Detail);
    }

    // --- phase: resizing_wide_to_narrow_reflows_the_same_instance ---
    {
        // Guards against the "responsive on first launch, frozen after resize"
        // bug: the breakpoint must be re-derived from the CURRENT width on
        // every layout pass, not cached from the first frame.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Lay the SAME instance out wide, then narrow, then wide again.
        let mut app = frame_at(app, doc, 1280, 800);
        assert!(!app.narrow, "wide viewport is the docked split");
        assert!(
            app.list_rect.x1 <= app.detail_rect.x0 + 0.001,
            "list sits left of detail"
        );

        rerender_at(&mut app, 560, 720);
        assert!(
            app.narrow,
            "shrinking below the breakpoint reflows to single-pane push"
        );

        rerender_at(&mut app, 1280, 800);
        assert!(
            !app.narrow,
            "growing back above the breakpoint restores the docked split"
        );
    }
}

/// 200% text reflows and scales interaction geometry and full names fit the detail row.
#[test]
fn text_scale_reflows_geometry_and_names_fit() {
    // --- phase: two_hundred_percent_text_reflows_and_scales_interaction_geometry ---
    {
        let mut app = ContactsApp::sample();
        let large_text = Theme::dark().with_text_scale(2.0);
        drive_frame_with_theme(&mut app, 1440, 900, &large_text);
        assert!(app.narrow, "large text reflows master/detail to one pane");
        assert!(
            app.toolbar_h()
                >= ui::text_metrics::line_height_px(text_base(), 2.0)
                    + 2.0 * f64::from(app.spacing.s3)
        );
        assert_eq!(app.row_h(), ROW_H * 2.0);
        let field_h = at_least_whole(
            FIELD_H,
            line_h(text_base(), 2.0) + 2.0 * f64::from(app.spacing.s2),
        );
        assert_eq!(app.hit.search.height(), field_h);

        app.shots_edit_contact();
        app.layout();
        let form = app.form.as_ref().expect("edit form");
        let first = form
            .inputs
            .iter()
            .find(|field| field.slot == Slot::First)
            .expect("first name")
            .ti_rect();
        let last = form
            .inputs
            .iter()
            .find(|field| field.slot == Slot::Last)
            .expect("last name")
            .ti_rect();
        assert!(last.y0 > first.y1, "paired fields stack at large text");
        assert_eq!(first.height(), field_h);
        let notes = form
            .inputs
            .iter()
            .find(|field| field.slot == Slot::Notes)
            .expect("notes editor");
        assert!(
            notes.ti.is_multiline(),
            "note text wraps instead of clipping"
        );
        assert!(
            notes.ti_rect().height() >= field_h * 3.0,
            "notes reserve multiple large-text lines"
        );
        assert_eq!(notes.ti_rect().height(), 3.0 * field_h);
    }

    // --- phase: detail_name_row_fits_every_full_name_at_moderate_width ---
    {
        // Regression: at 800x600 the detail heading truncated the name
        // ("Ada Lovela…") because the top-right action band stole its
        // horizontal room. The name now sits on its own full-width row, so
        // for every seeded contact the row must be wide enough to fit the
        // full display name at its rendered 26px Bold — i.e. no elision.
        let mut h = Harness::new(800, 600);
        let mut tr = TextRenderer::new();
        let ids: Vec<ContactId> = h.app.store.contacts.iter().map(|c| c.id).collect();
        assert!(!h.app.narrow, "800px is the docked side-by-side split");
        for id in ids {
            h.app.commit_select(id);
            h.relayout();
            let name = h.app.store.get(id).unwrap().display_name();
            let name_rect = h
                .app
                .detail_field_hits
                .iter()
                .find(|(s, _)| *s == Slot::First)
                .map(|(_, r)| *r)
                .expect("the detail header exposes a name row hit rect");
            let measured = f64::from(tr.measure_weighted(&name, 26.0, None, TextWeight::Bold).0);
            assert!(
                name_rect.width() >= measured,
                "name {name:?} needs {measured:.0}px but its row is only {:.0}px — it would truncate",
                name_rect.width(),
            );
        }
    }
}

/// Saves round-trip through the document; refused saves, favourites and deletes reconcile and announce errors without claiming success.
#[test]
fn saves_persist_and_refusals_never_claim_success() {
    // --- phase: save_round_trips_through_doc ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        let id = c.id;
        app.begin_edit(c, false);
        app.form
            .as_mut()
            .and_then(|form| {
                form.inputs
                    .iter_mut()
                    .find(|input| input.slot == Slot::Title)
            })
            .expect("the editor exposes a title input")
            .ti
            .set_text("Edited Title");
        app.save_edit(&mut doc);
        assert_eq!(app.mode, Mode::View);
        let stored = ContactsDoc::list_contacts(&doc);
        let r = stored.iter().find(|x| x.id == id).unwrap();
        assert_eq!(r.title, "Edited Title");
    }

    // --- phase: refused_save_reconciles_and_announces_an_error_without_claiming_success ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let contact = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        ContactsDoc::delete_contact(&mut doc, contact.id);
        app.begin_edit(contact.clone(), false);
        app.save_edit(&mut doc);

        assert_eq!(app.mode, Mode::Edit, "the rejected draft remains editable");
        assert!(app.form.is_some());
        assert!(
            app.store.get(contact.id).is_none(),
            "store reconciles from the document"
        );
        let message = app
            .toast
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap();
        assert!(message.starts_with("Couldn't save contact:"));
        let tree = app.accessibility().unwrap();
        assert!(
            live_message(&tree, message).is_some(),
            "the refusal is announced through the retained live region"
        );
    }

    // --- phase: refused_favorite_and_delete_reconcile_without_false_success ---
    {
        // Simulate a peer deleting the record after this replica rendered its
        // detail controls. Both public mutations must use the action result,
        // not their stale local projection.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let contact = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        ContactsDoc::delete_contact(&mut doc, contact.id);

        assert!(
            !app.toggle_favorite(&mut doc, contact.id),
            "a tombstoned favorite toggle is rejected"
        );
        assert!(
            app.store.get(contact.id).is_none(),
            "favorite refusal reconciles"
        );
        let favorite_error = app.toast.as_ref().unwrap().0.clone();
        assert!(favorite_error.starts_with("Couldn't update favorite:"));
        assert!(
            live_message(&app.accessibility().unwrap(), &favorite_error).is_some(),
            "the favorite refusal is announced"
        );

        // Recreate the stale projection to exercise the independently visible
        // delete-confirmation path against the same tombstone.
        app.store.upsert(contact.clone());
        app.selected = Some(contact.id);
        app.delete_selected(&mut doc, contact.id);
        assert!(
            app.store.get(contact.id).is_none(),
            "delete refusal reconciles"
        );
        let delete_error = app.toast.as_ref().unwrap().0.clone();
        assert!(delete_error.starts_with("Couldn't delete contact:"));
        assert_ne!(delete_error, "Contact deleted", "no false success toast");
        assert!(
            live_message(&app.accessibility().unwrap(), &delete_error).is_some(),
            "the delete refusal is announced"
        );
    }
}

// =================================================================
// Interaction harness — drives real winit events through the
// production `handle_event` path (not test-only shortcuts), so the
// assertions prove the shipped behaviour.
// =================================================================

use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

/// Build a `KeyboardInput` winit event. winit 0.30's `KeyEvent` carries a
/// `pub(crate)` `platform_specific` field, so it can't be struct-literalled
/// — the whole struct is zero-initialised (the same trick `shell`'s and
/// `terminal`'s test harnesses use) then the public fields we care about are
/// overwritten. `text` is what a focused `TextInput` actually inserts.
fn key_input_event(logical_key: Key, text: Option<&str>) -> WindowEvent {
    // SAFETY: every public `KeyEvent` field tolerates the all-zero bit
    // pattern (`Option` → `None`; `Key`/`PhysicalKey`/`KeyLocation`/
    // `ElementState` first variant; `bool` → false) and we overwrite the
    // ones we read below; the private field is inert for our widgets.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical_key;
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = text.map(SmolStr::new);
    WindowEvent::KeyboardInput {
        device_id: device_id(),
        event: ev,
        is_synthetic: false,
    }
}

fn device_id() -> DeviceId {
    // winit gives no public DeviceId constructor; the production code
    // does exactly this for synthesised presses (see
    // `forward_press_to_focused_input`).
    unsafe { std::mem::zeroed() }
}

/// Mounts the app in the production frame host so tests can pump events
/// through the production `Widget::handle_event`.
struct Harness {
    app: Framed<ContactsApp>,
}

impl Harness {
    fn new(w: u32, h: u32) -> Self {
        Harness {
            app: Framed::new(
                ui::State::new(),
                ContactsApp::sample(),
                (w, h),
                Theme::dark(),
            ),
        }
    }

    /// Rebuild, lay out and draw a whole frame (after the model changed). The
    /// draw populates draw-time hit rects (e.g. `detail_field_hits` for
    /// double-click-to-edit), exactly as the live renderer does each frame.
    fn relayout(&mut self) {
        Framed::harness_mut(&mut self.app)
            .recompose()
            .expect("contacts frame lays out");
    }

    fn doc(&self) -> &AutoCommit {
        &Framed::harness(&self.app).state().doc
    }

    fn parts(&mut self) -> (&mut ContactsApp, &mut AutoCommit) {
        let (app, state) = Framed::split(&mut self.app);
        (app, &mut state.doc)
    }

    fn dispatch(&mut self, event: &WindowEvent) -> bool {
        Framed::harness_mut(&mut self.app).dispatch_to_root_with(event, &Modifiers::default())
    }

    fn move_to(&mut self, p: Point) {
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: device_id(),
            position: PhysicalPosition::new(p.x, p.y),
        });
    }

    fn press_left(&mut self, p: Point) -> bool {
        self.move_to(p);
        self.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        })
    }

    fn press_right(&mut self, p: Point) -> bool {
        self.move_to(p);
        self.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        })
    }

    /// A double click: two left presses at the same point in quick
    /// succession (the production double-click window is 400ms).
    fn double_click(&mut self, p: Point) {
        self.press_left(p);
        self.press_left(p);
    }

    /// First visible list row's center point.
    fn row_center(&self, index: usize) -> Point {
        self.app.list_rows[index].1.center()
    }

    fn row_id(&self, index: usize) -> ContactId {
        self.app.list_rows[index].0
    }

    /// Dispatch a key event with explicit modifier state through the real
    /// `handle_event` path.
    fn dispatch_key_mods(&mut self, event: &WindowEvent, mods: ModifiersState) -> bool {
        Framed::harness_mut(&mut self.app).dispatch_to_root_with(event, &mods.into())
    }

    /// Press a single `Named` key (no modifiers).
    fn press_named(&mut self, key: NamedKey) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Named(key), None),
            ModifiersState::empty(),
        )
    }

    /// Press a named key with Shift held (the standard Shift+F10 context-menu
    /// equivalent on keyboards without a dedicated Menu key).
    fn press_named_shift(&mut self, key: NamedKey) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Named(key), None),
            ModifiersState::SHIFT,
        )
    }

    /// Press a single character key (no modifiers) — a global shortcut like
    /// `n`, `e`, `/` when no field is focused.
    fn press_char(&mut self, c: &str) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Character(SmolStr::new(c)), Some(c)),
            ModifiersState::empty(),
        )
    }

    /// Press a character key with Ctrl held (e.g. Ctrl+S, Ctrl+K).
    fn press_char_ctrl(&mut self, c: &str) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Character(SmolStr::new(c)), Some(c)),
            ModifiersState::CONTROL,
        )
    }

    /// Press Shift+Tab (previous-field navigation) through the real path.
    fn press_shift_tab(&mut self) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Named(NamedKey::Tab), None),
            ModifiersState::SHIFT,
        )
    }

    /// The `Slot` of the currently-focused edit-form field, if any.
    fn focused_slot(&self) -> Option<Slot> {
        self.app
            .form
            .as_ref()
            .and_then(|f| f.inputs.iter().find(|i| i.ti.is_focused()).map(|i| i.slot))
    }

    /// Type a string one keystroke at a time into whichever input is
    /// focused, driving the production `handle_event` → `TextInput` path
    /// (NOT `set_text`), so the live edit semantics are what's exercised.
    fn type_text(&mut self, s: &str) {
        for ch in s.chars() {
            let buf = ch.to_string();
            self.dispatch_key_mods(
                &key_input_event(Key::Character(SmolStr::new(&buf)), Some(&buf)),
                ModifiersState::empty(),
            );
        }
    }

    /// Value currently held in the focused/edit form field for `slot`.
    fn field_value(&self, slot: Slot) -> String {
        self.app
            .form
            .as_ref()
            .and_then(|f| f.inputs.iter().find(|i| i.slot == slot))
            .map(|i| i.ti.value().to_string())
            .unwrap_or_default()
    }

    /// The stored (committed) contact for `id`, read back from the CRDT doc.
    fn doc_contact(&self, id: ContactId) -> Option<Contact> {
        ContactsDoc::list_contacts(self.doc())
            .into_iter()
            .find(|c| c.id == id)
    }

    /// Focus the edit-form field for `slot` by clicking its laid-out rect
    /// through the production press path. Returns whether it was found.
    fn focus_field(&mut self, slot: Slot) -> bool {
        let rect = self
            .app
            .form
            .as_ref()
            .and_then(|f| f.inputs.iter().find(|i| i.slot == slot))
            .map(|i| i.ti.get_rect());
        match rect {
            Some(r) if r.width() > 0.0 => {
                self.press_left(r.center());
                self.app
                    .form
                    .as_ref()
                    .and_then(|f| f.inputs.iter().find(|i| i.slot == slot))
                    .is_some_and(|i| i.ti.is_focused())
            }
            _ => false,
        }
    }
}

/// Detail fields enter edit on double click only, focus scrolls below-the-fold fields into view, and every slot has a placeholder.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn detail_fields_enter_edit_on_double_click_with_placeholders() {
    // --- phase: double_click_detail_field_enters_edit_focused_on_that_field ---
    {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.mode, Mode::View);
        // Find the BIRTHDAY field value rect (every seed contact has one). The
        // website is deliberately excluded — it's a live hyperlink now, not a
        // double-click-to-edit target.
        let (slot, rect) = h
            .app
            .detail_field_hits
            .iter()
            .find(|(s, _)| *s == Slot::Birthday)
            .copied()
            .expect("selected contact shows a birthday field");
        assert_eq!(slot, Slot::Birthday);
        h.double_click(rect.center());
        assert_eq!(h.app.mode, Mode::Edit, "double-click enters edit mode");
        // The birthday input must be the focused one.
        let form = h.app.form.as_ref().unwrap();
        let birthday_focused = form
            .inputs
            .iter()
            .find(|f| f.slot == Slot::Birthday)
            .is_some_and(|f| f.ti.is_focused());
        assert!(
            birthday_focused,
            "the double-clicked field is focused for editing"
        );
    }

    // --- phase: single_click_detail_field_does_not_enter_edit ---
    {
        let mut h = Harness::new(1280, 800);
        let rect = h
            .app
            .detail_field_hits
            .iter()
            .find(|(s, _)| *s == Slot::Birthday)
            .map(|(_, r)| *r)
            .expect("birthday field present");
        h.press_left(rect.center());
        assert_eq!(
            h.app.mode,
            Mode::View,
            "a single click must not enter edit mode"
        );
    }

    // --- phase: keyboard_focus_scrolls_a_below_the_fold_field_into_view ---
    {
        // On a form taller than the pane, moving focus to a field past the fold
        // must scroll it into the visible body — otherwise the focus ring lands
        // on a field the user can't see.
        let mut h = Harness::new(1000, 420); // short pane → the form overflows
        assert!(!h.app.narrow, "1000px is the docked split");
        let mut c = Contact::new();
        c.first = "Long".into();
        c.last = "Form".into();
        for _ in 0..4 {
            c.phones.push(Labeled::new("mobile", ""));
        }
        for _ in 0..4 {
            c.emails.push(Labeled::new("home", ""));
        }
        h.app.begin_edit(c, true);
        h.relayout();

        let rect_of = |h: &Harness, slot: Slot| {
            h.app
                .form
                .as_ref()
                .unwrap()
                .inputs
                .iter()
                .find(|i| i.slot == slot)
                .unwrap()
                .ti
                .get_rect()
        };

        // The last field (Groups) starts well below the visible body.
        let body_bot = h.app.detail_rect.y1;
        assert!(
            rect_of(&h, Slot::Groups).y1 > body_bot,
            "precondition: the last field is off-screen below the fold",
        );

        // Shift+Tab (nothing focused) lands on the last field; it must now be
        // scrolled fully into view.
        assert!(h.press_shift_tab());
        assert_eq!(h.focused_slot(), Some(Slot::Groups));
        h.relayout(); // apply the scroll the focus move requested
        assert!(
            h.app.detail_scroll > 0.0,
            "focusing an off-screen field scrolled the form"
        );
        assert!(
            rect_of(&h, Slot::Groups).y1 <= h.app.detail_rect.y1 + 0.5,
            "the focused last field is now within the visible pane",
        );

        // Tab forward wraps back to the first field and scrolls back to the top.
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.relayout();
        let body_top = h.app.form_body_top();
        assert!(
            rect_of(&h, Slot::First).y0 >= body_top - 24.0,
            "wrapping to the first field scrolled back to the top",
        );
    }

    // --- phase: placeholders_cover_every_form_slot ---
    {
        // Every editable slot must have a non-empty placeholder so the New
        // Contact form never shows a blank, unlabelled field.
        for slot in [
            Slot::First,
            Slot::Last,
            Slot::Org,
            Slot::Title,
            Slot::Website,
            Slot::Birthday,
            Slot::Notes,
            Slot::Groups,
            Slot::Phone(0),
            Slot::Email(0),
            Slot::AddrStreet(0),
            Slot::AddrCity(0),
            Slot::AddrRegion(0),
            Slot::AddrPostal(0),
            Slot::AddrCountry(0),
        ] {
            assert!(
                !placeholder_for(slot).is_empty(),
                "slot is missing a placeholder hint"
            );
        }
    }
}

/// Build an edit-mode harness on a known contact so the field order under
/// Tab is deterministic: First, Last, Org, Title, Phone(0), Email(0),
/// Birthday, Website, Notes, Groups.
fn edit_harness() -> Harness {
    let mut h = Harness::new(1280, 800);
    let mut c = Contact::new();
    c.first = "Ada".into();
    c.last = "Byron".into();
    c.phones.push(Labeled::new("mobile", "+1 555 0100"));
    c.emails.push(Labeled::new("home", "ada@example.com"));
    h.app.begin_edit(c, true);
    h.relayout();
    h
}

/// Tab and Shift-Tab move and wrap focus, Enter advances then submits, and tabbing places the caret at the end.
#[test]
fn form_focus_moves_by_tab_shift_tab_and_enter() {
    // --- phase: tab_moves_focus_forward_across_the_form_fields ---
    {
        let mut h = edit_harness();
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.focused_slot().is_none(), "no field is focused until Tab");
        // Tab lands on the first field, then walks the natural order.
        assert!(
            h.press_named(NamedKey::Tab),
            "Tab is handled in the edit form"
        );
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Org));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Title));
        h.press_named(NamedKey::Tab);
        assert_eq!(
            h.focused_slot(),
            Some(Slot::Phone(0)),
            "repeated phone rows are in the order"
        );
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Email(0)));
    }

    // --- phase: shift_tab_moves_focus_backward_and_wraps ---
    {
        let mut h = edit_harness();
        // Forward twice → Last is focused.
        h.press_named(NamedKey::Tab);
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        // Shift+Tab → back to First.
        assert!(h.press_shift_tab(), "Shift+Tab is handled");
        assert_eq!(h.focused_slot(), Some(Slot::First));
        // Shift+Tab from the first field wraps to the last (Groups).
        h.press_shift_tab();
        assert_eq!(
            h.focused_slot(),
            Some(Slot::Groups),
            "backward from the first wraps to the last"
        );
        // Tab from the last field wraps forward to the first.
        h.press_named(NamedKey::Tab);
        assert_eq!(
            h.focused_slot(),
            Some(Slot::First),
            "forward from the last wraps to the first"
        );
    }

    // --- phase: enter_advances_to_the_next_field_then_submits_on_the_last ---
    {
        let mut h = edit_harness();
        // Enter with nothing focused focuses the first field.
        assert!(
            h.press_named(NamedKey::Enter),
            "Enter is handled in the edit form"
        );
        assert_eq!(h.focused_slot(), Some(Slot::First));
        // Enter advances to the next field.
        h.press_named(NamedKey::Enter);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        // Walk back to the last field: Last → First → (wrap) → Groups. Then
        // Enter on the last field submits (saves) instead of wrapping forward.
        h.press_shift_tab(); // Last → First
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.press_shift_tab(); // First → (wrap) → Groups
        assert_eq!(h.focused_slot(), Some(Slot::Groups));
        h.press_named(NamedKey::Enter);
        assert_eq!(
            h.app.mode,
            Mode::View,
            "Enter on the last field submits the form"
        );
        assert!(h.app.form.is_none(), "the edit form closed on submit");
        assert!(
            h.app
                .store
                .contacts
                .iter()
                .any(|c| c.first == "Ada" && c.last == "Byron"),
            "the edited contact was saved to the store",
        );
    }

    // --- phase: tab_into_a_field_places_the_caret_at_the_end ---
    {
        // Tabbing into a pre-filled field puts the caret after its text so the
        // user can keep typing rather than overwriting.
        let mut h = edit_harness();
        h.press_named(NamedKey::Tab); // First ("Ada")
        let end = h.app.form.as_ref().unwrap().inputs[0].ti.value().len();
        assert_eq!(end, "Ada".len());
        h.type_text("!");
        assert_eq!(
            h.field_value(Slot::First),
            "Ada!",
            "typing appends at the caret"
        );
    }
}

/// The website field renders and opens through the shared link widget.
#[test]
fn website_renders_and_opens_through_the_link_widget() {
    // --- phase: website_renders_via_the_link_widget ---
    {
        // The view-mode website is a real ui `Link` widget (not hand-drawn
        // text): the app holds a Link bound to the selected contact's URL, laid
        // out with a hit rect so it's clickable.
        let h = Harness::new(1280, 800);
        let id = h.app.selected.expect("a contact is selected");
        let website = h.app.store.get(id).unwrap().website.clone();
        assert!(!website.is_empty(), "the selected seed has a website");
        let link = h
            .app
            .website_link
            .as_ref()
            .expect("the website is rendered via a Link widget");
        assert_eq!(link.url(), website, "the link opens the contact's website");
        assert!(
            link.get_rect().width() > 0.0,
            "the link is laid out with a hit rect"
        );
    }

    // --- phase: clicking_the_website_opens_it_through_the_link_widget ---
    {
        // Drive the whole click path (press → hit-test → Link::activate) and
        // confirm the contact's URL is what gets opened. A recording host
        // captures the URL instead of spawning the OS browser, so this exercises
        // the production click path hermetically — no xdg-open side effect.
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        // The link opens through background work; pump it so the open is seen.
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let url = h.app.store.get(id).unwrap().website.clone();
        assert!(!url.is_empty(), "the selected seed has a website");

        h.relayout();
        let rect = h.app.website_link.as_ref().unwrap().get_rect();
        assert!(
            rect.width() > 0.0,
            "the website link has a hit rect after layout"
        );

        assert!(
            h.press_left(rect.center()),
            "clicking the website link is handled"
        );
        work.run_pending();
        assert_eq!(
            recorder.opened_uris(),
            [url],
            "the click opened the contact's website via the Link widget",
        );
        assert_eq!(
            h.app.mode,
            Mode::View,
            "opening a link does not enter edit mode"
        );
    }
}

/// Pointer and keyboard context menus open the selected contacts' canonical actions; favourite toggles, persists and relabels.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn context_menu_opens_canonical_contact_actions() {
    // --- phase: right_click_row_opens_context_menu_then_delete_opens_confirm ---
    {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        assert!(h.press_right(row));
        match &h.app.popup {
            Some(Popup::ContextMenu { target, .. }) => assert_eq!(*target, id),
            other => panic!(
                "right-click should open the context menu, got {:?}",
                other.is_some()
            ),
        }
        // Click the "Delete" row in the menu → the delete-confirm modal opens.
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            unreachable!()
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let delete_idx = CONTEXT_ACTIONS
            .iter()
            .position(|a| *a == ContextAction::Delete)
            .unwrap();
        h.press_left(rows[delete_idx].center());
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "context-menu Delete routes through the confirmation modal"
        );
        assert!(
            h.app.popup.is_none(),
            "the context menu closes when an item is chosen"
        );
    }

    // --- phase: keyboard_context_menu_keys_open_selected_contacts_canonical_actions ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.expect("a contact starts selected");

        assert!(
            h.press_named(NamedKey::ContextMenu),
            "the dedicated Menu key opens the selected contact's actions"
        );
        match &h.app.popup {
            Some(Popup::ContextMenu { target, .. }) => assert_eq!(*target, id),
            _ => panic!("Menu key should open the selected contact's context menu"),
        }

        h.press_named(NamedKey::Escape);
        assert!(h.app.popup.is_none(), "Escape dismisses the transient menu");
        assert!(
            h.press_named_shift(NamedKey::F10),
            "Shift+F10 opens the same context menu on keyboards without Menu"
        );
        match &h.app.popup {
            Some(Popup::ContextMenu { target, .. }) => assert_eq!(*target, id),
            _ => panic!("Shift+F10 should open the selected contact's context menu"),
        }
    }

    // --- phase: context_menu_favorite_toggles_flag_persists_and_relabels ---
    // The context-menu Favorite item flips the contact's flag, persists it to
    // the CRDT, and its label reflects the (now-updated) state on reopen.
    {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);

        // Read the CRDT-backed favorite state (not just the in-memory store).
        let fav_in_doc = |doc: &AutoCommit, id: ContactId| {
            ContactsDoc::list_contacts(doc)
                .into_iter()
                .find(|c| c.id == id)
                .map(|c| c.favorite)
                .expect("contact present in doc")
        };
        let before = h.app.store.get(id).unwrap().favorite;

        // The menu item index is stable; resolve it and confirm its label
        // matches the *current* (pre-toggle) favorite state.
        let fav_idx = CONTEXT_ACTIONS
            .iter()
            .position(|a| *a == ContextAction::Favorite)
            .unwrap();
        assert_eq!(
            CONTEXT_ACTIONS[fav_idx].label(before),
            if before {
                "Remove from favorites"
            } else {
                "Add to favorites"
            },
            "the Favorite item is labelled for the contact's current state",
        );

        // Open the menu and click the Favorite item.
        assert!(h.press_right(row));
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            panic!("right-click should open the context menu");
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        h.press_left(rows[fav_idx].center());

        // The flag flipped in the store AND in the CRDT, and the menu closed.
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            !before,
            "store flag flipped"
        );
        assert_eq!(
            fav_in_doc(h.doc(), id),
            !before,
            "CRDT favorite flag flipped"
        );
        assert!(
            h.app.popup.is_none(),
            "the menu closes after toggling favorite"
        );

        // Reopening the menu now shows the inverted label.
        assert!(h.press_right(h.row_center(0)));
        assert_eq!(
            CONTEXT_ACTIONS[fav_idx].label(!before),
            if before {
                "Add to favorites"
            } else {
                "Remove from favorites"
            },
            "the relabelled item reflects the toggled state",
        );

        // Toggling again restores the original state (round-trip).
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            unreachable!()
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        h.press_left(rows[fav_idx].center());
        assert_eq!(
            fav_in_doc(h.doc(), id),
            before,
            "second toggle restores the CRDT flag"
        );
    }
}

/// The delete key and confirm modal dispatch the palette's delete action; the shared modal's cancel is non-destructive.
#[test]
fn delete_confirmation_dispatches_the_palette_action() {
    // --- phase: confirm_modal_delete_removes_contact_from_doc ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        h.app.confirm_delete = Some(id);
        let before = ContactsDoc::list_contacts(h.doc()).len();
        h.relayout();
        let del = h
            .app
            .delete_dialog
            .as_ref()
            .expect("draw builds the shared modal")
            .dialog
            .primary_rect();
        h.press_left(del.center());
        assert!(
            h.app.confirm_delete.is_none(),
            "the modal closes after confirming"
        );
        let after = ContactsDoc::list_contacts(h.doc()).len();
        assert_eq!(
            after,
            before - 1,
            "Delete removes the contact from the CRDT doc"
        );
        assert!(
            !ContactsDoc::list_contacts(h.doc())
                .iter()
                .any(|c| c.id == id),
            "the deleted contact is gone from the document"
        );
    }

    // --- phase: confirm_delete_dispatches_the_action_the_palette_builds ---
    {
        // Single source of truth, proven on the real button path: confirming
        // a delete dispatches the byte-identical EncodedAction the Ctrl+K
        // "Delete contact" spec builds for the same contact.
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        let expected =
            crate::actions::encode(&crate::actions::delete_contact_action(id.to_string()));
        h.app.confirm_delete = Some(id);
        h.relayout();
        let del = h
            .app
            .delete_dialog
            .as_ref()
            .expect("draw builds the shared modal")
            .dialog
            .primary_rect();
        h.press_left(del.center());
        assert_eq!(
            h.app.last_dispatched(),
            Some(&expected),
            "the confirm-delete button and the palette build the identical EncodedAction",
        );
        assert!(
            !ContactsDoc::list_contacts(h.doc())
                .iter()
                .any(|c| c.id == id),
            "and it actually deleted the contact",
        );
    }

    // --- phase: delete_key_opens_confirm_and_escape_dismisses_it ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        assert!(h.press_named(NamedKey::Delete));
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "Delete raises the confirm modal for the selection"
        );
        h.press_named(NamedKey::Escape);
        assert!(
            h.app.confirm_delete.is_none(),
            "Escape dismisses the confirm modal"
        );
        assert!(h.app.store.get(id).is_some(), "nothing was deleted");
    }

    // --- phase: delete_confirmation_is_shared_modal_and_cancel_is_non_destructive ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        h.app.confirm_delete = Some(id);
        h.relayout();

        let dialog = &h.app.delete_dialog.as_ref().unwrap().dialog;
        let access = dialog.accessibility().expect("modal accessibility node");
        assert_eq!(access.role, ui::a11y::Role::Dialog);
        assert!(access.state.modal, "assistive tech sees a blocking dialog");
        assert!(h.app.traps_focus(), "keyboard focus cannot leave the modal");
        let cancel = dialog.secondary_rect();

        assert!(h.press_left(cancel.center()));
        assert!(h.app.confirm_delete.is_none(), "Cancel closes the dialog");
        assert!(h.app.store.get(id).is_some(), "Cancel never deletes");
        assert!(
            ContactsDoc::list_contacts(h.doc())
                .iter()
                .any(|c| c.id == id),
            "Cancel leaves the document untouched"
        );
    }
}

/// Favourite toggles dispatch the palette action, palette actions reconcile on sync, the detail star toggles store and doc and a toast dismisses.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn favorite_toggles_dispatch_and_reconcile_with_feedback() {
    // --- phase: favorite_toggle_dispatches_the_action_the_palette_builds ---
    {
        // Same, on the favourite path: the context-menu Favorite item dispatches
        // the byte-identical EncodedAction the Ctrl+K "favorite" spec builds,
        // toggling to the negation of the current state.
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        let before = h.app.store.get(id).unwrap().favorite;
        let expected = crate::actions::encode(&crate::actions::set_favorite_action(
            id.to_string(),
            !before,
        ));

        assert!(h.press_right(row));
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            panic!("right-click should open the context menu");
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let fav_idx = CONTEXT_ACTIONS
            .iter()
            .position(|a| *a == ContextAction::Favorite)
            .unwrap();
        h.press_left(rows[fav_idx].center());
        assert_eq!(
            h.app.last_dispatched(),
            Some(&expected),
            "the favourite affordance and the palette build the identical EncodedAction",
        );
    }

    // --- phase: palette_action_reconciles_into_the_list_on_next_sync ---
    {
        // A Ctrl+K palette action mutates only the document; the next sync must
        // reconcile the in-memory store so the change shows in the list — the
        // same way an on-screen edit already does.
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        let before = h.app.store.len();
        // Dispatch a delete straight into the doc through the executor, bypassing
        // the app's own affordances (exactly as the shell's Ctrl+K palette would).
        let action = crate::actions::delete_contact_action(id.to_string());
        let encoded = crate::actions::encode(&action);
        let mut tx = tilo_actions::LocalTransport::new();
        tilo_actions::dispatch(h.parts().1, &action, encoded, &mut tx).unwrap();
        // The store hasn't seen it yet — it still holds the contact.
        assert!(
            h.app.store.get(id).is_some(),
            "store is stale before the reconciling sync"
        );
        let (app, doc) = h.parts();
        app.sync(doc, &ROOT);
        assert!(
            h.app.store.get(id).is_none(),
            "the reconciling sync dropped the deleted contact"
        );
        assert_eq!(h.app.store.len(), before - 1);
    }

    // --- phase: detail_favorite_star_click_toggles_store_and_doc ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let before = h.app.store.get(id).unwrap().favorite;
        assert!(
            h.app.hit.fav.width() > 0.0,
            "the detail favorite star is laid out"
        );
        h.press_left(h.app.hit.fav.center());
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            !before,
            "the star flips the store flag"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().favorite,
            !before,
            "and persists to the doc"
        );
    }

    // --- phase: toast_auto_dismisses_after_a_real_action ---
    {
        // After Save/Delete/Import/Export the confirmation toast used to stay
        // forever: it stored an Instant nothing ever read. Drive a real action
        // (delete) so the production `show_toast` fires, then prove the frame
        // loop keeps itself awake and clears the toast once its lifetime
        // elapses, and idles afterwards.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let id = app.selected.unwrap();

        app.delete_selected(&mut doc, id);
        assert!(app.toast.is_some(), "deleting a contact raises a toast");
        assert!(
            app.needs_animation_frame(),
            "the loop stays awake so the timed dismissal fires without further input",
        );

        // A frame drawn before the lifetime elapses keeps the toast.
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.toast.is_some(),
            "a fresh toast survives an immediate frame"
        );

        // Back-date the toast past its lifetime and draw again → it clears,
        // and the loop is then free to idle.
        if let Some((_, shown)) = app.toast.as_mut() {
            *shown = ui::time::now()
                .checked_sub(TOAST_LIFETIME)
                .unwrap()
                .checked_sub(Duration::from_millis(50))
                .unwrap();
        }
        assert!(
            app.toast_expired(),
            "the back-dated toast is past its lifetime"
        );
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.toast.is_none(),
            "the toast auto-dismisses once its lifetime elapses"
        );
        assert!(
            !app.needs_animation_frame(),
            "with the toast gone the loop can settle back to idle",
        );
    }
}

/// Switching away from a dirty edit pops the save modal (save commits, discard switches), clean edits switch directly, and keystroke dirtying reaches the guard.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn switching_while_editing_is_guarded_by_the_unsaved_modal() {
    // --- phase: switching_while_editing_dirty_pops_save_modal_then_save_commits ---
    {
        let mut h = Harness::new(1280, 800);
        // Enter edit on the selected contact and dirty a field.
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_mut()
            && let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title)
        {
            fi.ti.set_text("Brand New Title");
        }
        h.relayout();
        assert!(h.app.form_is_dirty(), "the edited form is dirty");
        // Click a different list row → the guard modal pops, no switch yet.
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert_eq!(
            h.app.pending_switch,
            Some(other_id),
            "switch is deferred behind the modal"
        );
        assert_eq!(
            h.app.mode,
            Mode::Edit,
            "still editing while the modal is up"
        );
        // Press Save → commits the edit AND switches to the target.
        let g = h.app.unsaved_geometry();
        h.press_left(g.save.center());
        assert!(h.app.pending_switch.is_none());
        assert_eq!(h.app.mode, Mode::View, "saved and left the form");
        assert_eq!(
            h.app.selected,
            Some(other_id),
            "switched to the clicked contact"
        );
        let stored = ContactsDoc::list_contacts(h.doc());
        let edited = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(
            edited.title, "Brand New Title",
            "the edit was saved to the doc"
        );
    }

    // --- phase: switching_while_editing_discard_drops_edits_and_switches ---
    {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let original_title = h.app.store.get(cur).unwrap().title.clone();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_mut()
            && let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title)
        {
            fi.ti.set_text("Throwaway Title");
        }
        h.relayout();
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert_eq!(h.app.pending_switch, Some(other_id));
        // Press Discard → drop edits, switch.
        let g = h.app.unsaved_geometry();
        h.press_left(g.discard.center());
        assert_eq!(h.app.selected, Some(other_id));
        let stored = ContactsDoc::list_contacts(h.doc());
        let unchanged = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(
            unchanged.title, original_title,
            "discard left the doc unchanged"
        );
    }

    // --- phase: switching_with_no_changes_switches_directly_no_modal ---
    {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(!h.app.form_is_dirty(), "no edits ⇒ not dirty");
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert!(
            h.app.pending_switch.is_none(),
            "no modal when there are no unsaved changes"
        );
        assert_eq!(h.app.selected, Some(other_id), "switched directly");
        assert_eq!(h.app.mode, Mode::View);
    }

    // --- phase: dirtying_via_keystrokes_makes_the_unsaved_guard_reachable ---
    {
        // Proves the guard on the REAL keystroke path (not `set_text`): typing
        // leaves the store untouched, so `form_is_dirty` sees the divergence
        // and switching contacts pops the modal instead of losing the edit.
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(h.focus_field(Slot::Title));
        h.type_text("Q");
        assert!(
            h.app.form_is_dirty(),
            "keystroke-dirtied form is dirty via the production path"
        );
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        h.press_left(h.row_center(other_idx));
        assert_eq!(
            h.app.pending_switch,
            Some(other_id),
            "the guard defers the switch"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing behind the modal");
    }
}

// =================================================================
// Live-edit draft semantics — the structural fix. Editing mutates only
// the form's local buffers/draft; nothing reaches the store or the CRDT
// doc until Save. This is what makes Cancel real and the unsaved guard
// reachable, so these drive the *real* keystroke path (handle_event),
// never `set_text`.
// =================================================================

/// Typing does not commit, cancel discards (no phantom new record), the edit affordance opens and returns, and Ctrl-S commits through the keystroke path.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn edits_commit_only_on_save_and_cancel_discards() {
    // --- phase: typing_in_edit_form_does_not_commit_and_cancel_discards ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let original = h.app.store.get(id).unwrap().title.clone();
        let c = h.app.store.get(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        // Focus the Title field via a real click, then type through the
        // production keystroke path (NOT `set_text`).
        assert!(
            h.focus_field(Slot::Title),
            "the title field focuses on click"
        );
        h.type_text("XYZ");
        // The keystrokes landed in the field's local buffer …
        let live = h.field_value(Slot::Title);
        assert!(
            live.contains("XYZ"),
            "the field shows the typed text: {live:?}"
        );
        assert_ne!(live, original);
        // … but NOTHING was committed: editing is a true draft now.
        assert_eq!(
            h.app.store.get(id).unwrap().title,
            original,
            "store untouched mid-edit"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "CRDT doc untouched mid-edit"
        );
        assert!(h.app.form_is_dirty(), "the live-typed form reports dirty");
        // Cancel genuinely discards the draft.
        assert!(
            h.app.hit.cancel.width() > 0.0,
            "the Cancel button is laid out"
        );
        h.press_left(h.app.hit.cancel.center());
        assert_eq!(h.app.mode, Mode::View, "Cancel leaves edit mode");
        assert!(h.app.form.is_none(), "Cancel drops the form");
        assert_eq!(
            h.app.store.get(id).unwrap().title,
            original,
            "Cancel discarded the edit (store)"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "Cancel discarded the edit (doc)"
        );
    }

    // --- phase: cancel_on_new_contact_leaves_no_phantom_record ---
    {
        let mut h = Harness::new(1280, 800);
        let store_before = h.app.store.len();
        let doc_before = ContactsDoc::list_contacts(h.doc()).len();
        // Start a new contact via the keyboard shortcut, focus the name, type.
        assert!(h.press_char("n"), "'n' starts a new contact");
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.app.form.as_ref().unwrap().is_new);
        h.relayout();
        assert!(h.focus_field(Slot::First));
        h.type_text("Ghost");
        // The half-typed new contact must not exist anywhere yet.
        assert_eq!(
            h.app.store.len(),
            store_before,
            "a half-typed new contact isn't in the store"
        );
        assert_eq!(
            ContactsDoc::list_contacts(h.doc()).len(),
            doc_before,
            "nor in the doc",
        );
        // Cancel → still nothing, no "No Name"/"Ghost" phantom left behind.
        h.press_left(h.app.hit.cancel.center());
        assert_eq!(h.app.mode, Mode::View);
        assert_eq!(
            h.app.store.len(),
            store_before,
            "Cancel on New leaves no phantom record"
        );
        assert!(
            !h.app
                .store
                .contacts
                .iter()
                .any(|c| c.display_name() == "Ghost"),
            "the abandoned draft never entered the book",
        );
    }

    // --- phase: edit_affordance_opens_form_on_selected_contact_and_cancel_returns ---
    // The read-view Edit affordance is a plain click that opens the edit form
    // on the *same* contact, and Cancel returns tidily to the read view — the
    // clean view→edit→view round trip, driven entirely through the production
    // click path (`handle_event`), not by poking `mode`/`begin_edit` directly.
    {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.mode, Mode::View, "the app opens in the read view");
        let sel = h
            .app
            .selected
            .expect("a contact is selected in the read view");
        assert!(
            h.app.hit.edit.width() > 0.0,
            "the read-view Edit button is laid out"
        );

        // Clicking Edit enters the form on the shown contact (identity carried
        // across the transition — the header's avatar + name reflect this draft).
        h.press_left(h.app.hit.edit.center());
        assert_eq!(h.app.mode, Mode::Edit, "clicking Edit opens the edit form");
        {
            let form = h.app.form.as_ref().expect("an edit form is open");
            assert_eq!(
                form.draft.id, sel,
                "the form edits the same contact the view showed"
            );
            assert!(!form.is_new, "editing an existing record, not creating one");
        }

        // Leaving is just as direct: Cancel returns to the read view, no form.
        h.relayout();
        assert!(
            h.app.hit.cancel.width() > 0.0,
            "the Cancel button is laid out in the form"
        );
        h.press_left(h.app.hit.cancel.center());
        assert_eq!(h.app.mode, Mode::View, "Cancel returns to the read view");
        assert!(h.app.form.is_none(), "no edit form lingers after Cancel");
        assert_eq!(h.app.selected, Some(sel), "the same contact stays selected");
    }

    // --- phase: typed_edit_saved_with_ctrl_s_commits_through_the_keystroke_path ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(h.focus_field(Slot::Org));
        h.type_text("Z");
        let typed = h.field_value(Slot::Org);
        assert!(typed.contains('Z'));
        // Ctrl+S saves; the typed value now round-trips into store + doc.
        assert!(h.press_char_ctrl("s"), "Ctrl+S is handled in edit mode");
        assert_eq!(h.app.mode, Mode::View, "Ctrl+S leaves the form");
        assert_eq!(
            h.app.store.get(id).unwrap().org,
            typed,
            "the typed org saved to the store"
        );
        assert_eq!(h.doc_contact(id).unwrap().org, typed, "and to the CRDT doc");
    }
}

/// Bare keys never discard an open form and a new form is not dirty until a field is filled.
#[test]
fn open_edit_forms_ignore_bare_keys_and_start_clean() {
    // --- phase: bare_keys_do_not_discard_an_open_edit_form ---
    {
        // Regression: with the edit form open but no field yet focused (its
        // initial state right after opening), a stray 'n' used to start a
        // brand-new contact — silently discarding the in-progress edit with no
        // unsaved guard — and '/' yanked focus to the list search. The form's
        // own keys own the keyboard until a field is focused.
        let mut h = edit_harness();
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.app.form.as_ref().unwrap().is_new);
        assert!(
            h.focused_slot().is_none(),
            "the form opens with no field focused"
        );
        let draft_id = h.app.form.as_ref().unwrap().draft.id;

        // 'n' must NOT start a new contact / swap the draft out from under us.
        h.press_char("n");
        assert_eq!(h.app.mode, Mode::Edit, "'n' does not leave the edit form");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.id,
            draft_id,
            "'n' did not replace the in-progress draft",
        );

        // '/' must NOT steal focus to the list search while editing.
        h.press_char("/");
        assert!(
            !h.app.search.is_focused(),
            "'/' does not steal focus mid-edit"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing");

        // Once a field IS focused, the very same key types into it normally.
        assert!(h.focus_field(Slot::First));
        h.type_text("n");
        assert!(
            h.field_value(Slot::First).contains('n'),
            "with a field focused the key types into it: {:?}",
            h.field_value(Slot::First),
        );
    }

    // --- phase: new_contact_form_is_not_dirty_until_a_field_is_filled ---
    {
        // A freshly-opened New Contact form (with the two empty seeded
        // phone/email rows) must not be considered dirty — otherwise the
        // unsaved guard would fire on the very first contact switch.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        app.shots_new_contact();
        assert!(
            !app.form_is_dirty(),
            "an untouched New Contact form is clean"
        );
        if let Some(form) = app.form.as_mut() {
            form.draft.first = "Jane".into();
            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::First) {
                fi.ti.set_text("Jane");
            }
        }
        assert!(app.form_is_dirty(), "typing a name makes the form dirty");
    }
}

// =================================================================
// List controls: search, sort, group filter, keyboard nav, scroll.
// =================================================================

/// Focus chords and Ctrl-K focus the search, typing narrows the list and an empty result offers a clear action.
#[test]
fn search_field_focus_and_typing_narrow_the_list() {
    // --- phase: typing_in_search_narrows_the_visible_list ---
    {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.visible_ids().len(), 7);
        // Focus the search field with '/', then type a query via keystrokes.
        assert!(h.press_char("/"), "'/' focuses search");
        assert!(h.app.search.is_focused());
        h.type_text("hopper");
        assert_eq!(h.app.search.value(), "hopper");
        let visible = h.app.visible_ids();
        assert_eq!(visible.len(), 1, "only Grace Hopper matches 'hopper'");
        assert_eq!(h.app.store.get(visible[0]).unwrap().last, "Hopper");
        // The rendered rows reflect it after a layout pass.
        h.relayout();
        assert_eq!(
            h.app.list_rows.len(),
            1,
            "the list pane renders the single match"
        );
    }

    // --- phase: focus_search_chord_predicate_matches_ctrl_k_and_ctrl_slash ---
    {
        assert!(is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("k"))
        ));
        assert!(is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("K"))
        ));
        assert!(is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("/"))
        ));
        // Not the chord: same keys without ctrl, or other ctrl chords.
        assert!(!is_focus_search_chord(
            false,
            &Key::Character(SmolStr::new("k"))
        ));
        assert!(!is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("s"))
        ));
        assert!(!is_focus_search_chord(true, &Key::Named(NamedKey::Escape)));
    }

    // --- phase: ctrl_k_focuses_the_search_field_through_handle_event ---
    {
        // The chord used to be a dead seam that did nothing; it now performs a
        // real, in-app action — focusing the list search box — driven all the
        // way through the production `handle_event` path.
        let mut h = Harness::new(1280, 800);
        assert!(!h.app.search.is_focused(), "search starts unfocused");
        assert!(h.press_char_ctrl("k"), "Ctrl+K is handled by the app");
        assert!(h.app.search.is_focused(), "Ctrl+K focuses the search field");
        // Escape releases it, and Ctrl+/ focuses it again.
        h.press_named(NamedKey::Escape);
        assert!(!h.app.search.is_focused());
        assert!(h.press_char_ctrl("/"), "Ctrl+/ is handled too");
        assert!(
            h.app.search.is_focused(),
            "Ctrl+/ also focuses the search field"
        );
    }

    // --- phase: empty_filtered_list_offers_a_keyboard_and_semantic_clear_action ---
    {
        let mut h = Harness::new(1_280, 800);
        h.app.search.set_text("definitely no contact");
        h.relayout();
        assert_eq!(h.app.visible_row_count, 0);
        assert_eq!(h.app.empty_list_action, EmptyListAction::ClearFilters);

        let tree = h.app.accessibility().expect("an empty list is semantic");
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("contacts list is exposed");
        assert!(list.children.iter().any(|child| {
            child.action_target.as_deref() == Some("contacts.empty.clear-filters")
        }));

        h.press_named(NamedKey::Tab);
        assert!(
            h.app.empty_action_focused,
            "Tab reaches the empty-list action"
        );
        h.press_named(NamedKey::Enter);
        assert!(h.app.search.value().is_empty());
        assert!(h.app.group_filter.is_none());
        h.relayout();
        assert!(
            h.app.visible_row_count > 0,
            "clearing restores the address book"
        );
    }
}

/// The sort chip reorders, the group filter popup filters keeping selection valid, and the filter shot defers its anchor.
#[test]
fn sort_and_group_filters_reorder_and_filter_the_list() {
    // --- phase: sort_chip_opens_selector_then_reorders_on_choice ---
    {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.sort, SortKey::Name);
        let first_by_name = h
            .app
            .store
            .get(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_eq!(
            first_by_name, "Tim Berners-Lee",
            "name sort leads with Berners-Lee"
        );
        assert!(
            h.press_left(h.app.hit.sort.center()),
            "the sort chip is clickable"
        );
        let Some(Popup::Sort { anchor }) = h.app.popup else {
            panic!("the sort chip opens its explicit selector");
        };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, 2, 180.0);
        h.press_left(rows[1].center());
        assert_eq!(h.app.sort, SortKey::Org, "choosing Org changes the sort");
        let first_by_org = h
            .app
            .store
            .get(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_ne!(first_by_org, first_by_name, "the list reordered");
        assert_eq!(
            first_by_org, "Ada Lovelace",
            "org sort leads with Analytical Engines"
        );
        // Choosing Name returns to the name order.
        h.press_left(h.app.hit.sort.center());
        let Some(Popup::Sort { anchor }) = h.app.popup else {
            panic!("the sort selector opens again");
        };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, 2, 180.0);
        h.press_left(rows[0].center());
        assert_eq!(h.app.sort, SortKey::Name);
    }

    // --- phase: group_filter_popup_selection_filters_and_keeps_selection_valid ---
    {
        let mut h = Harness::new(1280, 800);
        // The seeded selection (Ada) is not in "Friends".
        let sel_before = h.app.selected.unwrap();
        assert!(
            !h.app
                .store
                .get(sel_before)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.value.eq_ignore_ascii_case("Friends")),
        );
        // Open the group filter popup and choose "Friends".
        assert!(h.press_left(h.app.hit.group.center()));
        let Some(Popup::GroupFilter { anchor }) = h.app.popup else {
            panic!("the group chip opens the filter popup");
        };
        let opts = h.app.group_filter_options();
        let idx = opts
            .iter()
            .position(|o| o == "Friends")
            .expect("Friends is a seeded group");
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, opts.len(), 180.0);
        h.press_left(rows[idx].center());
        assert_eq!(
            h.app.group_filter.as_deref(),
            Some("Friends"),
            "the filter is applied"
        );
        let visible = h.app.visible_ids();
        assert!(!visible.is_empty());
        assert!(
            visible.iter().all(|id| h
                .app
                .store
                .get(*id)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.value.eq_ignore_ascii_case("Friends"))),
            "every visible contact is in Friends",
        );
        // Selection stayed valid: it now points at a still-visible contact.
        assert!(
            visible.contains(&h.app.selected.unwrap()),
            "selection moved to a visible contact"
        );
    }

    // --- phase: group_filter_shot_defers_its_anchor_until_layout_exists ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        app.shots_show_group_filter();
        drive_frame(&mut app, 1_280, 800);
        assert!(matches!(
            app.popup,
            Some(Popup::GroupFilter { anchor }) if anchor == app.hit.group && anchor != Rect::ZERO
        ));
    }
}

/// Arrow, n and e keys drive the list; the wheel scrolls it and large books virtualise while keeping selection in view.
#[test]
fn list_keyboard_and_scroll_navigation() {
    // --- phase: arrow_keys_move_the_list_selection ---
    {
        let mut h = Harness::new(1280, 800);
        let ids = h.app.visible_ids();
        let pos = ids
            .iter()
            .position(|x| *x == h.app.selected.unwrap())
            .unwrap();
        assert!(
            pos + 1 < ids.len(),
            "the seeded selection isn't the last row"
        );
        assert!(h.press_named(NamedKey::ArrowDown));
        assert_eq!(
            h.app.selected,
            Some(ids[pos + 1]),
            "ArrowDown selects the next contact"
        );
        assert!(h.press_named(NamedKey::ArrowUp));
        assert_eq!(
            h.app.selected,
            Some(ids[pos]),
            "ArrowUp returns to the previous"
        );
    }

    // --- phase: n_key_opens_new_and_e_key_edits_selection ---
    {
        let mut h = Harness::new(1280, 800);
        assert!(h.press_char("n"));
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(
            h.app.form.as_ref().unwrap().is_new,
            "'n' opens a NEW contact form"
        );
        // Escape out, then 'e' edits the existing selection.
        h.press_named(NamedKey::Escape);
        assert_eq!(h.app.mode, Mode::View);
        assert!(h.press_char("e"));
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(
            !h.app.form.as_ref().unwrap().is_new,
            "'e' edits the selected contact"
        );
    }

    // --- phase: mouse_wheel_scrolls_the_overflowing_contact_list ---
    {
        // A short, wide window overflows the list so scrolling has an effect.
        let mut h = Harness::new(1000, 320);
        assert!(!h.app.narrow, "1000px stays the docked split");
        assert_eq!(h.app.list_scroll, 0.0);
        h.move_to(h.app.list_rect.center());
        h.dispatch(&WindowEvent::MouseWheel {
            device_id: device_id(),
            delta: MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        });
        h.relayout();
        assert!(
            h.app.list_scroll > 0.0,
            "scrolling down moves the list; clamp keeps it in range"
        );
    }

    // --- phase: large_books_virtualize_rows_and_keep_keyboard_selection_in_view ---
    {
        // The CRDT/search model may legitimately hold an imported company
        // directory.  Layout and accessibility must scale with the viewport,
        // not with the directory's total cardinality.
        let mut h = Harness::new(900, 640);
        h.app.store.contacts = (0..1_000)
            .map(|index| Contact {
                id: uuid::Uuid::new_v4(),
                first: format!("Directory {index:04}"),
                ..Contact::default()
            })
            .collect();
        h.app.selected = h.app.store.contacts.first().map(|contact| contact.id);
        h.relayout();

        let viewport_capacity = (h.app.list_body.height() / ROW_H).ceil() as usize;
        assert_eq!(h.app.visible_row_count, 1_000);
        assert!(
            h.app.list_rows.len() <= viewport_capacity + LIST_OVERSCAN_ROWS * 2,
            "only viewport rows and bounded overscan are materialized"
        );

        h.app.move_selection(40);
        h.relayout();
        let selected = h.app.selected.expect("selection remains available");
        assert!(
            h.app.list_rows.iter().any(|(id, _)| *id == selected),
            "keyboard selection scrolls itself into the virtualized viewport"
        );

        let tree = h.app.accessibility().expect("semantic tree is present");
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("contacts list is exposed");
        assert_eq!(list.children.len(), h.app.list_rows.len());
        assert!(
            list.children.len() < h.app.visible_row_count,
            "accessibility mirrors the bounded viewport rather than allocating every row"
        );
        let selected_row = list
            .children
            .iter()
            .find(|node| node.state.selected)
            .expect("selected row is in the semantic viewport");
        assert_eq!(selected_row.pos_in_set, Some(41));
        assert_eq!(selected_row.set_size, Some(1_000));
    }
}

/// The divider resizes the split and the compact toolbar fits with tooltips at phone width.
#[test]
fn split_divider_and_compact_toolbar_fit_their_space() {
    // --- phase: dragging_the_divider_resizes_the_split ---
    {
        let mut h = Harness::new(1280, 800);
        let before = h.app.split_pos;
        // Grab the divider (between list and detail).
        let grab = Point::new(
            h.app.list_rect.x1 + DIVIDER_W / 2.0,
            h.app.detail_rect.center().y,
        );
        h.move_to(grab);
        assert!(h.app.divider_hover, "cursor over the divider arms the drag");
        h.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        assert!(h.app.divider_drag, "pressing on the divider starts a drag");
        // Drag to ~55% of the width.
        h.move_to(Point::new(
            h.app.rect.x0 + h.app.rect.width() * 0.55,
            grab.y,
        ));
        assert!(
            h.app.split_pos > before,
            "dragging right widens the list pane"
        );
        assert!(
            (h.app.split_pos - 0.55).abs() < 0.02,
            "the split follows the cursor"
        );
        h.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        assert!(!h.app.divider_drag, "releasing ends the drag");
    }

    // --- phase: compact_toolbar_has_room_and_tooltips_at_phone_width ---
    {
        let mut h = Harness::new(320, 640);
        assert!(h.app.toolbar_is_compact());
        assert!(h.app.hit.import.x0 >= h.app.rect.x0 + PAD - 0.01);
        assert!(h.app.hit.import.x1 < h.app.hit.export.x0);
        assert!(h.app.hit.export.x1 < h.app.hit.new.x0);
        h.app.last_mouse = h.app.hit.import.center();
        assert_eq!(Widget::tooltip(&*h.app), Some("Import contacts"));
        h.app.last_mouse = h.app.hit.new.center();
        assert_eq!(Widget::tooltip(&*h.app), Some("Create contact"));
    }
}

// =================================================================
// Edit-form controls: add/remove field, type chip, calendar, favorites.
// =================================================================

/// Phone rows grow and shrink, repeated row ids survive edits, and the favourite pill toggles only the draft.
#[test]
fn repeated_rows_grow_shrink_and_keep_ids_in_the_draft() {
    // --- phase: add_and_remove_phone_buttons_grow_and_shrink_the_draft ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let base = c.phones.len();
        h.app.begin_edit(c, false);
        h.relayout();
        // Add-phone button appends a blank row.
        assert!(
            h.app.hit.add_phone.width() > 0.0,
            "the add-phone button is laid out"
        );
        h.press_left(h.app.hit.add_phone.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones.len(),
            base + 1,
            "add-phone grew the draft"
        );
        h.relayout();
        // Remove-field button on the new phone drops it again.
        let rm = h
            .app
            .hit
            .remove_btns
            .iter()
            .find(|(t, _)| matches!(t, TypeTarget::Phone(i) if *i == base))
            .map(|(_, r)| *r)
            .expect("the new phone row has a remove button");
        h.press_left(rm.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones.len(),
            base,
            "remove-field shrank the draft"
        );
    }

    // --- phase: form_collect_preserves_repeated_row_ids_across_value_and_group_order_edits ---
    {
        let contact = sample_contacts().into_iter().next().unwrap();
        let phone_id = contact.phones[0].id;
        let first_group_id = contact.groups[0].id;
        let second_group_id = contact.groups[1].id;
        let mut form = EditForm::new(contact, false);
        for input in &mut form.inputs {
            match input.slot {
                Slot::Phone(0) => input.ti.set_text("+44 20 7946 0999"),
                Slot::Groups => input.ti.set_text("Favorites, Work"),
                _ => {}
            }
        }
        form.collect();
        assert_eq!(form.draft.phones[0].id, phone_id);
        assert_eq!(form.draft.phones[0].value, "+44 20 7946 0999");
        assert_eq!(form.draft.groups[0].id, second_group_id);
        assert_eq!(form.draft.groups[1].id, first_group_id);
    }

    // --- phase: edit_form_favorite_pill_click_toggles_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let before = c.favorite;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            h.app.hit.fav_toggle.width() > 0.0,
            "the favorite pill is laid out in the form"
        );
        h.press_left(h.app.hit.fav_toggle.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.favorite,
            !before,
            "the pill flips the draft flag"
        );
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            before,
            "draft-only: store unchanged until Save"
        );
    }
}

/// Type chip and calendar popups change only the draft and popup keyboard selection matches the pointer.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn draft_popups_set_type_and_birthday_only_in_the_draft() {
    // --- phase: type_chip_menu_selection_sets_label_in_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        assert!(!c.phones.is_empty());
        let stored_label = c.phones[0].label.clone();
        h.app.begin_edit(c, false);
        h.relayout();
        // Click the type chip for the first phone.
        let chip = h
            .app
            .hit
            .type_chips
            .iter()
            .find(|(t, _)| matches!(t, TypeTarget::Phone(0)))
            .map(|(_, r)| *r)
            .expect("phone 0 has a type chip");
        h.press_left(chip.center());
        let Some(Popup::TypeMenu { anchor, .. }) = h.app.popup else {
            panic!("the type chip opens the type menu");
        };
        // Pick a label different from the current one.
        let pick = TYPE_OPTIONS
            .iter()
            .position(|l| *l != stored_label)
            .unwrap();
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, TYPE_OPTIONS.len(), 140.0);
        h.press_left(rows[pick].center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones[0].label,
            TYPE_OPTIONS[pick],
            "the draft label updated",
        );
        // Draft-only: the committed store still holds the old label.
        assert_eq!(
            h.app.store.get(id).unwrap().phones[0].label,
            stored_label,
            "the label isn't committed until Save",
        );
    }

    // --- phase: calendar_day_click_sets_birthday_in_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let stored_bday = c.birthday;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            h.app.hit.cal_btn.width() > 0.0,
            "the calendar button is laid out"
        );
        h.press_left(h.app.hit.cal_btn.center());
        let Some(Popup::Calendar {
            year,
            month,
            anchor,
        }) = h.app.popup
        else {
            panic!("the calendar button opens the calendar");
        };
        let (panel, _prev, _next, cells) = h.app.calendar_geometry(anchor, year, month);
        // Pick an in-panel day different from the stored one (10).
        let (day, rect) = cells
            .iter()
            .copied()
            .find(|(d, r)| *d != 10 && panel.contains(r.center()))
            .expect("a selectable in-panel day");
        h.press_left(rect.center());
        let expect = format!("{year:04}-{month:02}-{day:02}");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.birthday,
            Some(Birthday::Full { year, month, day }),
            "the picked day set the draft birthday as a structured date",
        );
        assert_eq!(
            h.field_value(Slot::Birthday),
            expect,
            "and mirrored into the birthday field"
        );
        // Draft-only: not committed to the store yet.
        assert_eq!(
            h.app.store.get(id).unwrap().birthday,
            stored_bday,
            "birthday not committed until Save"
        );
    }

    // --- phase: popup_keyboard_selects_the_same_type_choice_as_a_pointer ---
    {
        let mut h = Harness::new(1_280, 800);
        let id = h.app.selected.expect("sample contact selected");
        let contact = h.app.store.get(id).cloned().expect("stored contact");
        h.app.begin_edit(contact, false);
        h.relayout();
        let chip = h
            .app
            .hit
            .type_chips
            .iter()
            .find(|(target, _)| matches!(target, TypeTarget::Phone(0)))
            .map(|(_, rect)| *rect)
            .expect("phone type chip is visible");
        h.press_left(chip.center());
        assert!(matches!(h.app.popup, Some(Popup::TypeMenu { .. })));
        assert_eq!(h.app.popup_focus, 0, "first menu row receives focus");
        h.press_named(NamedKey::ArrowDown);
        h.press_named(NamedKey::Enter);
        assert!(h.app.popup.is_none(), "activation dismisses the menu");
        assert_eq!(
            h.app.form.as_ref().expect("still editing").draft.phones[0].label,
            TYPE_OPTIONS[1],
            "keyboard activation uses the same draft update as a click"
        );
    }
}

/// A unique scratch `.vcf` destination for an export test — the path the
/// portal Save dialog would return, driven directly since the picker can't
/// run headless.
fn scratch_export() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "contacts-export-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let vcf = dir.join("out.vcf");
    (dir, vcf)
}

/// Export parks the selection, writes the VCF to the portal path and round-trips every contact.
#[test]
#[serial_test::serial]
fn vcf_export_writes_the_selected_book() {
    // --- phase: export_selection_parks_then_save_portal_path_writes_the_vcf ---
    {
        let (dir, vcf) = scratch_export();
        let mut h = Harness::new(1280, 800);
        // Open the export dialog via the toolbar Export button.
        h.press_left(h.app.hit.export.center());
        assert!(
            h.app.export_dialog.is_open(),
            "Export opens the selection dialog"
        );
        // Pre-ticked with all visible contacts.
        let total = h.app.store.filtered_sorted("", None, h.app.sort).len();
        assert_eq!(
            h.app.export_dialog.selection().len(),
            total,
            "all pre-selected"
        );
        // Select none, then tick exactly one row.
        let g = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort);
        h.press_left(g.select_none.center());
        let g = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort);
        let (first_id, _row, cb) = g.rows[0];
        h.press_left(cb.center());
        assert_eq!(h.app.export_dialog.selection().len(), 1);
        assert!(h.app.export_dialog.selection().contains(&first_id));

        // Export 3.0 → closes the dialog and parks the selection for the Save
        // dialog. It must NOT write anything before the user chooses a path.
        // A recording host stands in for the desktop and will answer the Save
        // dialog with `vcf`.
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ExportOutcome::Opened(vcf.clone()));
        let _host = ui::host::enter(&services);
        let g = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort);
        h.press_left(g.export_v3.center());
        assert!(
            !h.app.export_dialog.is_open(),
            "choosing a version closes the dialog"
        );
        {
            let (ids, version) = h.app.pending_export.as_ref().expect("selection is parked");
            assert_eq!(ids, &vec![first_id], "the one ticked contact is parked");
            assert_eq!(*version, VCardVersion::V3, "the chosen version is parked");
        }
        assert!(h.app.portal_in_flight(), "the Save dialog is in flight");
        assert!(
            !vcf.exists(),
            "nothing is written until a destination is chosen"
        );

        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1, "exactly one Save dialog was requested");
        assert_eq!(picks[0].title(), "Export contacts");

        // Drain the destination the Save dialog returned through one real
        // frame's sync — which writes the file.
        let (app, doc) = h.parts();
        app.sync(doc, &ROOT);
        assert!(
            h.app.pending_export.is_none(),
            "the parked selection was consumed"
        );
        assert!(!h.app.portal_in_flight(), "draining clears the request");
        let body = std::fs::read_to_string(&vcf).expect("export wrote a vcf to the chosen path");
        assert_eq!(
            body.matches("BEGIN:VCARD").count(),
            1,
            "exactly the ticked contact"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: write_export_round_trips_every_contact ---
    {
        // The hermetic write core: hand it every visible id + a version and it
        // writes a matching .vcf to the given path.
        let (dir, vcf) = scratch_export();
        let mut h = Harness::new(1280, 800);
        let ids = h.app.store.filtered_sorted("", None, h.app.sort);
        let total = ids.len();
        h.app.write_export(&ids, VCardVersion::V4, &vcf);
        let body = std::fs::read_to_string(&vcf).unwrap();
        assert_eq!(
            body.matches("BEGIN:VCARD").count(),
            total,
            "every contact written"
        );
        assert!(body.contains("VERSION:4.0"), "Export 4.0 emits v4 cards");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// =================================================================
// Real persistence + first-launch behaviour
// =================================================================

fn scratch_book() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "contacts-app-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("addressbook.automerge");
    (dir, path)
}

/// Run the live Import action under a recording host whose multi-select
/// chooser answers `outcome`, and check the app asked for that chooser.
fn import_with(app: &mut ContactsApp, outcome: ImportOutcome) {
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick_multiple(outcome);
    ui::host::scoped(&services, || app.request_import());
    let picks = recorder.pick_requests();
    assert_eq!(picks.len(), 1, "one file chooser was requested");
    assert_eq!(picks[0].title(), "Import contacts");
    assert!(picks[0].is_multiple(), "Import picks several files at once");
}

/// A live launch with no saved book is empty, and the first-run state is actionable by pointer, keyboard and assistive tech.
#[test]
fn first_run_is_empty_and_actionable() {
    // --- phase: live_launch_with_no_saved_book_is_empty_not_fabricated ---
    {
        // The live app must NOT invent sample contacts on an empty store.
        let (dir, _path) = scratch_book();
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        assert!(
            app.store.is_empty(),
            "a fresh disk-backed launch shows an empty book"
        );
        assert!(
            app.selected.is_none(),
            "nothing is selected when the book is empty"
        );
        // And a full frame over the empty state must not panic.
        drive_frame(&mut app, 1280, 800);
        drive_frame(&mut app, 560, 720);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: first_run_state_is_actionable_by_pointer_keyboard_and_assistive_tech ---
    {
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        drive_frame(&mut app, 760, 620);

        assert!(app.store.is_empty());
        assert!(app.empty_action_rect.width() >= 44.0);
        let first_run_area = Rect::new(
            app.rect.x0,
            app.rect.y0 + app.toolbar_h(),
            app.rect.x1,
            app.rect.y1,
        );
        assert!(first_run_area.contains(app.empty_action_rect.center()));

        let root = app
            .accessibility()
            .expect("Contacts exposes a semantic window");
        assert_eq!(root.role, ui::a11y::Role::Window);
        assert!(
            root.children
                .iter()
                .flat_map(|node| node.children.iter())
                .all(|node| node.action_target.as_deref() != Some("contacts.export")),
            "an empty address book does not advertise a meaningless export action",
        );
        let action = root
            .children
            .iter()
            .flat_map(|node| node.children.iter())
            .find(|node| node.action_target.as_deref() == Some("contacts.empty.new"))
            .expect("the first-run action is exposed");
        assert_eq!(action.role, ui::a11y::Role::Button);

        assert!(app.perform_semantic_action(
            "contacts.empty.new",
            &ui::a11y::Action::Focus,
            &mut doc,
            &ROOT,
        ));
        assert!(app.empty_action_focused);
        assert!(app.perform_semantic_action(
            "contacts.empty.new",
            &ui::a11y::Action::Activate,
            &mut doc,
            &ROOT,
        ));
        assert_eq!(app.mode, Mode::Edit);
        assert!(app.form.as_ref().is_some_and(|form| form.is_new));
    }
}

/// The on-disk book persists creations and deletions, merges with a diverged replica, and imports real VCF files.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
#[serial_test::serial]
fn saved_book_persists_across_restarts_and_imports() {
    // --- phase: created_contact_persists_across_a_restart ---
    // The production restart path: a contact created through the real edit
    // flow goes into the shared document, the store writes that document,
    // and a second launch reading the store finds the contact again.
    {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", home.path())
            .remove("TILO_PACKAGE_ID");

        // First launch.
        let mut store = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc = store.load_or_new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty(), "a fresh store shows an empty book");
        let mut c = Contact::new();
        c.first = "Persisted".into();
        c.last = "Person".into();
        c.phones.push(Labeled::new("mobile", "+1 555 0100"));
        app.begin_edit(c, true);
        app.save_edit(&mut doc);
        assert_eq!(app.store.len(), 1, "the contact is in the working set");
        store.save(&mut doc).unwrap();
        drop((store, doc, app)); // the process exits here

        // Second launch ("reboot"): a new app over the reloaded document.
        let mut store2 = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc2 = store2.load().unwrap().expect("the store has a document");
        let mut app2 = ContactsApp::new();
        app2.sync(&mut doc2, &ROOT);
        assert_eq!(
            app2.store.len(),
            1,
            "the saved contact loads on the next launch"
        );
        let loaded = &app2.store.contacts[0];
        assert_eq!(loaded.first, "Persisted");
        assert_eq!(loaded.phones[0].value, "+1 555 0100");
    }

    // --- phase: a_saved_book_still_merges_with_a_replica_that_diverged_before_the_save ---
    // The property the old save path destroyed, asserted in the app that
    // had the bug.
    //
    // The old save path rebuilt a fresh `AutoCommit` from the contact list on
    // every write. The document it produced shared no history — and no
    // object identity — with the one it came from, so merging it with a
    // replica that had diverged produced two unrelated `contacts` objects
    // and every record twice. Round-tripping through the store instead must
    // leave the two sides mergeable: one shared object, each side's edit
    // present exactly once.
    {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", home.path())
            .remove("TILO_PACKAGE_ID");
        let mut store = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc = store.new_document();

        // A shared ancestor: one contact both sides know about.
        let mut shared = Contact::new();
        shared.first = "Ada".into();
        shared.last = "Lovelace".into();
        let shared_id = shared.id;
        ContactsDoc::upsert_contact(&mut doc, &shared);
        store.save(&mut doc).unwrap();

        // A peer forks here and adds a contact of its own.
        let mut peer = doc.fork();
        let mut theirs = Contact::new();
        theirs.first = "Grace".into();
        theirs.last = "Hopper".into();
        let their_id = theirs.id;
        ContactsDoc::upsert_contact(&mut peer, &theirs);

        // Meanwhile this app adds one and the session ends.
        let mut mine = Contact::new();
        mine.first = "Alan".into();
        mine.last = "Turing".into();
        let my_id = mine.id;
        ContactsDoc::upsert_contact(&mut doc, &mine);
        store.save(&mut doc).unwrap();
        drop((store, doc));

        // Restart, then merge the peer that diverged before the save.
        let mut store2 = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut restored = store2.load().unwrap().expect("the saved document");
        restored
            .merge_incoming(&mut peer)
            .expect("a stored book merges with a peer");

        let book = ContactsDoc::list_contacts(&restored);
        assert_eq!(
            book.len(),
            3,
            "each side's contact appears exactly once, not duplicated: {:?}",
            book.iter().map(|c| c.first.clone()).collect::<Vec<_>>()
        );
        for (id, name) in [(shared_id, "Ada"), (my_id, "Alan"), (their_id, "Grace")] {
            let found = book.iter().filter(|c| c.id == id).count();
            assert_eq!(
                found, 1,
                "{name} must be present exactly once, found {found}"
            );
        }
        // The decisive check: both sides edited the *same* contacts object.
        // A rebuilt-from-scratch save would have made two, and the merge
        // would leave a conflict at the key.
        assert_eq!(
            restored.get_all(&ROOT, "contacts").unwrap().len(),
            1,
            "history was preserved, so the merge reconciled one object rather \
             than colliding two unrelated ones"
        );
    }

    // --- phase: import_reads_real_vcf_files_and_persists_them ---
    {
        let (dir, path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        // Two separate, differently-shaped .vcf files — the multi-select the
        // portal hands back. The first holds one card, the second holds two,
        // so the merge over arbitrarily many files is what's exercised.
        let vcf_a = dir.join("work.vcf");
        std::fs::write(
            &vcf_a,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported Person\r\nN:Person;Imported;;;\r\nEMAIL;TYPE=HOME:imported@example.com\r\nEND:VCARD\r\n",
        )
        .unwrap();
        let vcf_b = dir.join("family.vcard");
        std::fs::write(
            &vcf_b,
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Byron\r\nN:Byron;Ada;;;\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n\
             BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nORG:US Navy\r\nEND:VCARD\r\n",
        )
        .unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());

        // Drive the parse+merge core directly with the paths the portal would
        // have handed back (the picker itself can't run headless).
        let summary = app.import_paths(&mut doc, &[vcf_a, vcf_b]);
        assert_eq!(
            summary.created, 3,
            "all three cards across the two files were imported"
        );
        assert_eq!(
            app.store.len(),
            3,
            "the parsed cards are in the working set"
        );
        let firsts: std::collections::HashSet<&str> = app
            .store
            .contacts
            .iter()
            .map(|c| c.first.as_str())
            .collect();
        assert!(firsts.contains("Imported"));
        assert!(firsts.contains("Ada"));
        assert!(firsts.contains("Grace"));

        // The import reached the shared document, which is what the store
        // writes — so it survives a restart.
        assert_eq!(
            ContactsDoc::list_contacts(&doc).len(),
            3,
            "written to the doc"
        );
        let _ = path;

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: deleting_a_contact_persists_the_removal ---
    {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", home.path())
            .remove("TILO_PACKAGE_ID");
        // Seed a book into the store, then delete one contact and restart.
        let book = sample_contacts();
        let removed = book[0].id;
        let mut store = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc = store.new_document();
        for c in &book {
            ContactsDoc::upsert_contact(&mut doc, c);
        }
        store.save(&mut doc).unwrap();

        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let before = app.store.len();
        assert_eq!(before, book.len(), "the seeded book loaded");
        app.delete_selected(&mut doc, removed);
        store.save(&mut doc).unwrap();
        drop((store, doc, app));

        // Reload from the store: the deletion stuck.
        let mut store2 = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let doc2 = store2.load().unwrap().expect("document after restart");
        let reloaded = ContactsDoc::list_contacts(&doc2);
        assert_eq!(
            reloaded.len(),
            before - 1,
            "the removal was written to the store"
        );
        assert!(!reloaded.iter().any(|c| c.id == removed));
    }
}

/// Re-imports recognise UIDs and uidless cards, disconnected imports converge, hostile UIDs go to review, and empty imports are no-ops.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn imports_recognise_identities_without_duplicating() {
    // --- phase: reimporting_an_export_recognises_its_uid_without_duplicating_it ---
    {
        let (dir, _path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let card = sample_contacts().into_iter().next().unwrap();
        let vcf = dir.join("contacts-export.vcf");
        std::fs::write(&vcf, vcard::to_vcard(&card, VCardVersion::V4)).unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);

        let first = app.import_paths(&mut doc, std::slice::from_ref(&vcf));
        assert_eq!(first.created, 1);
        assert_eq!(app.store.len(), 1);
        let second = app.import_paths(&mut doc, std::slice::from_ref(&vcf));
        assert_eq!(second.created, 0);
        assert_eq!(second.unchanged, 1);
        assert_eq!(
            app.store.len(),
            1,
            "an exported UID resolves to the existing contact rather than a duplicate"
        );
        assert_eq!(app.store.contacts[0].id, card.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: reimporting_the_same_uidless_card_is_idempotent ---
    {
        let (dir, _path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("external.vcf");
        std::fs::write(
            &vcf,
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;TYPE=work:+1 202 555 0143\r\nEMAIL:grace@example.com\r\nEND:VCARD\r\n",
        )
        .unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let first = app.import_paths(&mut doc, std::slice::from_ref(&vcf));
        let imported_id = app.store.contacts[0].id;
        let imported_phone_id = app.store.contacts[0].phones[0].id;
        let second = app.import_paths(&mut doc, std::slice::from_ref(&vcf));

        assert_eq!(first.created, 1);
        assert_eq!(second.created, 0);
        assert_eq!(second.unchanged, 1);
        assert_eq!(app.store.len(), 1);
        assert_eq!(app.store.contacts[0].id, imported_id);
        assert_eq!(app.store.contacts[0].phones[0].id, imported_phone_id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: disconnected_uidless_imports_converge_on_one_contact_and_stable_rows ---
    {
        let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;TYPE=work:+1 202 555 0143\r\nEMAIL:grace@example.com\r\nCATEGORIES:Pioneers\r\nEND:VCARD\r\n";
        let mut ancestor = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut ancestor, &ROOT);
        ancestor.commit();
        let mut left = ancestor.fork();
        let mut right = ancestor.fork();
        let left_contact = vcard::parse_vcards(card).pop().unwrap();
        let right_contact = vcard::parse_vcards(card).pop().unwrap();
        assert_eq!(left_contact, right_contact);

        tilo_actions::Action::apply(
            &crate::actions::create_contact_action(&left_contact),
            &mut left,
        )
        .unwrap();
        tilo_actions::Action::apply(
            &crate::actions::create_contact_action(&right_contact),
            &mut right,
        )
        .unwrap();
        left.commit();
        right.commit();
        left.merge_incoming(&mut right).unwrap();

        let merged = ContactsDoc::list_contacts(&left);
        assert_eq!(
            merged.len(),
            1,
            "the repeated external import is one entity"
        );
        assert_eq!(merged[0].phones.len(), 1);
        assert_eq!(merged[0].emails.len(), 1);
        assert_eq!(merged[0].groups.len(), 1);
        assert_eq!(merged[0].id, left_contact.id);
        assert_eq!(merged[0].phones[0].id, left_contact.phones[0].id);
    }

    // --- phase: hostile_known_uid_is_reported_for_review_without_overwriting_contact ---
    {
        let (dir, _path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let original = sample_contacts().into_iter().next().unwrap();
        let malicious = vcard::to_vcard(&original, VCardVersion::V4)
            .replace(&original.emails[0].value, "attacker@example.invalid");
        let original_file = dir.join("original.vcf");
        let malicious_file = dir.join("malicious.vcf");
        std::fs::write(&original_file, vcard::to_vcard(&original, VCardVersion::V4)).unwrap();
        std::fs::write(&malicious_file, malicious).unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.import_paths(&mut doc, &[original_file]).created, 1);

        let result = app.import_paths(&mut doc, &[malicious_file]);
        assert_eq!(result.conflicts, 1);
        assert!(result.failure_message().contains("review"));
        let resources = ui::resources::resources(&app, &doc);
        let review = resources[0]
            .children
            .iter()
            .find(|resource| resource.kind == "contacts.import-conflict")
            .expect("the pending collision has an addressable review resource");
        assert!(
            review
                .actions
                .iter()
                .any(|action| action.id == "keep-existing")
        );
        assert!(
            review
                .actions
                .iter()
                .any(|action| action.id == "import-as-new")
        );
        assert_eq!(
            app.store.len(),
            1,
            "a collision does not create a duplicate"
        );
        assert_eq!(
            app.store.get(original.id).unwrap().emails[0].value,
            original.emails[0].value,
            "an untrusted UID cannot overwrite the known contact"
        );
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|(message, _)| message.contains("review")),
            "the collision is visible to the user"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: import_of_nothing_is_a_clean_noop ---
    {
        // An empty selection (or a cancelled picker) fabricates nothing: the
        // book stays empty and disk is untouched — the button never silently
        // invents contacts.
        let (dir, path) = scratch_book();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());

        // Empty selection.
        assert!(!app.import_paths(&mut doc, &[]).completed());
        // A cancelled picker drained through the real path.
        import_with(&mut app, ImportOutcome::Cancelled);
        app.sync(&mut doc, &ROOT);

        assert!(app.store.is_empty(), "import of nothing adds nothing");
        assert!(
            ContactsDoc::list_contacts(&doc).is_empty(),
            "and writes nothing to the doc"
        );
        assert!(!path.exists(), "and never touches disk");
        let _ = &path;
        assert!(app.toast.is_none(), "a cancelled/empty import stays silent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Import/export jobs are portal-brokered, merge through sync, and parser generations fence stale or exhausted workers.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn import_export_jobs_are_brokered_and_generation_fenced() {
    // --- phase: import_drain_merges_selected_files_through_sync ---
    {
        // The live plumbing end to end: an ImportOutcome::Selected on the
        // channel is drained by `sync` (the per-frame drain) and merged into
        // the book — exactly what the portal worker feeds after the user picks
        // files. Only the host dialog is stood in for by a recording host; the
        // request, drain, parse and merge are real.
        let (dir, path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("incoming.vcf");
        std::fs::write(
        &vcf,
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Drained Person\r\nN:Person;Drained;;;\r\nEND:VCARD\r\n",
    )
    .unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        let _ = &path;
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());

        // Run the live Import action with the host's chooser picking `vcf`,
        // then run one real frame's sync — which drains it.
        import_with(&mut app, ImportOutcome::Selected(vec![vcf]));
        assert!(
            app.portal_in_flight(),
            "a request is in flight until drained"
        );
        // The picker is drained on this frame; parsing happens on a worker so
        // a slow vCard cannot block rendering. Drive a few frames instead of
        // assuming the scheduler has already run that worker.
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            app.sync(&mut doc, &ROOT);
            if !app.portal_in_flight() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !app.portal_in_flight(),
            "draining the outcome clears the request"
        );
        assert_eq!(app.store.len(), 1, "the drained selection was imported");
        assert_eq!(app.store.contacts[0].first, "Drained");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: stale_background_parser_cannot_apply_or_leave_an_accepted_import_pending ---
    {
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let mut stale = Contact::new();
        stale.first = "Stale worker".into();
        let (sender, receiver) = mpsc::sync_channel(1);
        app.import_generation = 2;
        app.import_parse_rx = Some(receiver);
        let invocation = tilo_actions::InvocationId::new();
        app.resource_invocation = Some(invocation);
        sender
            .send(ParsedImport {
                generation: 1,
                contacts: vec![stale],
                errors: 0,
            })
            .unwrap();

        app.sync(&mut doc, &ROOT);
        assert!(
            app.store.is_empty(),
            "wrong-generation data never reaches the book"
        );
        assert!(app.import_parse_rx.is_none());
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
    }

    // --- phase: brokered_file_operations_refuse_legacy_direct_paths ---
    {
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::sample();
        app.sync(&mut doc, &ROOT);
        for operation in [
            crate::actions::ContactsOperation::ImportVcard {
                path: Some("/etc/passwd".into()),
            },
            crate::actions::ContactsOperation::ExportAllVcard {
                path: Some("/tmp/contacts.vcf".into()),
            },
        ] {
            let payload = serde_json::to_vec(&operation).unwrap();
            let error = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::CONTACTS_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut doc,
                    &ROOT,
                )
                .expect_err("a broker cannot bypass the system file picker");
            assert!(error.contains("system file picker"));
            assert!(app.resource_invocation.is_none());
        }
    }

    // --- phase: exhausted_parser_generation_refuses_a_new_import_and_settles_the_job ---
    {
        let (dir, _path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("generation-limit.vcf");
        std::fs::write(&vcf, "BEGIN:VCARD\r\nFN:Never Parsed\r\nEND:VCARD\r\n").unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let invocation = tilo_actions::InvocationId::new();
        app.resource_invocation = Some(invocation);
        app.import_generation = u64::MAX;
        import_with(&mut app, ImportOutcome::Selected(vec![vcf]));

        app.sync(&mut doc, &ROOT);
        assert!(
            app.store.is_empty(),
            "no wrapped parser generation can apply data"
        );
        assert!(app.import_parse_rx.is_none());
        assert_eq!(
            app.import_generation,
            u64::MAX,
            "the exhausted token is never reused"
        );
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|(message, _)| message.contains("generation exhausted"))
        );
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: cancelling_at_generation_max_invalidates_the_worker_without_wrapping ---
    {
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let (sender, receiver) = mpsc::sync_channel(1);
        let invocation = tilo_actions::InvocationId::new();
        app.resource_invocation = Some(invocation);
        app.import_parse_rx = Some(receiver);
        app.import_generation = u64::MAX;

        let update = app
            .poll_resource_operation(invocation, true, &doc, &ROOT)
            .expect("accepted import is settled when cancelled");
        assert!(matches!(
            update,
            tilo_actions::InvocationUpdate::Cancelled { .. }
        ));
        assert!(
            app.import_parse_rx.is_none(),
            "the worker result is detached"
        );
        assert_eq!(
            app.import_generation,
            u64::MAX,
            "cancellation never wraps to 0"
        );
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|(message, _)| message.contains("generation exhausted"))
        );
        // The worker can still finish in the background, but its disconnected
        // receiver means no stale contacts can reach this app instance.
        let _ = sender.send(ParsedImport {
            generation: u64::MAX,
            contacts: vec![Contact::new()],
            errors: 0,
        });
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());
    }

    // --- phase: export_with_empty_selection_never_opens_the_portal ---
    {
        // Choosing a version with nothing ticked must not spin up a Save
        // dialog or park anything — it guides the user instead.
        let mut h = Harness::new(1280, 800);
        let opened = h.app.request_export(Vec::new(), VCardVersion::V3);
        assert!(!opened, "an empty selection does not open the Save dialog");
        assert!(h.app.pending_export.is_none(), "nothing is parked");
        assert!(!h.app.portal_in_flight(), "no portal request is in flight");
        match &h.app.toast {
            Some((msg, _)) => assert!(msg.to_lowercase().contains("select")),
            None => panic!("an empty export must surface a guiding toast"),
        }
    }
}

/// Import conflicts and discovered duplicates have an accessible, scrollable, responsive review route resolving explicit choices.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn duplicate_and_conflict_review_is_accessible() {
    // --- phase: import_conflict_review_is_visible_accessible_and_resolves_both_choices ---
    {
        fn conflicting_app() -> (ContactsApp, AutoCommit, Contact, Contact) {
            let original = sample_contacts().into_iter().next().unwrap();
            let mut incoming = original.clone();
            incoming.emails[0].value = "attacker@example.invalid".into();
            let mut doc = AutoCommit::new();
            let mut app = ContactsApp::new();
            app.sync(&mut doc, &ROOT);
            let mut summary = ImportSummary::default();
            let mut first_new = None;
            app.absorb_import_contact(&mut doc, original.clone(), &mut summary, &mut first_new);
            app.absorb_import_contact(&mut doc, incoming.clone(), &mut summary, &mut first_new);
            assert_eq!(summary.conflicts, 1);
            (app, doc, original, incoming)
        }

        // Wide rendering exposes a real modal rather than only a toast or a
        // palette resource, and its two decisions are in the semantic tree.
        let (app, doc, original, _) = conflicting_app();
        let mut app = frame_at(app, doc, 1280, 800);
        let token = app.import_conflicts[0].token;
        let keep = import_conflict_target(token, "keep-existing");
        let import_as_new = import_conflict_target(token, "import-as-new");
        let tree = app.accessibility().expect("review dialog is accessible");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        assert!(access_target(&tree, &keep).is_some());
        assert!(access_target(&tree, &import_as_new).is_some());
        app.ensure_import_conflict_dialog();
        let mut tr = TextRenderer::new();
        let rect = app.rect;
        app.import_conflict_dialog
            .as_mut()
            .unwrap()
            .dialog
            .layout_in(rect, &Theme::dark(), &mut tr);
        assert!(app.modal_capture().is_some(), "review dialog is painted");
        assert!(perform(&mut app, &keep, &ui::a11y::Action::Activate));
        sync_doc(&mut app);
        assert!(app.import_conflicts.is_empty());
        assert_eq!(app.store.len(), 1);
        assert_eq!(
            app.store.get(original.id).unwrap().emails[0].value,
            original.emails[0].value
        );

        // Import-as-new is the destructive, confirmation-labelled decision.
        // It mints a fresh local id and dispatches the normal CreateContact
        // action, never patching the contact identified by the untrusted UID.
        let (app, doc, original, incoming) = conflicting_app();
        let mut app = frame_at(app, doc, 560, 720);
        let token = app.import_conflicts[0].token;
        let import_as_new = import_conflict_target(token, "import-as-new");
        app.ensure_import_conflict_dialog();
        let mut tr = TextRenderer::new();
        let rect = app.rect;
        app.import_conflict_dialog
            .as_mut()
            .unwrap()
            .dialog
            .layout_in(rect, &Theme::dark(), &mut tr);
        assert!(
            app.modal_capture().is_some(),
            "review is also rendered narrow"
        );
        assert!(perform(
            &mut app,
            &import_as_new,
            &ui::a11y::Action::Activate,
        ));
        sync_doc(&mut app);
        assert!(app.import_conflicts.is_empty());
        assert_eq!(app.store.len(), 2);
        assert_eq!(
            app.store.get(original.id).unwrap().emails[0].value,
            original.emails[0].value
        );
        assert!(app.store.contacts.iter().any(|contact| {
            contact.id != original.id && contact.emails[0].value == incoming.emails[0].value
        }));
        assert!(
            app.last_dispatched().is_some(),
            "Import as new uses ContactsAction"
        );
    }

    // --- phase: narrow_duplicate_review_is_scrollable_and_exposes_explicit_value_choices ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        app.shots_show_duplicate_review();
        let mut app = frame_at(app, doc, 390, 844);
        app.text_scale = 2.0;

        let review = app.duplicate_review.as_ref().expect("review is open");
        let geometry = app.duplicate_review_geometry(review);
        assert!(geometry.full_screen);
        assert_eq!(
            geometry.rows.len(),
            7,
            "the fixture exercises every scalar conflict"
        );
        assert!(
            geometry.max_scroll > 0.0,
            "every conflict remains reachable by scrolling"
        );
        assert!(
            geometry.cancel.y1 < geometry.merge.y0,
            "footer actions stack instead of colliding"
        );

        let tree = app.accessibility().expect("duplicate review has semantics");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        let current = "contacts.duplicate.field:Organization:current";
        let other = "contacts.duplicate.field:Organization:other";
        assert!(access_target(&tree, current).is_some());
        assert!(access_target(&tree, other).is_some());
        assert!(perform(&mut app, other, &ui::a11y::Action::Activate));
        assert!(app.duplicate_review.as_ref().is_some_and(|review| {
            review
                .take_from_source
                .contains(&crate::actions::MergeField::Organization)
        }));
        assert!(perform(&mut app, current, &ui::a11y::Action::Activate));
        assert!(app.duplicate_review.as_ref().is_some_and(|review| {
            !review
                .take_from_source
                .contains(&crate::actions::MergeField::Organization)
        }));
    }

    // --- phase: discovered_duplicates_have_a_canonical_responsive_review_route ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let survivor = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .expect("sample selection exists");
        let mut source = survivor;
        source.id = uuid::Uuid::new_v4();
        source.notes = "Independent import that needs review".into();
        app.store.upsert(source);
        app.duplicate_candidate = crate::actions::duplicate_candidates(&app.store.contacts, 1)
            .into_iter()
            .next();
        app.duplicate_candidate_heads.clone_from(&app.model_heads);
        let candidate = app
            .duplicate_candidate
            .clone()
            .expect("shared email/name makes a candidate");

        // The wide toolbar reserves a separate hit target, so its action never
        // crowds import/export/new and is exposed with the safety explanation.
        let mut app = frame_at(app, doc, 1_280, 800);
        assert!(app.hit.duplicates.area() > 0.0);
        for other in [app.hit.import, app.hit.export, app.hit.new] {
            assert!(
                app.hit.duplicates.intersect(other).area() <= 0.0,
                "duplicate review target must not overlap another toolbar action"
            );
        }
        app.last_mouse = app.hit.duplicates.center();
        assert_eq!(
            Widget::tooltip(&*app),
            Some("Review possible duplicate — nothing merges automatically")
        );
        let tree = app.accessibility().expect("toolbar has semantics");
        let review = access_target(&tree, "contacts.duplicates").expect("review is focusable");
        assert_eq!(review.role, Role::Button);
        assert!(
            review
                .description
                .as_deref()
                .is_some_and(|copy| copy.contains("Nothing is merged automatically"))
        );

        // Semantic activation serializes the same typed View operation published
        // by the exact duplicate resource, then opens the field-level review.
        assert!(perform(
            &mut app,
            "contacts.duplicates",
            &ui::a11y::Action::Activate,
        ));
        let opened = app.duplicate_review.as_ref().expect("review opened");
        assert_eq!(opened.survivor.id, candidate.survivor_id);
        assert_eq!(opened.source.id, candidate.source_id);

        // At 200% on a phone width, every compact toolbar target remains a
        // distinct touch/focus rectangle rather than turning into overlaid icons.
        app.duplicate_review = None;
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        app.layout();
        assert!(app.toolbar_is_compact());
        for (left, right) in [
            (app.hit.import, app.hit.duplicates),
            (app.hit.duplicates, app.hit.export),
            (app.hit.export, app.hit.new),
        ] {
            assert!(
                left.intersect(right).area() <= 0.0,
                "compact duplicate controls remain disjoint at 200%"
            );
            assert!(left.width() >= 40.0 && right.width() >= 40.0);
        }
    }
}

fn access_target<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
    if node.action_target.as_deref() == Some(target) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| access_target(child, target))
}

/// Export and unsaved modals are focus-trapped and semantic; detail, editor and popup controls reach the durable action path.
#[allow(
    clippy::too_many_lines,
    reason = "this end-to-end semantic route test intentionally exercises each popup control in one readable scenario"
)]
#[test]
fn modals_and_editors_expose_semantic_action_routes() {
    // --- phase: export_modal_is_focus_trapped_semantic_and_virtualized ---
    {
        let mut h = Harness::new(900, 640);
        h.press_left(h.app.hit.export.center());
        assert!(h.app.traps_focus(), "export blocks focus behind its scrim");
        let tree = h.app.accessibility().expect("export has semantics");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        assert!(access_target(&tree, "contacts.export.select-none").is_some());

        assert!(h.press_named(NamedKey::Tab));
        assert!(
            access_target(
                &h.app.accessibility().expect("focused export semantics"),
                "contacts.export.select-all",
            )
            .is_some_and(|node| node.state.focused),
            "Tab moves focus into the export dialog rather than behind its scrim",
        );
        assert!(h.press_named(NamedKey::Space));
        assert_eq!(h.app.export_dialog.selection().len(), h.app.store.len());

        let (app, doc) = h.parts();
        assert!(app.perform_semantic_action(
            "contacts.export.select-none",
            &ui::a11y::Action::Activate,
            doc,
            &ROOT,
        ));
        assert!(h.app.export_dialog.selection().is_empty());
        let id = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort)
            .rows[0]
            .0;
        let target = format!("contacts.export.row:{id}");
        let (app, doc) = h.parts();
        assert!(app.perform_semantic_action(&target, &ui::a11y::Action::ToggleCheck, doc, &ROOT,));
        assert!(h.app.export_dialog.selection().contains(&id));

        h.app.store.contacts = (0..1_000)
            .map(|index| Contact {
                id: uuid::Uuid::new_v4(),
                first: format!("Export {index:04}"),
                ..Contact::default()
            })
            .collect();
        let tree = h
            .app
            .accessibility()
            .expect("large export remains semantic");
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("export choice list is exposed");
        assert!(
            list.children.len() < h.app.store.len(),
            "export materializes only visible semantic rows"
        );
    }

    // --- phase: unsaved_modal_is_focus_trapped_semantic_and_resolves_via_action ---
    {
        let mut h = Harness::new(1280, 800);
        let current = h.app.selected.expect("sample selects one contact");
        let contact = h.app.store.get(current).cloned().unwrap();
        h.app.begin_edit(contact, false);
        h.app
            .form
            .as_mut()
            .and_then(|form| {
                form.inputs
                    .iter_mut()
                    .find(|input| input.slot == Slot::Title)
            })
            .expect("sample editor has title")
            .ti
            .set_text("Discard me");
        let other = h
            .app
            .visible_ids()
            .into_iter()
            .find(|id| *id != current)
            .expect("sample has another contact");
        h.app.request_select(other);
        assert!(h.app.traps_focus(), "unsaved choice traps focus");
        let tree = h.app.accessibility().expect("unsaved dialog has semantics");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        assert!(access_target(&tree, "contacts.unsaved.discard").is_some());
        assert!(h.press_named(NamedKey::Tab));
        assert_eq!(h.app.unsaved_focus, UnsavedFocus::Discard);
        let (app, doc) = h.parts();
        assert!(app.perform_semantic_action(
            "contacts.unsaved.discard",
            &ui::a11y::Action::Activate,
            doc,
            &ROOT,
        ));
        assert!(h.app.pending_switch.is_none());
        assert_eq!(h.app.selected, Some(other));
    }

    // --- phase: detail_and_editor_semantics_reach_the_same_durable_action_path ---
    {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let mut app = frame_at(app, doc, 1280, 800);

        let id = app.selected.expect("sample contact is selected");
        let favorite = app.store.get(id).expect("selected contact exists").favorite;
        let favorite_target = format!("contacts.detail.favorite:{id}");
        let tree = app.accessibility().expect("Contacts exposes semantics");
        let favorite_button = access_target(&tree, &favorite_target)
            .expect("the painted favorite star is reachable to assistive tech");
        assert_eq!(favorite_button.role, Role::ToggleButton);
        assert_eq!(favorite_button.state.checked, Some(favorite));

        assert!(perform(
            &mut app,
            &favorite_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(
            app.store.get(id).map(|contact| contact.favorite),
            Some(!favorite),
            "semantic activation updates the visible model"
        );
        assert_eq!(
            ContactsDoc::list_contacts(doc_of(&app))
                .into_iter()
                .find(|contact| contact.id == id)
                .map(|contact| contact.favorite),
            Some(!favorite),
            "and dispatches the canonical collaborative action into the document"
        );

        let contact = app.store.get(id).cloned().unwrap();
        app.begin_edit(contact.clone(), false);
        rerender_at(&mut app, 1280, 800);
        let first_target = form_field_target(id, Slot::First);
        let save_target = format!("contacts.form.save:{id}");
        let tree = app.accessibility().expect("editor semantics are exposed");
        assert_eq!(
            access_target(&tree, &first_target).map(|node| node.role),
            Some(Role::TextInput),
            "a painted draft field is a real accessible text input"
        );
        assert!(access_target(&tree, &save_target).is_some());

        assert!(perform(
            &mut app,
            &first_target,
            &ui::a11y::Action::SetValue("Augusta".into()),
        ));
        assert_eq!(
            app.form.as_ref().map(|form| form.draft.first.as_str()),
            Some(contact.first.as_str()),
            "SetValue remains a cancellable local draft until Save"
        );
        assert!(perform(&mut app, &save_target, &ui::a11y::Action::Activate,));
        assert_eq!(
            ContactsDoc::list_contacts(doc_of(&app))
                .into_iter()
                .find(|stored| stored.id == id)
                .map(|stored| stored.first),
            Some("Augusta".into()),
            "Save commits the assistive edit through ContactsAction"
        );
    }

    // --- phase: semantic_popup_controls_cover_type_remove_calendar_group_and_unique_targets ---
    {
        fn assert_unique_targets(node: &AccessNode, seen: &mut std::collections::HashSet<String>) {
            if let Some(target) = &node.action_target {
                assert!(
                    seen.insert(target.clone()),
                    "duplicate semantic target: {target}"
                );
            }
            for child in &node.children {
                assert_unique_targets(child, seen);
            }
        }

        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let id = app.selected.unwrap();
        app.begin_edit(app.store.get(id).cloned().unwrap(), false);
        let mut app = frame_at(app, doc, 1280, 800);

        let (type_row, _) = app.hit.type_chips[0];
        let type_target = type_chip_target(id, type_row);
        assert!(perform(&mut app, &type_target, &ui::a11y::Action::Activate,));
        assert!(matches!(app.popup, Some(Popup::TypeMenu { .. })));
        let option_target = type_option_target(id, type_row, "other");
        let tree = app.accessibility().unwrap();
        assert_eq!(
            access_target(&tree, &option_target).map(|node| node.role),
            Some(Role::RadioButton)
        );
        assert!(perform(
            &mut app,
            &option_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(
            type_label(&app.form.as_ref().unwrap().draft, type_row),
            "other"
        );

        let before = app.form.as_ref().unwrap().draft.phones.len();
        let remove_target = remove_type_target(id, type_row);
        assert!(perform(
            &mut app,
            &remove_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(app.form.as_ref().unwrap().draft.phones.len(), before - 1);

        rerender_at(&mut app, 1280, 800);
        let calendar = calendar_target(id);
        assert!(perform(&mut app, &calendar, &ui::a11y::Action::Activate,));
        let Some(Popup::Calendar { year, month, .. }) = app.popup else {
            panic!("semantic calendar button opens the date picker");
        };
        let day = calendar_day_target(id, year, month, 1);
        let tree = app.accessibility().unwrap();
        assert!(
            access_target(&tree, &day).is_some(),
            "calendar days are exposed"
        );
        assert!(perform(&mut app, &day, &ui::a11y::Action::Activate));
        assert_eq!(
            app.form
                .as_ref()
                .unwrap()
                .draft
                .birthday
                .map(|birthday| birthday.iso()),
            Some(format!("{year:04}-{month:02}-01")),
            "calendar selection remains in the cancellable draft"
        );

        app.cancel_edit();
        rerender_at(&mut app, 1280, 800);
        assert!(perform(
            &mut app,
            "contacts.group",
            &ui::a11y::Action::Expand,
        ));
        let group = "Friends";
        let group_target = group_option_target(group);
        let tree = app.accessibility().unwrap();
        assert_eq!(
            access_target(&tree, &group_target).map(|node| node.role),
            Some(Role::RadioButton)
        );
        assert!(perform(
            &mut app,
            &group_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(app.group_filter.as_deref(), Some(group));

        let mut targets = std::collections::HashSet::new();
        assert_unique_targets(&app.accessibility().unwrap(), &mut targets);
    }
}

fn live_message<'a>(node: &'a AccessNode, message: &str) -> Option<&'a AccessNode> {
    if node.name.as_deref() == Some(message) && node.live.is_some() {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| live_message(child, message))
}

/// Mount `app` over the case's `doc` in the production frame host and run one
/// frame at `w`x`h`.
fn frame_at(app: ContactsApp, doc: AutoCommit, w: u32, h: u32) -> Framed<ContactsApp> {
    let mut state = ui::State::new();
    assert!(
        state.adopt_doc(doc),
        "the harness adopts the case's document"
    );
    Framed::new(state, app, (w, h), Theme::dark())
}

/// Lay the same instance out at `w`x`h`, to simulate a live window resize (the
/// ui loop re-runs layout on the existing widget when the allocation changes).
fn rerender_at(app: &mut Framed<ContactsApp>, w: u32, h: u32) {
    Framed::harness_mut(app)
        .relayout_themed((w, h), Theme::dark())
        .expect("contacts frame lays out");
}

fn perform(app: &mut Framed<ContactsApp>, target: &str, action: &ui::a11y::Action) -> bool {
    let (app, state) = Framed::split(app);
    app.perform_semantic_action(target, action, &mut state.doc, &ROOT)
}

fn sync_doc(app: &mut Framed<ContactsApp>) {
    let (app, state) = Framed::split(app);
    app.sync(&mut state.doc, &ROOT);
}

fn doc_of(app: &Framed<ContactsApp>) -> &AutoCommit {
    &Framed::harness(app).state().doc
}

/// Form rows, pills, panel lifts, rows and toolbar faces are authored at 1x and grow with the text scale on every preset.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn metrics_follow_the_text_scale_and_presets() {
    // --- phase: edit_form_rows_are_authored_at_one_x_and_hold_scaled_text ---
    // The edit form's rows are the authored 1× design exactly and, at 2×,
    // grow by their lines (whole pixels) so section labels, fields, type
    // faces and the add / action buttons never overprint one another.
    {
        let (sp, mx) = (Spacing::default(), Metrics::BASE);
        let one = FormMetrics::new(&sp, &mx, 1.0, &LabelGrowth::default());
        assert_eq!(one.title_h, f64::from(mx.panel_row_height));
        assert_eq!(one.action_h, f64::from(mx.min_pointer_target));
        assert_eq!(one.label_h, FORM_LABEL_H);
        assert_eq!((one.field_h, one.type_h), (FIELD_H, FIELD_H));
        assert_eq!((one.type_w, one.remove_w), (FORM_TYPE_W, FORM_REMOVE_W));
        assert_eq!(
            (one.add_w, one.add_address_w, one.add_h),
            (
                FORM_ADD_W,
                FORM_ADD_ADDRESS_W,
                f64::from(mx.selector_height)
            )
        );
        assert_eq!(
            (one.fav_w, one.save_w, one.cancel_w),
            (FORM_FAV_W, FORM_SAVE_W, FORM_CANCEL_W)
        );

        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let growth = LabelGrowth::measure(&mut tr);
        assert!(growth.add_phone > 0.0 && growth.save > 0.0 && growth.favorite > 0.0);
        let two = FormMetrics::new(&sp, &mx, 2.0, &growth);
        for value in [
            two.title_h,
            two.action_h,
            two.label_h,
            two.field_h,
            two.type_w,
            two.remove_w,
            two.add_h,
            two.add_w,
            two.add_address_w,
            two.fav_w,
            two.save_w,
            two.cancel_w,
        ] {
            assert_eq!(value.fract(), 0.0, "{value} is not a whole pixel");
        }
        assert!(two.label_h >= line_h(FORM_LABEL_TEXT, 2.0));
        assert!(two.field_h >= line_h(text_base(), 2.0));
        assert!(two.add_h >= line_h(text_sm(), 2.0));
        assert!(
            two.add_w > one.add_w && two.add_w < one.add_w * 2.0,
            "measured, not a blanket 2×"
        );

        // Laid out at 2×, no field, face or button overlaps another, every
        // row has the metrics' height, and the first group's label band
        // clears the header band.
        let mut app = ContactsApp::new();
        app.rect = Rect::new(0.0, 0.0, 1200.0, 900.0);
        app.text_scale = 2.0;
        app.label_growth = growth;
        app.begin_edit(Contact::new(), true);
        app.layout();
        let form = app.form.as_ref().expect("edit mode has a form");
        let mut rects: Vec<Rect> = form.inputs.iter().map(FieldInput::ti_rect).collect();
        rects.extend([
            app.hit.save,
            app.hit.cancel,
            app.hit.fav_toggle,
            app.hit.add_phone,
            app.hit.add_email,
            app.hit.add_address,
            app.hit.cal_btn,
        ]);
        rects.extend(app.hit.type_chips.iter().map(|(_, r)| *r));
        rects.extend(app.hit.remove_btns.iter().map(|(_, r)| *r));
        for (i, a) in rects.iter().enumerate() {
            assert!(a.width() > 0.0 && a.height() > 0.0, "{a:?} is empty");
            for b in &rects[i + 1..] {
                assert!(a.intersect(*b).area() <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
        assert_eq!(app.hit.save.height(), two.action_h);
        assert_eq!(app.hit.add_phone.height(), two.add_h);
        assert!(
            form.inputs.iter().all(|field| {
                let rows = if field.slot == Slot::Notes { 3.0 } else { 1.0 };
                (field.ti_rect().height() - two.field_h * rows).abs() < 0.01
            }),
            "every field uses its authored number of scaled rows"
        );
        let first = form
            .inputs
            .iter()
            .find(|f| f.slot == Slot::First)
            .map(FieldInput::ti_rect)
            .expect("first-name field");
        assert!(first.y0 >= app.form_body_top() + f64::from(app.spacing.s3) + two.label_h - 0.01);
    }

    // --- phase: neighbouring_selection_pills_keep_a_token_gap ---
    // Rule 1 of the 1× polish bar for the contact list: two neighbouring
    // rows' selection pills keep an `s1` between them, and a pill keeps `s2`
    // from the pane's sides — the geometry the audit cannot observe through
    // the app's semantics tree, asserted from the same helper the painter
    // uses.
    {
        let sp = &Theme::dark().tokens.spacing;
        let row_h = ContactRowMetrics::new(sp, 1.0).row_h;
        let a = super::selection_pill(Rect::new(0.0, 0.0, 300.0, row_h), sp);
        let b = super::selection_pill(Rect::new(0.0, row_h, 300.0, 2.0 * row_h), sp);
        assert_eq!(b.y0 - a.y1, f64::from(sp.s1), "pill ↔ pill is one s1");
        assert_eq!(a.x0, f64::from(sp.s2), "pill ↔ pane edge is one s2");
        assert_eq!(300.0 - a.x1, f64::from(sp.s2));
        assert!(a.height() >= row_h - f64::from(sp.s1));
    }

    // --- phase: panel_row_lift_registers_on_every_preset ---
    // The chosen and hovered row lifts on a Panel ground register as a
    // contrast step on every preset — the reason the menu rows do not use
    // `chrome::selection_tier`, whose lift lands within a hairline of the
    // dark and high-contrast Panel grounds.
    {
        use ui::surface::edge_delta;
        use ui::tokens::colors::HAIRLINE_MIN_DELTA;
        let opaque = |c: Color| c.components[3] == 1.0;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = ui::chrome::panel_ground(&theme);
            let active = super::row_tier_on(ground, SelectionState::active(true));
            assert!(opaque(active.fill), "{}", theme.name);
            assert!(
                edge_delta(active.fill, ground) >= HAIRLINE_MIN_DELTA,
                "{}: active menu row does not register on the panel",
                theme.name
            );
            let border = active.border.expect("active rows carry an edge");
            assert!(
                edge_delta(border, active.fill) >= HAIRLINE_MIN_DELTA,
                "{}",
                theme.name
            );
            let hover = super::row_tier_on(ground, SelectionState::new(false, true, false));
            assert!(
                opaque(hover.fill) && hover.border.is_none(),
                "{}",
                theme.name
            );
            assert!(edge_delta(hover.fill, ground) > 0.0, "{}", theme.name);
            let idle = super::row_tier_on(ground, SelectionState::default());
            assert_eq!(idle.fill.components[3], 0.0, "{}", theme.name);
        }
    }

    // --- phase: row_and_toolbar_metrics_follow_the_text_scale ---
    // List rows, the toolbar and its buttons keep their authored 1× sizes
    // and grow from the measured lines they carry at larger text scales, so
    // the 2× name/subtitle collision and the clipped people-count are gone.
    {
        let sp = Spacing::default();
        let one = ContactRowMetrics::new(&sp, 1.0);
        assert_eq!(one.row_h, ROW_H);
        assert_eq!(one.avatar, ROW_AVATAR);

        let two = ContactRowMetrics::new(&sp, 2.0);
        // The 2× row holds both scaled lines with the `s3` the painter leaves
        // above and below them, and lands on a whole pixel.
        assert!(two.row_h >= two.name_h + two.sub_h + 2.0 * f64::from(sp.s3));
        assert_eq!(two.row_h.fract(), 0.0);
        assert!(two.row_h > one.row_h);
        assert!(two.avatar >= ROW_AVATAR && two.avatar <= two.row_h);

        let mut app = ContactsApp::new();
        let (s2, s3) = (f64::from(app.spacing.s2), f64::from(app.spacing.s3));
        assert_eq!(app.toolbar_h(), f64::from(app.spacing.s12));
        assert_eq!(
            app.toolbar_button_h(),
            f64::from(app.metrics.min_pointer_target)
        );
        assert_eq!(
            at_least_whole(FIELD_H, line_h(text_base(), 1.0) + 2.0 * s2),
            FIELD_H,
            "1× search field is the authored 38"
        );
        app.text_scale = 2.0;
        assert!(app.toolbar_h() >= line_h(text_base(), 2.0) + 2.0 * s3);
        assert!(app.toolbar_button_h() >= line_h(TOOLBAR_BUTTON_TEXT, 2.0) + s2);
        assert!(app.toolbar_button_h() < app.toolbar_h());
        for value in [
            app.toolbar_h(),
            app.toolbar_button_h(),
            at_least_whole(FIELD_H, line_h(text_base(), 2.0) + 2.0 * s2),
        ] {
            assert_eq!(
                value.fract(),
                0.0,
                "scaled toolbar metrics land on whole pixels"
            );
        }
        // The scaled row grid is what the virtualised rows and the
        // ensure-visible path read, so they agree with the painter.
        assert_eq!(app.row_metrics(), two);
    }

    // --- phase: toolbar_faces_grow_by_their_measured_labels ---
    // Toolbar faces keep their authored 1× widths and grow by exactly how
    // much wider their measured labels are at the renderer's scale — never
    // by a blanket `× scale` — and the hit rects follow in the same frame.
    {
        let mut app = ContactsApp::new();
        app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
        app.layout();
        assert_eq!(app.hit.new.width(), TOOLBAR_NEW_W);
        assert_eq!(app.hit.import.width(), TOOLBAR_FILE_W);
        assert_eq!(app.hit.export.width(), TOOLBAR_FILE_W);

        let mut tr = TextRenderer::new();
        app.refresh_label_growth(&mut tr);
        assert_eq!(
            app.label_growth,
            LabelGrowth::default(),
            "1× labels grow nothing"
        );
        assert_eq!(app.hit.new.width(), TOOLBAR_NEW_W);

        tr.set_text_scale(2.0);
        app.text_scale = 2.0;
        app.refresh_label_growth(&mut tr);
        // Paint only requests the relayout; the host's next layout pass
        // applies the measured growth. Run it here as the host would.
        assert!(
            app.is_layout_dirty(),
            "a changed label growth requests a relayout"
        );
        app.layout();
        let scaled = f64::from(
            tr.measure_weighted(
                TOOLBAR_NEW_LABEL,
                TOOLBAR_BUTTON_TEXT,
                None,
                TextWeight::Medium,
            )
            .0,
        );
        let one_x = f64::from(
            tr.measure_weighted(
                TOOLBAR_NEW_LABEL,
                TOOLBAR_BUTTON_TEXT / 2.0,
                None,
                TextWeight::Medium,
            )
            .0,
        );
        assert!(scaled > one_x);
        assert!(
            app.hit.new.width() >= TOOLBAR_NEW_W + (scaled - one_x) - 1.0,
            "the New face grew by its label's measured growth"
        );
        assert!(
            app.hit.new.width() < TOOLBAR_NEW_W * 2.0,
            "not a blanket 2× face"
        );
        for r in [app.hit.new, app.hit.export, app.hit.import] {
            assert_eq!(r.width().fract(), 0.0, "face widths are whole pixels");
            assert_eq!(r.y0, app.hit.new.y0, "faces share one baseline row");
        }
        assert!(app.hit.export.x1 < app.hit.new.x0 && app.hit.import.x1 < app.hit.export.x0);
    }
}

#[test]
fn duplicate_review_face_owns_its_drawn_hit_and_semantic_rect() {
    let mut app = ContactsApp::new();
    app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
    let contacts = sample_contacts();
    for contact in &contacts {
        app.store.upsert(contact.clone());
    }
    app.duplicate_candidate = Some(crate::actions::DuplicateCandidate {
        survivor_id: contacts[0].id,
        source_id: contacts[1].id,
        reasons: vec![crate::actions::DuplicateReason::Name],
    });

    app.layout();
    let duplicate = app.hit.duplicates;
    assert_eq!(
        duplicate.width(),
        TOOLBAR_DUPLICATE_W,
        "wide toolbar draws the authored face at 1×"
    );
    assert!(app.hit.import.x1 <= duplicate.x0);
    assert!(duplicate.x1 <= app.hit.export.x0);
    let tree = app.accessibility().expect("contacts has semantics");
    let action = access_target(&tree, "contacts.duplicates")
        .expect("duplicate review is exposed as an action");
    assert_eq!(
        action.bounds.map(ui::a11y::AccessBounds::to_rect),
        Some(duplicate),
        "paint, pointer, and semantic routes share one rectangle"
    );

    let mut tr = TextRenderer::new();
    tr.set_text_scale(2.0);
    app.text_scale = 2.0;
    app.refresh_label_growth(&mut tr);
    app.layout();
    let scaled = app.hit.duplicates;
    assert_eq!(
        scaled.width(),
        (TOOLBAR_DUPLICATE_W + app.label_growth.duplicate).ceil(),
        "the drawn duplicate label and its target grow together at 2×"
    );
    assert!(scaled.width() > duplicate.width());
    assert!(app.hit.import.x1 <= scaled.x0);
    assert!(scaled.x1 <= app.hit.export.x0);
    let tree = app.accessibility().expect("contacts has semantics");
    assert_eq!(
        access_target(&tree, "contacts.duplicates")
            .and_then(|action| action.bounds)
            .map(ui::a11y::AccessBounds::to_rect),
        Some(scaled),
        "scaled paint, pointer, and semantic routes share one rectangle"
    );

    app.duplicate_candidate = None;
    app.layout();
    assert_eq!(
        app.hit.duplicates,
        Rect::ZERO,
        "removing the candidate clears the prior frame's hit target"
    );
}
