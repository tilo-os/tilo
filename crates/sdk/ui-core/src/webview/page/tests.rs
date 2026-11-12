//! What a page request is on the wire and within its bounds, and what an
//! answer to one may carry.

use super::*;

#[test]
fn a_request_says_what_it_did_in_the_words_its_answer_repeats() {
    let element = || "d1e5".to_string();
    let described = |request: PageRequest| request.described();
    assert_eq!(
        described(PageRequest::Click {
            element: element(),
            frame: None,
        }),
        "Clicked element d1e5"
    );
    assert_eq!(
        described(PageRequest::Fill {
            element: element(),
            text: "Maria".into(),
            secret: false,
            frame: None,
        }),
        "Filled element d1e5"
    );
    assert_eq!(
        described(PageRequest::Submit {
            element: element(),
            cross_origin: false,
            frame: None,
        }),
        "Submitted form d1e5"
    );
    assert_eq!(
        described(PageRequest::Select {
            element: element(),
            values: vec!["a".into(), "b".into()],
            frame: None,
        }),
        "Chose a, b on element d1e5"
    );
    let enter = PageKey::parse("Enter").unwrap();
    assert_eq!(
        described(PageRequest::Press {
            element: None,
            key: enter.clone(),
            frame: None,
        }),
        "Pressed Enter"
    );
    assert_eq!(
        described(PageRequest::Press {
            element: Some(element()),
            key: enter,
            frame: None,
        }),
        "Pressed Enter on element d1e5"
    );
    assert_eq!(
        described(PageRequest::Scroll {
            element: None,
            to: Some(ScrollTarget::Bottom),
            by: None,
            into_view: None,
            frame: None,
        }),
        "Scrolled to the bottom"
    );
    assert_eq!(
        described(PageRequest::Scroll {
            element: None,
            to: None,
            by: Some(-400),
            into_view: None,
            frame: None,
        }),
        "Scrolled by -400 pixels"
    );
    assert_eq!(
        described(PageRequest::Scroll {
            element: None,
            to: None,
            by: None,
            into_view: Some(element()),
            frame: None,
        }),
        "Scrolled element d1e5 into view"
    );
}

