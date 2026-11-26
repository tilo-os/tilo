use tilo_ui_base::widget::native_view::*;
use std::cell::RefCell;
use std::rc::Rc;
use tilo_ui_base::platform::PlatformConventions;
use vello::kurbo::Point;
use winit::event::WindowEvent;

#[derive(Clone, Debug, PartialEq)]
enum Call {
    Mount,
    HostChanged,
    Geometry(NativeViewGeometry),
    Visible(bool),
    Focus(bool),
    Enabled(bool),
    Platform(PlatformConventions),
    Input(Option<Point>),
    Unmount,
}

struct ProbeBackend {
    calls: Rc<RefCell<Vec<Call>>>,
    fail_mount: bool,
}

impl NativeViewBackend for ProbeBackend {
    fn mount(&mut self, _host: &NativeViewHost<'_>) -> Result<(), String> {
        self.calls.borrow_mut().push(Call::Mount);
        if self.fail_mount {
            Err("native API unavailable".into())
        } else {
            Ok(())
        }
    }

    fn unmount(&mut self) {
        self.calls.borrow_mut().push(Call::Unmount);
    }

    fn host_changed(&mut self, _host: &NativeViewHost<'_>) {
        self.calls.borrow_mut().push(Call::HostChanged);
    }

    fn set_geometry(&mut self, geometry: NativeViewGeometry) {
        self.calls.borrow_mut().push(Call::Geometry(geometry));
    }

    fn set_visible(&mut self, visible: bool) {
        self.calls.borrow_mut().push(Call::Visible(visible));
    }

    fn set_focus(&mut self, focused: bool) {
        self.calls.borrow_mut().push(Call::Focus(focused));
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.calls.borrow_mut().push(Call::Enabled(enabled));
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.calls.borrow_mut().push(Call::Platform(conventions));
    }

    fn handle_event(&mut self, _event: &WindowEvent, input: NativeViewInput) -> bool {
        self.calls
            .borrow_mut()
            .push(Call::Input(input.local_position));
        true
    }
}

fn backend(calls: &Rc<RefCell<Vec<Call>>>) -> ProbeBackend {
    ProbeBackend {
        calls: Rc::clone(calls),
        fail_mount: false,
    }
}

tilo_test_env::scenario! {
    fn a_native_mount_owns_its_lifecycle_and_reports_failure_truthfully;

    phase native_mount_failure_and_platform_accessibility_stay_truthful;
    {
        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new(
                "System account picker",
                ProbeBackend {
                    calls,
                    fail_mount: true,
                },
            );
            view.mount_native_view(&NativeViewHost::headless(
                PlatformConventions::default(),
                1.0,
            ));
            assert!(!view.is_mounted());
            assert_eq!(view.mount_error(), Some("native API unavailable"));
            let node = view
                .accessibility()
                .expect("failure wrapper remains semantic");
            assert_eq!(node.name.as_deref(), Some("System account picker"));
            assert_eq!(node.description.as_deref(), Some("native API unavailable"));
        }

        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let view = NativeView::platform_accessible(backend(&calls));
            assert!(view.accessibility().is_none());
        }

        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new(
                "System picker",
                ProbeBackend {
                    calls,
                    fail_mount: true,
                },
            )
            .with_fallback(Box::new(crate::widget::Text::new("Portable picker")));
            view.mount_native_view(&NativeViewHost::headless(
                PlatformConventions::default(),
                1.0,
            ));
            assert_eq!(view.children().len(), 1);
            assert_eq!(view.children()[0].name(), "Text");
        }
    }
}
