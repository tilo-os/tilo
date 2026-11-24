//! A view that declares `protect capture`: its window is left out of captures and streams,
//! whichever window half draws it.

use crate::hosted::{SECRET, Session, Window};

tilo_test_env::scenario! {
    /// The window a protected view fills withholds itself from captures, as a Rust app's
    /// `capture_protected` window does; a view that declares nothing does not.
    fn a_protected_view_withholds_its_window_from_captures;

    phase the_headless_frame_reports_the_protection;
    {
        let _settled = ui::motion::testing::settled();
        let mut session = Session::new(SECRET, "ada");
        session.settle();
        session.window.harness().relayout();
        assert!(
            session.window.harness().capture_protected(),
            "the frame a capture would take is withheld"
        );
    }

    phase the_live_window_withholds_itself;
    {
        let _settled = ui::motion::testing::settled();
        let mut session = Session::open(SECRET, "ada", Window::live);
        session.settle();
        let Window::Live(app) = &session.window else {
            panic!("the window is the live app");
        };
        // What the session reads to withhold the app's windows from screenshots, recordings,
        // share streams and remote viewers.
        assert!(
            app.withholds_captures(),
            "the live window says it keeps what it shows"
        );
    }

    phase the_window_half_reports_it_to_its_host;
    {
        let _settled = ui::motion::testing::settled();
        let mut session = Session::new(SECRET, "ada");
        session.settle();
        let view = session.hosted().view().expect("the window is connected");
        assert!(
            view.capture_protected(),
            "the window's half says what it shows may not be captured"
        );
    }

    phase the_page_rendered_from_it_is_withheld_too;
    {
        use tilolang_process::window::shown_on_page::ShownOnPage;
        let _settled = ui::motion::testing::settled();
        let mut session = Session::new(SECRET, "ada");
        session.settle();
        let shown = ShownOnPage::new().open(session.window.harness(), None);
        assert!(
            shown.capture_protected,
            "the page a capture would render leaves the view out"
        );
    }
}