#[test]
fn requests_are_bounded_and_round_trip_to_the_wire() {
    // phase: element_refs_accept_only_document_and_element_numbers
    {
        assert!(is_element_ref("d1e2"));
        assert!(is_element_ref("d12e345"));
        for bad in [
            "",
            "e2",
            "d1",
            "d1e",
            "de2",
            "d1e2x",
            "d1e2;alert(1)",
            "D1E2",
        ] {
            assert!(!is_element_ref(bad), "{bad}");
        }
        assert!(!is_element_ref(&format!("d1e{}", "9".repeat(40))));
    }
    // phase: key_chords_parse_to_canonical_form
    {
        let key = PageKey::parse("Ctrl+Shift+a").unwrap();
        assert_eq!(key.key, "a");
        assert!(key.modifiers.ctrl && key.modifiers.shift && !key.modifiers.alt);
        assert_eq!(key.chord(), "Control+Shift+a");
        assert_eq!(PageKey::parse(&key.chord()), Some(key));
        assert_eq!(PageKey::parse("Space").unwrap().key, " ");
        assert_eq!(PageKey::parse("Control++").unwrap().key, "+");
        for bad in ["", "Enterr", "Hyper+a", "Shift+Shift+a", "ab", "Control+"] {
            assert!(PageKey::parse(bad).is_none(), "{bad}");
        }
    }
    // phase: key_names_are_read_in_any_letter_case
    {
        // A person writes END or end for the same key a keyboard has.
        for (written, canonical) in [
            ("END", "End"),
            ("end", "End"),
            ("arrowdown", "ArrowDown"),
            ("PageUp", "PageUp"),
            ("pageup", "PageUp"),
            ("SPACE", "Space"),
            ("ctrl+END", "Control+End"),
            ("SHIFT+Tab", "Shift+Tab"),
            ("cmd+a", "Meta+a"),
        ] {
            let key = PageKey::parse(written).unwrap_or_else(|| panic!("{written}"));
            assert_eq!(key.chord(), canonical, "{written}");
            assert_eq!(PageKey::parse(&key.chord()), Some(key), "{written}");
        }
        // A single character still means itself, whatever its case.
        assert_eq!(PageKey::parse("A").unwrap().key, "A");
        assert_eq!(PageKey::parse("a").unwrap().key, "a");
    }
    // phase: a_bad_key_says_what_a_key_is
    {
        let unknown = PageKey::parse_detailed("Homee").unwrap_err();
        assert!(unknown.contains("\"Homee\" is not a key name"), "{unknown}");
        assert!(unknown.contains("Home"), "{unknown}");
        assert!(unknown.contains("did you mean Home?"), "{unknown}");
        let modifier = PageKey::parse_detailed("Hyper+x").unwrap_err();
        assert!(
            modifier.contains("\"Hyper\" is not a key modifier"),
            "{modifier}"
        );
        assert!(modifier.contains("Control"), "{modifier}");
        let empty = PageKey::parse_detailed("  ").unwrap_err();
        assert!(empty.contains("needs a key"), "{empty}");
        let trailing = PageKey::parse_detailed("Control+").unwrap_err();
        assert!(
            trailing.contains("ends before it names a key"),
            "{trailing}"
        );
        let twice = PageKey::parse_detailed("Shift+Shift+a").unwrap_err();
        assert!(twice.contains("held twice"), "{twice}");
    }
    // phase: invalid_requests_are_refused_before_the_engine
    {
        let element = "d1e2".to_string();
        assert!(
            PageRequest::Outline {
                from: None,
                frame: None,
            }
            .valid()
        );
        assert!(
            PageRequest::Fill {
                element: element.clone(),
                text: "hello".into(),
                secret: false,
                frame: None,
            }
            .valid()
        );
        assert!(
            !PageRequest::Fill {
                element: element.clone(),
                text: "x".repeat(MAX_FILL_BYTES + 1),
                secret: false,
                frame: None,
            }
            .valid()
        );
        assert!(
            !PageRequest::Select {
                element: element.clone(),
                values: Vec::new(),
                frame: None,
            }
            .valid()
        );
        assert!(
            !PageRequest::Click {
                element: "#submit".into(),
                frame: None,
            }
            .valid()
        );
        // A chosen file is an absolute path, so a relative one never reaches
        // the engine to resolve against the browser's own working folder.
        assert!(
            PageRequest::FillFiles {
                element: element.clone(),
                paths: vec!["/var/home/you/scan.pdf".into()],
                frame: None,
            }
            .valid()
        );
        for paths in [
            Vec::new(),
            vec!["scan.pdf".into()],
            vec!["./scan.pdf".into()],
            vec![format!("/{}", "x".repeat(MAX_FILE_PATH_BYTES))],
            (0..=MAX_FILE_PATHS)
                .map(|n| format!("/var/home/you/{n}.pdf"))
                .collect(),
        ] {
            assert!(
                !PageRequest::FillFiles {
                    element: element.clone(),
                    paths,
                    frame: None,
                }
                .valid()
            );
        }
    }
    // phase: pointer_gestures_are_bounded_and_round_trip_to_the_wire
    {
        let inside = |kind, element: Option<&str>, x, y| PointerStep {
            kind,
            at: PointerPoint::InElement { x, y },
            element: element.map(str::to_owned),
            frame: None,
        };
        let gesture = |steps: Vec<PointerStep>, button| PageRequest::Pointer { steps, button };
        let drag = gesture(
            vec![
                inside(PointerStepKind::Down, Some("d1e2"), 4.0, 4.0),
                inside(PointerStepKind::Move, Some("d1e3"), 4.0, 4.0),
                PointerStep {
                    kind: PointerStepKind::Up,
                    at: PointerPoint::Viewport { x: 40.0, y: 90.0 },
                    element: None,
                    frame: None,
                },
            ],
            PointerButton::Left,
        );
        assert!(drag.valid());
        assert_eq!(drag.element(), None, "a gesture addresses many elements");
        assert_eq!(
            encode_request(13, None, &drag),
            serde_json::json!({"op":"page","request_id":13,"action":"pointer","button":"left",
                "steps":[
                    {"kind":"down","x":4.0,"y":4.0,"element":"d1e2"},
                    {"kind":"move","x":4.0,"y":4.0,"element":"d1e3"},
                    {"kind":"up","x":40.0,"y":90.0}]})
        );
        // A step naming no element is a viewport point, and it is
        // written without the key rather than with a null.
        assert!(
            encode_request(14, None, &drag)["steps"][2]
                .get("element")
                .is_none()
        );
        let right = gesture(
            vec![PointerStep {
                kind: PointerStepKind::Down,
                at: PointerPoint::Viewport { x: 1.0, y: 2.0 },
                element: None,
                frame: None,
            }],
            PointerButton::Right,
        );
        assert_eq!(encode_request(15, None, &right)["button"], "right");
        for bad in [
            // A gesture is a movement of the hand, never empty.
            gesture(Vec::new(), PointerButton::Left),
            // A step the engine cannot place.
            gesture(
                vec![PointerStep {
                    kind: PointerStepKind::Down,
                    at: PointerPoint::InElement {
                        x: f64::INFINITY,
                        y: 0.0,
                    },
                    element: None,
                    frame: None,
                }],
                PointerButton::Left,
            ),
            gesture(
                vec![PointerStep {
                    kind: PointerStepKind::Down,
                    at: PointerPoint::Viewport {
                        x: 0.0,
                        y: MAX_COORDINATE + 1.0,
                    },
                    element: None,
                    frame: None,
                }],
                PointerButton::Left,
            ),
            // A step that names something other than an element ref.
            gesture(
                vec![inside(PointerStepKind::Move, Some("#menu"), 0.0, 0.0)],
                PointerButton::Left,
            ),
            // More steps than any gesture needs.
            gesture(
                (0..=MAX_POINTER_STEPS)
                    .map(|_| inside(PointerStepKind::Move, None, 0.0, 0.0))
                    .collect(),
                PointerButton::Left,
            ),
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
        assert_eq!(PointerButton::parse("right"), PointerButton::Right);
        assert_eq!(
            PointerButton::parse("middle"),
            PointerButton::Left,
            "an unknown button is the one a hand uses"
        );
    }
    // phase: a_scroll_says_one_place_and_one_way_there
    {
        let scroll = |element: Option<&str>, to, by, into_view: Option<&str>| PageRequest::Scroll {
            element: element.map(str::to_owned),
            to,
            by,
            into_view: into_view.map(str::to_owned),
            frame: None,
        };
        // The wheel turns over the viewport, or over the box the caller
        // names, and it turns until one place is reached.
        for good in [
            scroll(None, None, Some(600), None),
            scroll(None, None, Some(-600), None),
            scroll(None, Some(ScrollTarget::Top), None, None),
            scroll(None, Some(ScrollTarget::Bottom), None, None),
            scroll(Some("d1e2"), None, Some(4000), None),
            scroll(Some("d1e2"), Some(ScrollTarget::Bottom), None, None),
            scroll(None, None, None, Some("d1e7")),
            scroll(Some("d1e2"), None, None, Some("d1e7")),
        ] {
            assert!(good.valid(), "{good:?}");
        }
        // The most a scroll may go by, and a step past it either way.
        let furthest = i32::try_from(MAX_SCROLL_BY).unwrap();
        for bad in [
            // A scroll goes somewhere.
            scroll(None, None, None, None),
            // Two places at once.
            scroll(None, Some(ScrollTarget::Top), Some(400), None),
            scroll(None, Some(ScrollTarget::Top), None, Some("d1e7")),
            // A distance that is not bounded, or is nothing at all.
            scroll(None, None, Some(furthest + 1), None),
            scroll(None, None, Some(-furthest - 1), None),
            // Something that is not an element ref.
            scroll(Some("#menu"), None, Some(400), None),
            scroll(None, None, None, Some("d1e2;alert(1)")),
            scroll(None, None, None, Some(&"d1e".repeat(20))),
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
        assert_eq!(ScrollTarget::parse("bottom"), Some(ScrollTarget::Bottom));
        assert_eq!(ScrollTarget::parse("top"), Some(ScrollTarget::Top));
        assert_eq!(ScrollTarget::parse("sideways"), None);
        let command = encode_request(
            9,
            None,
            &scroll(None, Some(ScrollTarget::Bottom), None, None),
        );
        assert_eq!(
            command,
            serde_json::json!({"op":"page","request_id":9,"action":"scroll","to":"bottom"})
        );
        let command = encode_request(10, None, &scroll(Some("d1e2"), None, Some(-400), None));
        assert_eq!(
            command,
            serde_json::json!({"op":"page","request_id":10,"action":"scroll","element":"d1e2",
                "by":-400})
        );
        // An element to bring into view travels as its ref, as every
        // other request's element does.
        let command = encode_request(11, None, &scroll(None, None, None, Some("d1e7")));
        assert_eq!(
            command,
            serde_json::json!({"op":"page","request_id":11,"action":"scroll",
                "element":"d1e7","into_view":"d1e7"})
        );
        // A scroll in a frame names the frame the outline printed.
        let framed = PageRequest::Scroll {
            element: None,
            to: Some(ScrollTarget::Top),
            by: None,
            into_view: None,
            frame: Some(FrameId::parse("1.2").unwrap()),
        };
        assert!(framed.valid());
        assert_eq!(
            encode_request(12, None, &framed)["frame"],
            serde_json::json!("1.2")
        );
    }
    // phase: wire_encoding_names_every_field
    {
        let command = encode_request(
            7,
            None,
            &PageRequest::Press {
                element: Some("d1e2".into()),
                key: PageKey::parse("Shift+Enter").unwrap(),
                frame: None,
            },
        );
        assert_eq!(
            command,
            serde_json::json!({"op":"page","request_id":7,"action":"press","element":"d1e2",
                "key":"Enter","ctrl":false,"shift":true,"alt":false,"meta":false})
        );
        let command = encode_request(
            8,
            None,
            &PageRequest::Select {
                element: "d1e3".into(),
                values: vec!["NL".into()],
                frame: None,
            },
        );
        assert_eq!(command["values"], serde_json::json!(["NL"]));
    }
    // phase: a_frame_is_named_only_as_the_outline_prints_it
    {
        for good in ["1", "2", "12", "1.2", "3.10.4"] {
            assert_eq!(FrameId::parse(good).unwrap().as_str(), good);
        }
        // A frame is the number the outline printed, in every form a
        // caller copies it in: the number, its text, and the whole
        // `[frame=1]` the outline writes.
        for (written, named) in [
            ("1", "1"),
            (" 1", "1"),
            ("1 ", "1"),
            ("frame=1", "1"),
            ("frame=1.2", "1.2"),
            ("[frame=1]", "1"),
            ("[frame=2.1]", "2.1"),
        ] {
            assert_eq!(
                FrameId::parse(written).unwrap().as_str(),
                named,
                "{written:?}"
            );
        }
        for bad in [
            "", "0", "01", ".", "1.", ".1", "1..2", "a", "1e2", "1234", "frame=", "frame=a",
            "[frame=]", "frame=01",
        ] {
            assert!(FrameId::parse(bad).is_none(), "{bad:?}");
        }
        let deep = (1..=9).map(|n| n.to_string()).collect::<Vec<_>>().join(".");
        assert!(FrameId::parse(&deep).is_none());
        let frame = FrameId::parse("1.2").unwrap();
        let in_frame = PageRequest::Click {
            element: "d4e9".into(),
            frame: Some(frame.clone()),
        };
        assert!(in_frame.valid());
        assert_eq!(in_frame.frame(), Some(&frame));
        assert_eq!(in_frame.clone().main_frame().frame(), None);
        assert_eq!(
            encode_request(13, None, &in_frame)["frame"],
            serde_json::json!("1.2")
        );
        assert_eq!(
            encode_request(
                13,
                None,
                &PageRequest::Outline {
                    from: None,
                    frame: None,
                }
            )["frame"],
            serde_json::Value::Null
        );
        // A frame id the caller made up never reaches the engine as the
        // main document.
        let invented = PageRequest::Outline {
            from: None,
            frame: Some(FrameId::malformed()),
        };
        assert!(!invented.valid());
        assert!(invented.main_frame().valid());
    }
    // phase: agent_requests_name_their_arguments_and_bounds
    {
        let at = encode_request(
            9,
            None,
            &PageRequest::At {
                x: 12.5,
                y: 40.0,
                frame: None,
            },
        );
        assert_eq!(
            at,
            serde_json::json!({"op":"page","request_id":9,"action":"at","x":12.5,"y":40.0})
        );
        let settle = PageRequest::Settle {
            quiet_ms: 500,
            timeout_ms: 10_000,
            text: None,
            gone: None,
            frame: None,
        };
        assert!(settle.valid());
        assert_eq!(encode_request(10, None, &settle)["timeout_ms"], 10_000);
        // A wait without words is the command it always was.
        assert_eq!(
            encode_request(10, None, &settle),
            serde_json::json!({"op":"page","request_id":10,"action":"settle","quiet_ms":500,"timeout_ms":10_000})
        );
        // The words to wait for, and the words to see gone, travel as given.
        let words = PageRequest::Settle {
            quiet_ms: 500,
            timeout_ms: 8000,
            text: Some("Hello World!".into()),
            gone: Some("Loading...".into()),
            frame: None,
        };
        assert!(words.valid() && words.only_observes());
        assert_eq!(
            encode_request(13, None, &words),
            serde_json::json!({"op":"page","request_id":13,"action":"settle","quiet_ms":500,
                "timeout_ms":8000,"text":"Hello World!","gone":"Loading..."})
        );
        assert_eq!(
            words.described(),
            "Waited for the page to say \"Hello World!\" and stop saying \"Loading...\" (up to 8000 ms)"
        );
        assert!(
            PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: 8000,
                text: None,
                gone: Some("x".repeat(MAX_SEARCH_UNITS)),
                frame: None,
            }
            .valid()
        );
        assert_eq!(
            encode_request(11, None, &PageRequest::Network { since: 42 })["since"],
            42
        );
        assert_eq!(
            encode_request(
                12,
                None,
                &PageRequest::Submit {
                    element: "d1e2".into(),
                    cross_origin: true,
                    frame: None,
                }
            )["cross_origin"],
            true
        );
        for bad in [
            PageRequest::At {
                x: -1.0,
                y: 0.0,
                frame: None,
            },
            PageRequest::At {
                x: f64::NAN,
                y: 0.0,
                frame: None,
            },
            PageRequest::Settle {
                quiet_ms: 10,
                timeout_ms: 1000,
                text: None,
                gone: None,
                frame: None,
            },
            PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: 100,
                text: None,
                gone: None,
                frame: None,
            },
            PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: MAX_SETTLE_MS + 1,
                text: None,
                gone: None,
                frame: None,
            },
            // Words are bounded as a search's are, and never empty.
            PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: 1000,
                text: Some(String::new()),
                gone: None,
                frame: None,
            },
            PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: 1000,
                text: None,
                gone: Some("x".repeat(MAX_SEARCH_UNITS + 1)),
                frame: None,
            },
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
        assert!(PageRequest::Changes { frame: None }.element().is_none());
    }
    // phase: a_find_names_an_element_by_what_a_page_says_it_is
    {
        let by_name = PageRequest::Find {
            role: None,
            name: Some("Email".into()),
            nth: None,
            frame: None,
            frames: false,
        };
        assert!(by_name.valid());
        assert_eq!(by_name.action(), "find");
        assert_eq!(by_name.element(), None, "a find names no ref of its own");
        assert_eq!(
            encode_request(20, None, &by_name),
            serde_json::json!({"op":"page","request_id":20,"action":"find","name":"Email"})
        );
        let by_role = PageRequest::Find {
            role: Some("button".into()),
            name: Some("Remove".into()),
            nth: Some(2),
            frame: FrameId::parse("1.2"),
            frames: false,
        };
        assert!(by_role.valid());
        assert_eq!(
            encode_request(21, None, &by_role),
            serde_json::json!({"op":"page","request_id":21,"action":"find",
                "role":"button","name":"Remove","nth":2,"frame":"1.2"})
        );
        assert_eq!(by_role.clone().main_frame().frame(), None);
        assert_eq!(
            by_role.described(),
            "Found button \"Remove\" (2)",
            "an answer that a dialog cut short says what was asked"
        );
        // A description that names nothing, or names it outside the
        // bounds the engine accepts, never reaches the page.
        for bad in [
            PageRequest::Find {
                role: None,
                name: None,
                nth: None,
                frame: None,
                frames: false,
            },
            PageRequest::Find {
                role: Some(String::new()),
                name: None,
                nth: None,
                frame: None,
                frames: false,
            },
            PageRequest::Find {
                role: None,
                name: Some(String::new()),
                nth: None,
                frame: None,
                frames: false,
            },
            PageRequest::Find {
                role: None,
                name: Some("x".repeat(MAX_FIND_NAME_BYTES + 1)),
                nth: None,
                frame: None,
                frames: false,
            },
            PageRequest::Find {
                role: Some("r".repeat(MAX_FIND_ROLE_BYTES + 1)),
                name: None,
                nth: None,
                frame: None,
                frames: false,
            },
            PageRequest::Find {
                role: Some("button".into()),
                name: None,
                nth: Some(0),
                frame: None,
                frames: false,
            },
            PageRequest::Find {
                role: Some("button".into()),
                name: None,
                nth: None,
                frame: Some(FrameId::malformed()),
                frames: false,
            },
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
    }
    // phase: pointer_gestures_are_bounded_and_name_their_two_ends
    {
        let hover = PageRequest::Hover {
            element: Some("d1e4".into()),
            point: None,
        };
        assert!(hover.valid());
        assert_eq!(hover.element(), Some("d1e4"));
        assert_eq!(
            encode_request(20, None, &hover),
            serde_json::json!({"op":"page","request_id":20,"action":"hover","element":"d1e4"})
        );
        // A point without an element hovers there instead of clicks.
        let at = PageRequest::Hover {
            element: None,
            point: Some((30.0, 44.5)),
        };
        assert!(at.valid());
        assert_eq!(
            encode_request(21, None, &at),
            serde_json::json!({"op":"page","request_id":21,"action":"at","hover":true,
                "x":30.0,"y":44.5})
        );
        for bad in [
            PageRequest::Hover {
                element: None,
                point: None,
            },
            PageRequest::Hover {
                element: Some("#menu".into()),
                point: None,
            },
            PageRequest::Hover {
                element: Some("d1e4".into()),
                point: Some((1.0, 1.0)),
            },
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
        let drag = PageRequest::Drag {
            from: Some("d1e4".into()),
            from_point: None,
            to: Some("d1e6".into()),
            to_point: None,
            steps: 8,
        };
        assert!(drag.valid());
        assert_eq!(
            encode_request(22, None, &drag),
            serde_json::json!({"op":"page","request_id":22,"action":"drag","element":"d1e4",
                "to":"d1e6","steps":8})
        );
        let by_point = PageRequest::Drag {
            from: None,
            from_point: Some((10.0, 20.0)),
            to: None,
            to_point: Some((70.0, 80.0)),
            steps: 4,
        };
        assert!(by_point.valid());
        assert_eq!(
            encode_request(23, None, &by_point),
            serde_json::json!({"op":"page","request_id":23,"action":"drag","x":10.0,"y":20.0,
                "to_x":70.0,"to_y":80.0,"steps":4})
        );
        for bad in [
            PageRequest::Drag {
                from: None,
                from_point: None,
                to: Some("d1e6".into()),
                to_point: None,
                steps: 4,
            },
            PageRequest::Drag {
                from: Some("d1e4".into()),
                from_point: None,
                to: None,
                to_point: None,
                steps: 4,
            },
            PageRequest::Drag {
                from: Some("d1e4".into()),
                from_point: None,
                to: Some("d1e6".into()),
                to_point: None,
                steps: 0,
            },
            PageRequest::Drag {
                from: Some("d1e4".into()),
                from_point: None,
                to: Some("d1e6".into()),
                to_point: None,
                steps: 65,
            },
            PageRequest::Drag {
                from: None,
                from_point: Some((f64::INFINITY, 0.0)),
                to: Some("d1e6".into()),
                to_point: None,
                steps: 4,
            },
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
        let path = PageRequest::Path {
            element: Some("d1e9".into()),
            points: vec![(1.0, 2.0), (3.5, 4.5), (60.0, 70.0)],
        };
        assert!(path.valid());
        let wire = encode_request(24, None, &path);
        assert_eq!(
            wire["points"],
            serde_json::json!([[1.0, 2.0], [3.5, 4.5], [60.0, 70.0]])
        );
        assert_eq!(wire["element"], "d1e9");
        let free = PageRequest::Path {
            element: None,
            points: vec![(1.0, 2.0), (3.0, 4.0)],
        };
        assert!(free.valid());
        assert!(encode_request(25, None, &free).get("element").is_none());
        for bad in [
            PageRequest::Path {
                element: Some("d1e9".into()),
                points: vec![(1.0, 2.0)],
            },
            PageRequest::Path {
                element: Some("d1e9".into()),
                points: vec![(1.0, 2.0); MAX_PATH_POINTS + 1],
            },
            PageRequest::Path {
                element: None,
                points: vec![(-1.0, 2.0), (3.0, 4.0)],
            },
            PageRequest::Path {
                element: Some("d1e9".into()),
                points: vec![(f64::NAN, 2.0), (3.0, 4.0)],
            },
        ] {
            assert!(!bad.valid(), "{bad:?}");
        }
    }
    // phase: findings_decode_as_data_within_their_bound
    {
        let reply = |data: serde_json::Value| {
            serde_json::json!({"request_id":3,"ok":true,"message":"2 requests","url":"https://a.test/",
                "navigated":false,"loading":false,"outline":null,"data":data})
        };
        let (_, result) = decode_result(&reply(
            serde_json::json!({"entries":[{"url":"https://a.test/x"}]}),
        ))
        .expect("decodes");
        assert_eq!(
            result.expect("succeeded").data,
            Some(serde_json::json!({"entries":[{"url":"https://a.test/x"}]}))
        );
        assert!(decode_result(&reply(serde_json::json!("not an object"))).is_none());
        let huge = "x".repeat(MAX_DATA_BYTES);
        assert!(decode_result(&reply(serde_json::json!({"text": huge}))).is_none());
    }
    // phase: results_decode_within_bounds
    {
        let ok = serde_json::json!({"event":"page_result","request_id":3,"ok":true,
            "url":"https://a.test/","message":"Read","outline":"- button \"Go\" [ref=d1e1]","navigated":false,"loading":false});
        let (id, reply) = decode_result(&ok).unwrap();
        assert_eq!(id, 3);
        let reply = reply.unwrap();
        assert_eq!(reply.outline.as_deref(), Some("- button \"Go\" [ref=d1e1]"));
        assert_eq!(reply.navigation, PageNavigation::Possible);
        let read = serde_json::json!({"event":"page_result","request_id":3,"ok":true,
            "url":"https://a.test/","message":"Read","outline":null,"navigation":"none","navigated":false,"loading":false});
        assert_eq!(
            decode_result(&read).unwrap().1.unwrap().navigation,
            PageNavigation::None
        );
        let contradicts = serde_json::json!({"event":"page_result","request_id":3,"ok":true,
            "url":"https://a.test/","message":"Read","outline":null,"navigation":"started","navigated":false,"loading":false});
        assert!(decode_result(&contradicts).is_none());
        let failed = serde_json::json!({"event":"page_result","request_id":4,"ok":false,
            "code":"stale_element","message":"Read the page again"});
        assert_eq!(
            decode_result(&failed).unwrap().1,
            Err(PageFailure::new("stale_element", "Read the page again"))
        );
        let oversized = serde_json::json!({"event":"page_result","request_id":5,"ok":true,
            "url":"https://a.test/","message":"Read","outline":"x".repeat(MAX_OUTLINE_BYTES + 1),"navigated":false,"loading":false});
        assert!(decode_result(&oversized).is_none());
        let anonymous = serde_json::json!({"event":"page_result","request_id":0,"ok":false,
            "code":"busy","message":""});
        assert!(decode_result(&anonymous).is_none());
    }
}

/// Several actors work in one tab: a request says who asks, the tab's
/// record and its fields are requests like any other, and an answer says
/// where its action stands and what the others did.
#[test]
fn several_actors_in_one_tab_are_named_recorded_and_told_of_each_other() {
    // phase: an_actor_is_the_admitted_principal_within_the_engines_bound
    {
        assert_eq!(PageActor::named(""), None, "nobody is the unnamed actor");
        assert_eq!(PageActor::named("agent:ada").unwrap().as_str(), "agent:ada");
        let longest = "p".repeat(MAX_ACTOR_UNITS);
        assert_eq!(PageActor::named(&longest).unwrap().as_str(), longest);
        // A longer principal is shortened the same way every time, and
        // two that begin alike still get two names.
        let ada = format!("agent:{}:ada", "x".repeat(80));
        let ben = format!("agent:{}:ben", "x".repeat(80));
        let short = PageActor::named(&ada).unwrap();
        assert_eq!(short, PageActor::named(&ada).unwrap());
        assert_ne!(short, PageActor::named(&ben).unwrap());
        assert_eq!(short.as_str().encode_utf16().count(), MAX_ACTOR_UNITS);
        assert!(
            short.as_str().starts_with(&ada[..ACTOR_HEAD_UNITS]),
            "{short}"
        );
        assert_eq!(short.as_str().as_bytes()[ACTOR_HEAD_UNITS], b'~');
        // The engine counts UTF-16 units, and a name never ends inside a
        // character: one outside the basic plane counts two.
        let wide = "\u{1F600}".repeat(40);
        let name = PageActor::named(&wide).unwrap();
        assert!(name.as_str().encode_utf16().count() <= MAX_ACTOR_UNITS);
        assert!(name.as_str().starts_with(&"\u{1F600}".repeat(15)));
    }
    // phase: the_actor_travels_with_the_request_and_is_absent_without_one
    {
        let read = PageRequest::Changes { frame: None };
        let ada = PageActor::named("agent:ada");
        assert_eq!(
            encode_request(3, ada.as_ref(), &read),
            serde_json::json!({"op":"page","request_id":3,"action":"changes",
                "actor":"agent:ada"})
        );
        assert!(encode_request(3, None, &read).get("actor").is_none());
    }
    // phase: the_record_and_the_fields_are_reads_of_their_own
    {
        let record = PageRequest::Ledger { since: 12 };
        assert!(record.valid() && record.only_observes());
        assert_eq!(record.element(), None);
        assert_eq!(record.frame(), None);
        assert_eq!(record.described(), "Read who did what in the tab");
        assert_eq!(
            encode_request(4, None, &record),
            serde_json::json!({"op":"page","request_id":4,"action":"ledger","since":12})
        );
        let fields = PageRequest::Fields { frame: None };
        assert!(fields.valid() && fields.only_observes());
        assert_eq!(fields.described(), "Read the page's fields");
        assert_eq!(
            encode_request(5, None, &fields),
            serde_json::json!({"op":"page","request_id":5,"action":"fields"})
        );
        let framed = PageRequest::Fields {
            frame: FrameId::parse("1.2"),
        };
        assert!(framed.valid());
        assert_eq!(
            encode_request(6, PageActor::named("rev").as_ref(), &framed),
            serde_json::json!({"op":"page","request_id":6,"action":"fields",
                "frame":"1.2","actor":"rev"})
        );
        assert_eq!(framed.clone().main_frame(), fields);
        let invented = PageRequest::Fields {
            frame: Some(FrameId::malformed()),
        };
        assert!(!invented.valid() && !invented.frame_valid());
    }
    // phase: an_answer_says_its_place_and_what_the_others_did
    {
        let answer = |extra: serde_json::Value| {
            let mut answer = serde_json::json!({"event":"page_result","request_id":7,
                "ok":true,"url":"https://a.test/form","message":"Filled","outline":null,
                "navigation":"possible","navigated":false,"loading":false});
            for (key, value) in extra.as_object().unwrap() {
                answer[key.as_str()] = value.clone();
            }
            answer
        };
        let quiet = decode_result(&answer(serde_json::json!({})))
            .unwrap()
            .1
            .unwrap();
        assert!(quiet.record.is_empty());
        let theirs = serde_json::json!([
            {"seq":1.0,"actor":"ben","action":"fill","ok":true,
                "target":{"role":"textbox","name":"Email"},"text":"ben@example.org"},
            {"seq":2.0,"actor":"ben","action":"click","ok":true},
        ]);
        // The engine counts the record in doubles: a place is whole
        // however it is written.
        let told = decode_result(&answer(serde_json::json!({
            "seq": 3.0, "others": theirs, "others_earlier": 4,
        })))
        .unwrap()
        .1
        .unwrap();
        assert_eq!(told.record.seq, Some(3));
        assert_eq!(told.record.others, theirs.as_array().unwrap().clone());
        assert_eq!(told.record.others_earlier, 4);
        assert!(!told.record.is_empty());
        // A refused action has its place too, and is told the same.
        let refused = serde_json::json!({"event":"page_result","request_id":8,"ok":false,
            "code":"stale_element","message":"Read the page again","seq":5,
            "others":[{"seq":4,"actor":"ben","action":"click","ok":true}]});
        let failure = decode_result(&refused).unwrap().1.unwrap_err();
        assert_eq!(failure.code, "stale_element");
        assert_eq!(failure.record.seq, Some(5));
        assert_eq!(failure.record.others.len(), 1);
        assert_eq!(
            PageFailure::new("busy", "x").record,
            PageRecordNote::default()
        );
        // More than an engine tells, or something that is no record, is
        // not an answer.
        let entry = serde_json::json!({"seq":1,"actor":"ben","action":"click","ok":true});
        for (label, bad) in [
            (
                "more entries than one answer carries",
                serde_json::json!({"others": vec![entry.clone(); MAX_OTHERS + 1]}),
            ),
            (
                "an entry that is no object",
                serde_json::json!({"others": ["ben clicked"]}),
            ),
            (
                "others that are no list",
                serde_json::json!({"others": "ben"}),
            ),
            ("an empty list of others", serde_json::json!({"others": []})),
            (
                "entries larger than findings may be",
                serde_json::json!({"others": [{"text": "x".repeat(MAX_DATA_BYTES)}]}),
            ),
            (
                "earlier ones beside none that are told",
                serde_json::json!({"others_earlier": 2}),
            ),
            (
                "a count that is no whole number",
                serde_json::json!({"others": [entry.clone()], "others_earlier": 1.5}),
            ),
            ("a place before the first", serde_json::json!({"seq": 0})),
            ("a place that is no number", serde_json::json!({"seq": "3"})),
            ("a place that is not whole", serde_json::json!({"seq": 2.5})),
        ] {
            assert!(decode_result(&answer(bad)).is_none(), "{label}");
        }
    }
}

/// A long page is read on from a line, its words are found by searching,
/// and a form is answered by the names it asks under: each a request
/// within the bounds an engine accepts, and nothing outside them.
#[test]
fn a_page_is_read_on_searched_and_its_forms_answered_by_name() {
    let answers = |value: serde_json::Value| value.as_object().unwrap().clone();
    // phase: reading_on_names_the_line_to_start_at
    {
        let on = PageRequest::Outline {
            from: Some(1240),
            frame: None,
        };
        assert!(on.valid() && on.only_observes());
        assert_eq!(
            encode_request(3, None, &on),
            serde_json::json!({"op":"page","request_id":3,"action":"outline","from":1240})
        );
        let top = PageRequest::Outline {
            from: None,
            frame: None,
        };
        assert!(
            encode_request(3, None, &top).get("from").is_none(),
            "a read from the top names no line"
        );
        let framed = PageRequest::Outline {
            from: Some(2),
            frame: FrameId::parse("1.2"),
        };
        assert_eq!(
            encode_request(4, None, &framed),
            serde_json::json!({"op":"page","request_id":4,"action":"outline","from":2,
                "frame":"1.2"})
        );
        assert_eq!(
            framed.main_frame(),
            PageRequest::Outline {
                from: Some(2),
                frame: None,
            },
            "the line is kept when the frame is not"
        );
        for from in [1, MAX_OUTLINE_FROM] {
            assert!(
                PageRequest::Outline {
                    from: Some(from),
                    frame: None,
                }
                .valid(),
                "{from}"
            );
        }
        for from in [0, MAX_OUTLINE_FROM + 1, u32::MAX] {
            assert!(
                !PageRequest::Outline {
                    from: Some(from),
                    frame: None,
                }
                .valid(),
                "{from}"
            );
        }
    }
    // phase: words_are_found_by_searching_for_them
    {
        let search = |text: &str| PageRequest::Search {
            text: text.into(),
            frame: None,
            frames: false,
        };
        let words = search("within 30 days");
        assert!(words.valid() && words.only_observes());
        assert_eq!(words.action(), "search");
        assert_eq!(words.element(), None);
        assert_eq!(
            words.described(),
            "Searched the page for \"within 30 days\""
        );
        assert_eq!(
            encode_request(5, None, &words),
            serde_json::json!({"op":"page","request_id":5,"action":"search",
                "text":"within 30 days"})
        );
        let framed = PageRequest::Search {
            text: "Total".into(),
            frame: FrameId::parse("2"),
            frames: false,
        };
        assert_eq!(
            encode_request(6, PageActor::named("agent:ada").as_ref(), &framed),
            serde_json::json!({"op":"page","request_id":6,"action":"search","text":"Total",
                "frame":"2","actor":"agent:ada"})
        );
        assert_eq!(framed.main_frame(), search("Total"));
        // The words are counted as an engine counts them, in UTF-16
        // units: a page in any script is searched within the same bound.
        for good in [
            "x".to_string(),
            "x".repeat(MAX_SEARCH_UNITS),
            "é".repeat(MAX_SEARCH_UNITS),
            "\u{1F600}".repeat(MAX_SEARCH_UNITS / 2),
        ] {
            assert!(search(&good).valid(), "{} units", units(&good));
        }
        for bad in [
            String::new(),
            "x".repeat(MAX_SEARCH_UNITS + 1),
            "\u{1F600}".repeat(MAX_SEARCH_UNITS / 2 + 1),
        ] {
            assert!(!search(&bad).valid(), "{} units", units(&bad));
        }
        assert!(
            !PageRequest::Search {
                text: "Total".into(),
                frame: Some(FrameId::malformed()),
                frames: false,
            }
            .valid()
        );
    }
    // phase: a_form_is_read_as_what_it_asks
    {
        let forms = PageRequest::Forms { frame: None };
        assert!(forms.valid() && forms.only_observes());
        assert_eq!(forms.described(), "Read the page's forms");
        assert_eq!(
            encode_request(7, None, &forms),
            serde_json::json!({"op":"page","request_id":7,"action":"forms"})
        );
        let framed = PageRequest::Forms {
            frame: FrameId::parse("1"),
        };
        assert_eq!(encode_request(8, None, &framed)["frame"], "1");
        assert_eq!(framed.main_frame(), forms);
    }
    // phase: a_form_is_answered_by_name_and_sent_one_way
    {
        let given = serde_json::json!({
            "Arrival": "2027-02-03",
            "Date of birth: Month": 11,
            "Share of costs": 0.5,
            "Refused before?": false,
            "Languages": ["English", "German"],
        });
        let fill = |submit: FormSubmit, cross_origin: bool| PageRequest::FillForm {
            form: "form1".into(),
            answers: answers(given.clone()),
            submit,
            cross_origin,
            frame: None,
        };
        let answered = fill(FormSubmit::No, false);
        assert!(answered.valid());
        assert!(
            !answered.only_observes() && !answered.enters_text(),
            "answering a form acts on the page, and never types a secret into it"
        );
        assert_eq!(answered.action(), "fill_form");
        assert_eq!((answered.element(), answered.frame()), (None, None));
        assert_eq!(answered.described(), "Answered form form1");
        // The answers travel as the object they are, each value as it
        // was given, and a form that is only answered says nothing of
        // sending.
        assert_eq!(
            encode_request(9, None, &answered),
            serde_json::json!({"op":"page","request_id":9,"action":"fill_form",
                "form":"form1","answers":given,"cross_origin":false})
        );
        // Sent with the first submit button, or with the one a label
        // names: a boolean or a text, as the engine reads it.
        let first = encode_request(10, None, &fill(FormSubmit::First, false));
        assert_eq!(first["submit"], serde_json::json!(true));
        let named = encode_request(
            11,
            PageActor::named("agent:ada").as_ref(),
            &fill(FormSubmit::Button("Send application".into()), true),
        );
        assert_eq!(named["submit"], serde_json::json!("Send application"));
        assert_eq!(named["cross_origin"], serde_json::json!(true));
        assert_eq!(named["actor"], "agent:ada");
        assert!(named["answers"].is_object());
        // A form named by its signature, with nothing but the sending.
        let by_signature = PageRequest::FillForm {
            form: "5f1c2a9be0".into(),
            answers: serde_json::Map::new(),
            submit: FormSubmit::First,
            cross_origin: false,
            frame: None,
        };
        assert!(by_signature.valid());
        // The bounds an engine accepts, each met and each passed.
        let with =
            |form: &str, answers: serde_json::Value, submit: FormSubmit| PageRequest::FillForm {
                form: form.into(),
                answers: answers.as_object().unwrap().clone(),
                submit,
                cross_origin: false,
                frame: None,
            };
        let most: serde_json::Map<String, serde_json::Value> = (0..MAX_FORM_ANSWERS)
            .map(|number| (format!("Question {number}"), serde_json::json!("x")))
            .collect();
        let mut one_more = most.clone();
        one_more.insert("One more".into(), serde_json::json!("x"));
        for good in [
            with(
                &"f".repeat(MAX_FORM_UNITS),
                serde_json::json!({}),
                FormSubmit::No,
            ),
            with("form1", most.into(), FormSubmit::No),
            with(
                "form1",
                serde_json::json!({"é".repeat(MAX_FORM_NAME_UNITS): "x"}),
                FormSubmit::No,
            ),
            with(
                "form1",
                serde_json::json!({"Notes": "x".repeat(MAX_FORM_TEXT_UNITS)}),
                FormSubmit::No,
            ),
            with(
                "form1",
                serde_json::json!({"Languages": vec!["x".repeat(MAX_FORM_CHOICE_UNITS); MAX_FORM_CHOICES]}),
                FormSubmit::No,
            ),
            with(
                "form1",
                serde_json::json!({"Languages": []}),
                FormSubmit::Button("x".repeat(MAX_FORM_NAME_UNITS)),
            ),
        ] {
            assert!(good.valid(), "{}", good.described());
        }
        for (why, bad) in [
            (
                "a form of no name",
                with("", serde_json::json!({"Arrival": "x"}), FormSubmit::No),
            ),
            (
                "a name longer than an id or a signature",
                with(
                    &"f".repeat(MAX_FORM_UNITS + 1),
                    serde_json::json!({}),
                    FormSubmit::No,
                ),
            ),
            (
                "more answers than a form takes",
                with("form1", one_more.into(), FormSubmit::No),
            ),
            (
                "an answer under no name",
                with("form1", serde_json::json!({"": "x"}), FormSubmit::No),
            ),
            (
                "a name longer than any question's",
                with(
                    "form1",
                    serde_json::json!({"x".repeat(MAX_FORM_NAME_UNITS + 1): "x"}),
                    FormSubmit::No,
                ),
            ),
            (
                "a text longer than a field takes",
                with(
                    "form1",
                    serde_json::json!({"Notes": "x".repeat(MAX_FORM_TEXT_UNITS + 1)}),
                    FormSubmit::No,
                ),
            ),
            (
                "an answer that is an object",
                with(
                    "form1",
                    serde_json::json!({"Arrival": {"day": 3}}),
                    FormSubmit::No,
                ),
            ),
            (
                "an answer that is nothing",
                with(
                    "form1",
                    serde_json::json!({"Arrival": null}),
                    FormSubmit::No,
                ),
            ),
            (
                "a choice that is no text",
                with(
                    "form1",
                    serde_json::json!({"Languages": ["English", 3]}),
                    FormSubmit::No,
                ),
            ),
            (
                "more choices than one answer makes",
                with(
                    "form1",
                    serde_json::json!({"Languages": vec!["x"; MAX_FORM_CHOICES + 1]}),
                    FormSubmit::No,
                ),
            ),
            (
                "a choice longer than any option",
                with(
                    "form1",
                    serde_json::json!({"Languages": ["x".repeat(MAX_FORM_CHOICE_UNITS + 1)]}),
                    FormSubmit::No,
                ),
            ),
            (
                "a button of no name",
                with(
                    "form1",
                    serde_json::json!({}),
                    FormSubmit::Button(String::new()),
                ),
            ),
            (
                "a label longer than any button's",
                with(
                    "form1",
                    serde_json::json!({}),
                    FormSubmit::Button("x".repeat(MAX_FORM_NAME_UNITS + 1)),
                ),
            ),
        ] {
            assert!(!bad.valid(), "{why}");
        }
    }
}

/// A picture of the page is a request like any other, and its answer's
/// pixels travel as the reply's own image: a PNG, bounded, and never a
/// finding the caller has to decode.
#[test]
fn a_capture_asks_for_a_bounded_picture_and_answers_with_its_pixels() {
    use base64::Engine as _;
    let png: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222,
    ];

    // phase: the_viewport_when_no_element_is_named
    let page = PageRequest::Capture {
        element: None,
        max_width: None,
        frame: None,
    };
    assert!(page.valid());
    assert_eq!(page.action(), "capture");
    assert_eq!(page.described(), "Pictured the page");
    assert_eq!(page.element(), None);
    assert_eq!(
        encode_request(7, None, &page),
        serde_json::json!({
            "op": "page",
            "request_id": 7,
            "action": "capture",
            "max_width": u32::from(MAX_CAPTURE_WIDTH),
        })
    );

    // phase: an_element_is_named_by_the_ref_the_outline_printed
    let element = PageRequest::Capture {
        element: Some("d1e5".into()),
        max_width: Some(400),
        frame: Some(FrameId::parse("1.2").unwrap()),
    };
    assert!(element.valid());
    assert_eq!(element.described(), "Pictured element d1e5");
    assert_eq!(
        encode_request(8, None, &element),
        serde_json::json!({
            "op": "page",
            "request_id": 8,
            "action": "capture",
            "element": "d1e5",
            "frame": "1.2",
            "max_width": 400,
        })
    );

    // phase: a_width_no_engine_paints_or_a_ref_no_outline_printed_is_refused
    for width in [0, u32::from(MAX_CAPTURE_WIDTH) + 1, 4000] {
        assert!(
            !PageRequest::Capture {
                element: None,
                max_width: Some(width),
                frame: None,
            }
            .valid(),
            "{width}"
        );
    }
    assert!(
        PageRequest::Capture {
            element: None,
            max_width: Some(u32::from(MAX_CAPTURE_WIDTH)),
            frame: None,
        }
        .valid()
    );
    assert!(
        !PageRequest::Capture {
            element: Some("not-a-ref".into()),
            max_width: None,
            frame: None,
        }
        .valid()
    );

    // phase: the_answer_carries_the_picture_as_its_own_pixels
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let answer = serde_json::json!({"event":"page_result","request_id":9,"ok":true,
        "url":"https://letters.test/scan","message":"The view","outline":null,
        "navigation":"none","navigated":false,"loading":false,
        "data":{"x":0.0,"y":0.0,"width":800.0,"height":600.0,"format":"png","capture":encoded}});
    let reply = decode_result(&answer)
        .expect("a picture is a well-formed answer")
        .1;
    let reply = reply.expect("it succeeded");
    assert_eq!(reply.image.as_deref(), Some(png));
    assert!(
        reply.data.unwrap().get("capture").is_none(),
        "the pixels are the image, never a base64 finding beside it"
    );

    // phase: what_is_not_a_png_in_named_limits_is_not_a_picture
    for (label, capture) in [
        ("not base64", "!!!".to_string()),
        (
            "another file",
            base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.7 ..."),
        ),
        (
            "larger than any engine paints",
            base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_CAPTURE_BYTES + 1]),
        ),
    ] {
        let bad = serde_json::json!({"event":"page_result","request_id":9,"ok":true,
            "url":"https://letters.test/scan","message":"The view","outline":null,
            "navigation":"none","navigated":false,"loading":false,
            "data":{"capture":capture}});
        assert!(decode_result(&bad).is_none(), "{label}");
    }
}

