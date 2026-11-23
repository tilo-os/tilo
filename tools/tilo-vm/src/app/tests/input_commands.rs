//! `tilo-vm input` from the parsed command line to the actions it becomes: bytes from standard
//! input, the pointer, the touchscreen, the pen and gamepads, each validated before use and
//! reported in words that keep nothing private.

use super::*;

tilo_test_env::scenario! {
    fn input_commands_become_validated_actions;

    phase stdin_input_preserves_bytes_and_redacts_failures_and_receipts;
    {
        use crate::cli::Cli;
        use clap::Parser as _;
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("private-reader-error"))
            }
        }
        let argv = ["tilo-vm", "input", "type", "--stdin"];
        let cli = Cli::try_parse_from(argv).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Input {
                action: InputCommand::Type {
                    text: None,
                    stdin: true
                },
                ..
            })
        ));
        for words in [
            vec!["tilo-vm", "input", "type"],
            vec!["tilo-vm", "input", "type", "value", "--stdin"],
        ] {
            assert!(Cli::try_parse_from(words).is_err());
        }
        let marker = "synthetic-private-input\n";
        let actions = stdin_input_actions(marker.as_bytes()).unwrap();
        assert_eq!(
            actions,
            vec![InputAction::Type {
                text: marker.into()
            }]
        );
        assert_eq!(describe_input(&actions[0]), "typed 24 characters");
        let oversized = vec![b'a'; 65_537];
        assert!(
            stdin_input_actions(&oversized[..])
                .unwrap_err()
                .to_string()
                .contains("65536")
        );
        assert!(stdin_input_actions(&vec![b'a'; 65_536][..]).is_ok());
        for invalid in [b"private-\xff".as_slice(), "private-é".as_bytes()] {
            let error = stdin_input_actions(invalid).unwrap_err().to_string();
            assert!(!error.contains("private"));
            assert!(!error.contains('é'));
        }
        assert!(
            !stdin_input_actions(Broken)
                .unwrap_err()
                .to_string()
                .contains("private")
        );
        let dir = tempfile::tempdir().unwrap();
        let receipt = Receipt::new(dir.path(), argv.iter().map(|s| (*s).into()).collect());
        let path = receipt
            .finish(Ok(&json!({"description": describe_input(&actions[0])})))
            .unwrap();
        let saved = std::fs::read_to_string(path).unwrap();
        assert!(!saved.contains("synthetic-private-input"));
        assert!(saved.contains("--stdin"));
    }

    phase device_profile_and_touchpad_commands_become_validated_actions;
    {
        let plug = input_actions(InputCommand::Device(DeviceCommand::Plug {
            profile: "lenovo-83ed-touchpad".into(),
        }))
        .unwrap();
        assert_eq!(
            plug,
            [InputAction::DevicePlug {
                profile: "lenovo-83ed-touchpad".into()
            }]
        );
        let unknown = input_actions(InputCommand::Device(DeviceCommand::Plug {
            profile: "nope".into(),
        }));
        assert!(
            matches!(unknown, Err(CliError::InvalidInput(m)) if m.contains("lenovo-83ed-keyboard"))
        );
        let swipe = input_actions(InputCommand::Touchpad(TouchpadCommand::Swipe {
            dx: 0,
            dy: -30,
            fingers: 2,
            duration: 300,
            press: false,
        }))
        .unwrap();
        assert_eq!(
            swipe,
            [InputAction::TouchpadSwipe {
                fingers: 2,
                dx_mm: 0,
                dy_mm: -30,
                duration_ms: 300,
                press: false,
            }]
        );
        assert!(
            input_actions(InputCommand::Touchpad(TouchpadCommand::Tap { fingers: 5 })).is_err()
        );
        assert_eq!(
            input_actions(InputCommand::Hold {
                combo: "shift".into()
            })
            .unwrap(),
            [InputAction::KeyDown {
                combo: "shift".into()
            }]
        );
    }

    phase inputs_and_boot_bundles_are_validated_before_use;
    {
        {
            let click = input_actions(InputCommand::Click {
                button: "right".into(),
                at: Some(vec![4, 5]),
            })
            .unwrap();
            assert_eq!(
                click,
                [InputAction::Click {
                    button: Button::Right,
                    at: Some((4, 5))
                }]
            );
            assert_eq!(describe_input(&click[0]), "clicked right at 4,5");
            assert!(matches!(
                input_actions(InputCommand::Click {
                    button: "fourth".into(),
                    at: None
                }),
                Err(CliError::InvalidInput(_))
            ));
            let error = input_actions(InputCommand::Type {
                text: Some("naïve".into()),
                stdin: false,
            })
            .unwrap_err();
            assert!(error.to_string().contains('ï'));
            assert!(error.hint().contains("tilo-vm input key"));
        }

        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("Image"), b"kernel").unwrap();
            std::fs::write(dir.path().join("initrd"), b"initrd").unwrap();
            let bundle = dir.path().join("boot.json");
            std::fs::write(
                &bundle,
                r#"{"kernel":"Image","initrd":"initrd","cmdline":"root=tilo-auto composefs=abc","loader_partuuid":"FCDFC623-0563-4F91-9DB8-F2B87B98B3CE"}"#,
            )
            .unwrap();
            let direct = read_direct_boot_bundle(&bundle).unwrap();
            assert_eq!(
                direct.kernel,
                dir.path().join("Image").display().to_string()
            );
            assert_eq!(
                direct.initrd,
                dir.path().join("initrd").display().to_string()
            );
            assert_eq!(
                direct.loader_partuuid.as_deref(),
                Some("fcdfc623-0563-4f91-9db8-f2b87b98b3ce")
            );
            std::fs::write(
                &bundle,
                r#"{"kernel":"Image","initrd":"initrd","cmdline":"root=tilo-auto","loader_partuuid":"invalid"}"#,
            )
            .unwrap();
            assert!(read_direct_boot_bundle(&bundle).is_err());
        }
    }

    phase touch_pen_and_gamepad_commands_become_actions;
    {
        use clap::Parser as _;
        let parse = |args: &[&str]| {
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            let Some(Command::Input { action, .. }) = cli.command else {
                panic!("{args:?} is not an input command");
            };
            input_actions(action)
        };
        let pinch = parse(&[
            "tilo-vm", "input", "touch", "pinch", "640", "400", "--from", "400", "--to", "100",
        ])
        .unwrap();
        assert_eq!(
            pinch,
            [InputAction::TouchPinch {
                center: (640, 400),
                from_distance: 400,
                to_distance: 100,
                duration_ms: 400,
            }]
        );
        assert_eq!(
            describe_input(&pinch[0]),
            "pinched at 640,400 from 400 to 100 px apart"
        );
        let stick = parse(&["tilo-vm", "input", "gamepad", "stick", "left", "0", "-100"]).unwrap();
        assert_eq!(
            stick,
            [InputAction::GamepadStick {
                stick: Stick::Left,
                x: 0,
                y: -100,
                duration_ms: 300,
            }]
        );
        let dpad = parse(&["tilo-vm", "input", "gamepad", "dpad", "down", "right"]).unwrap();
        assert_eq!(
            dpad,
            [InputAction::GamepadPress {
                combos: vec!["down".into(), "right".into()]
            }]
        );
        let bad = parse(&["tilo-vm", "input", "gamepad", "dpad", "a"]).unwrap_err();
        assert!(bad.to_string().contains("d-pad direction"), "{bad}");
        let bad = parse(&["tilo-vm", "input", "gamepad", "press", "guide+zz"]).unwrap_err();
        assert!(bad.to_string().contains("'zz'"), "{bad}");
        let pen = parse(&[
            "tilo-vm",
            "input",
            "pen",
            "tap",
            "1",
            "2",
            "--pressure",
            "0",
        ]);
        assert!(pen.unwrap_err().to_string().contains("pressure"));
    }
}