/// A find and a search say when they are to look in the frames the page
/// shows as well as in the page, and say nothing of it otherwise; a form is
/// answered in a frame's own document by naming the frame.
#[test]
fn a_find_and_a_search_look_in_the_frames_when_asked_and_a_form_is_answered_in_a_frame() {
    let find = PageRequest::Find {
        role: Some("button".into()),
        name: Some("Accept all".into()),
        nth: None,
        frame: None,
        frames: true,
    };
    assert!(find.valid() && find.only_observes());
    assert_eq!(
        encode_request(3, None, &find),
        serde_json::json!({"op":"page","request_id":3,"action":"find","role":"button",
            "name":"Accept all","frames":true})
    );
    let search = PageRequest::Search {
        text: "Total".into(),
        frame: None,
        frames: true,
    };
    assert!(search.valid() && search.only_observes());
    assert_eq!(
        encode_request(4, None, &search),
        serde_json::json!({"op":"page","request_id":4,"action":"search","text":"Total",
            "frames":true})
    );
    // Looking in the frames is no frame of its own: the request still runs
    // from the page's document.
    assert_eq!(find.frame(), None);
    assert_eq!(search.clone().main_frame(), search);

    let form = |frame: Option<FrameId>| PageRequest::FillForm {
        form: "form1".into(),
        answers: serde_json::Map::new(),
        submit: FormSubmit::No,
        cross_origin: false,
        frame,
    };
    let framed = form(FrameId::parse("[frame=3]"));
    assert!(framed.valid() && !framed.only_observes());
    assert_eq!(framed.frame(), FrameId::parse("3").as_ref());
    assert_eq!(
        encode_request(5, None, &framed),
        serde_json::json!({"op":"page","request_id":5,"action":"fill_form","form":"form1",
            "answers":{},"cross_origin":false,"frame":"3"})
    );
    assert_eq!(framed.main_frame(), form(None));
    assert!(
        !form(Some(FrameId::malformed())).valid(),
        "a frame nobody printed is refused, not answered in the page"
    );
}

/// What a person typed straight into the page arrives as the entry the
/// engine set down and its place, and only as that.
#[test]
fn what_a_person_typed_is_decoded_as_the_entry_the_engine_set_down() {
    let typed = serde_json::json!({"event":"page_record","tab":2,"seq":4.0,"entry":{
        "seq":4.0,"time_ms":1_700_000_000_004.0_f64,"actor":"","how":"input","ok":true,
        "target":{"role":"textbox","name":"Email","nth":1},"text":"ada@example.org",
        "message":"Direct input into textbox \"Email\"","url":"https://shop.test/form"}});
    let (seq, entry) = decode_record_event(&typed).expect("the event is what the host sends");
    assert_eq!(seq, 4);
    assert_eq!(
        entry["seq"], 4,
        "the entry names its place as a whole number"
    );
    assert_eq!(entry["how"], "input");
    assert_eq!(entry["text"], "ada@example.org");

    // An entry that leaves its place out is at the place the event names.
    let mut bare = typed.clone();
    bare["entry"].as_object_mut().unwrap().remove("seq");
    assert_eq!(
        decode_record_event(&bare).map(|(seq, entry)| (seq, entry["seq"].clone())),
        Some((4, serde_json::json!(4)))
    );

    for (why, change) in [
        ("no place", serde_json::json!({"seq": null})),
        ("the place before the first", serde_json::json!({"seq": 0})),
        ("half a place", serde_json::json!({"seq": 4.5})),
        ("no entry", serde_json::json!({"entry": null})),
        (
            "an entry that is a text",
            serde_json::json!({"entry": "typed"}),
        ),
        (
            "an entry of another place",
            serde_json::json!({"entry": {"seq": 5, "how": "input"}}),
        ),
        (
            "an entry longer than an answer's findings",
            serde_json::json!({"entry": {"seq": 4, "text": "x".repeat(MAX_DATA_BYTES)}}),
        ),
    ] {
        let mut event = typed.clone();
        event
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        assert!(decode_record_event(&event).is_none(), "{why}");
    }
}

/// What the page changed of itself arrives as which document, which forms
/// and which fields, and never as what a field holds.
#[test]
fn what_a_page_changed_of_itself_is_decoded_within_its_bounds() {
    let told = serde_json::json!({"event":"page_changed","tab":2,"document":7.0,
        "forms":[
            {"form":"form1","signature":"0badc0de","offers":"5f1c2a9b"},
            {"form":"form2","gone":true},
        ],
        "fields":[
            {"element":"d7e4","role":"textbox","name":"Arrival","how":"page",
                "form":"form1","question":"Arrival"},
            {"element":"d7e9","role":"searchbox","name":"Search","how":"input"},
        ],
        "more":3});
    assert_eq!(
        decode_changed_event(&told),
        Some(PageChange {
            document: 7,
            frame: None,
            forms: vec![
                PageFormChange {
                    form: "form1".into(),
                    signature: Some("0badc0de".into()),
                    offers: Some("5f1c2a9b".into()),
                    gone: false,
                },
                PageFormChange {
                    form: "form2".into(),
                    signature: None,
                    offers: None,
                    gone: true,
                },
            ],
            fields: vec![
                PageFieldChange {
                    element: "d7e4".into(),
                    role: "textbox".into(),
                    name: "Arrival".into(),
                    typed: false,
                    form: Some("form1".into()),
                    question: Some("Arrival".into()),
                },
                PageFieldChange {
                    element: "d7e9".into(),
                    role: "searchbox".into(),
                    name: "Search".into(),
                    typed: true,
                    form: None,
                    question: None,
                },
            ],
            more: 3,
        })
    );

    // A change in a frame says the frame by the number the tab calls it by.
    let mut framed = told.clone();
    framed["frame"] = "3".into();
    framed["frame_name"] = "Payment".into();
    framed["frame_url"] = "https://pay.test/card".into();
    assert_eq!(
        decode_changed_event(&framed).and_then(|change| change.frame),
        FrameId::parse("3")
    );
    // Forms alone, or fields alone, are a change too.
    let mut forms_only = told.clone();
    forms_only.as_object_mut().unwrap().remove("fields");
    forms_only.as_object_mut().unwrap().remove("more");
    assert!(
        decode_changed_event(&forms_only).is_some_and(|change| change.fields.is_empty()
            && change.more == 0
            && change.forms.len() == 2)
    );

    for (why, pointer, value) in [
        ("no document", "/document", serde_json::json!(null)),
        ("document 0", "/document", serde_json::json!(0)),
        (
            "a frame no outline printed",
            "/frame",
            serde_json::json!("main; drop"),
        ),
        ("a frame that is no text", "/frame", serde_json::json!(3)),
        (
            "forms that are no list",
            "/forms",
            serde_json::json!("form1"),
        ),
        (
            "a form without an id",
            "/forms/0/form",
            serde_json::json!(""),
        ),
        (
            "a form gone by a word",
            "/forms/1/gone",
            serde_json::json!("yes"),
        ),
        (
            "a field whose ref is none",
            "/fields/0/element",
            serde_json::json!("#arrival"),
        ),
        (
            "a field changed by nobody known",
            "/fields/0/how",
            serde_json::json!("action"),
        ),
        (
            "a field's name that is no text",
            "/fields/1/name",
            serde_json::json!(7),
        ),
        (
            "a count that is no number",
            "/more",
            serde_json::json!("many"),
        ),
    ] {
        let mut event = told.clone();
        if pointer == "/frame" {
            event["frame"] = value;
        } else {
            *event.pointer_mut(pointer).expect(pointer) = value;
        }
        assert!(decode_changed_event(&event).is_none(), "{why}");
    }
    let mut many = told.clone();
    many["fields"] =
        serde_json::Value::Array(vec![told["fields"][0].clone(); MAX_CHANGED_FIELDS + 1]);
    assert!(
        decode_changed_event(&many).is_none(),
        "more fields than one event names"
    );
}
