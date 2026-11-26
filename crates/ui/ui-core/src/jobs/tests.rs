//! The job registry: what it holds, what it says, and what survives.

use super::Owner;
use super::record::{Outcome, Progress, Spec, Subject, Unit};
use super::{dismiss, finished, rows, running, start, stop, summary};

fn owner() -> Owner {
    Owner::new("so.tilo.test", "Test").with_icon(crate::icons::Icon::Download)
}

fn began(title: &str) -> super::Running {
    start(Spec::new(title).subject(Subject::new(owner(), title)))
}

tilo_test_env::scenario! {
    fn a_job_shows_its_progress_and_can_be_stopped_only_where_it_can;

    phase a_job_is_visible_before_its_worker_says_anything;
    // A job survives the surface that started it: it is in the registry before its worker reports
    // anything, and the row is there for whoever looks next.
    {
        let _serial = crate::jobs::test_lock();
        crate::jobs::clear();
        let job = began("Installing LibreOffice");
        let rows = rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title(), "Installing LibreOffice");
        assert!(rows[0].is_running());
        assert_eq!(rows[0].live_line(), "Starting");
        assert_eq!(summary().running, 1);
        assert!(super::is_active());
        job.done("Installed LibreOffice");
        assert!(!super::is_active());
    }

    phase a_job_says_what_it_is_doing_and_how_far;
    // What a job is doing, how far, and how it ended read as the person's sentences, never as a bare
    // number.
    {
        let _serial = crate::jobs::test_lock();
        crate::jobs::clear();
        let job = start(
            Spec::new("Installing LibreOffice")
                .subject(Subject::new(owner(), "LibreOffice").opens("store/app/libreoffice"))
                .unit(Unit::Bytes)
                .phase("Downloading"),
        );
        job.progress(312_000_000, 480_000_000);
        let row = &rows()[0];
        assert_eq!(row.live_line(), "Downloading 312 MB of 480 MB");
        assert_eq!(row.percent().as_deref(), Some("65%"));
        assert_eq!(
            row.subject().target.as_deref(),
            Some("store/app/libreoffice")
        );

        job.phase("Verifying");
        job.seen(1_204);
        let row = &rows()[0];
        assert_eq!(row.live_line(), "Verifying 1.2 kB");
        assert_eq!(row.percent(), None, "no total, no percentage");

        job.done("Installed LibreOffice");
        let row = &finished()[0];
        assert_eq!(row.outcome_line().as_deref(), Some("Installed LibreOffice"));
        assert_eq!(row.end_line(), "Installed LibreOffice");
        assert!(!row.can_stop());
    }

    phase only_a_job_that_can_be_stopped_is_offered_a_stop;
    // Stop is asked of the worker, and a job that can stop says so; a job without a way out shows no Stop,
    // because a Stop that would not be honoured is a lie.
    {
        let _serial = crate::jobs::test_lock();
        crate::jobs::clear();
        let stoppable = start(
            Spec::new("Scanning Home")
                .subject(Subject::new(owner(), "Home"))
                .stoppable(|| {}),
        );
        assert!(running()[0].can_stop());
        let plain = began("Copying");
        assert!(!running()[1].can_stop());

        assert!(stop(stoppable.id()));
        assert!(stoppable.is_stopped(), "the worker hears it");
        let being_stopped = running()[0].clone();
        assert_eq!(being_stopped.id(), stoppable.id());
        assert!(being_stopped.is_stopping());
        assert!(!being_stopped.can_stop(), "Stop is not offered twice");
        stoppable.stopped();
        assert_eq!(finished()[0].outcome(), Some(&Outcome::Stopped));
        assert_eq!(finished()[0].outcome_line().as_deref(), Some("Stopped"));
        assert!(!stop(plain.id()), "a job with no way out is not asked");
    }

    phase a_running_row_cannot_be_read_away;
    {
        let _serial = crate::jobs::test_lock();
        crate::jobs::clear();
        let job = began("Scanning Home");
        dismiss(job.id());
        assert_eq!(running().len(), 1);
        job.stopped();
        dismiss(job.id());
        assert!(rows().is_empty());
    }

    phase jobs_are_concurrent_by_construction;
    {
        let _serial = crate::jobs::test_lock();
        crate::jobs::clear();
        let install = began("Installing LibreOffice");
        let _scan = began("Scanning Home");
        assert_eq!(running().len(), 2);
        install.progress(1, 4);
        assert_eq!(
            running()[0].progress(),
            &Progress::Measured { done: 1, total: 4 }
        );
        assert_eq!(
            running()[1].progress(),
            &Progress::Starting,
            "one job's progress is not another's"
        );
    }
}

#[test]
fn a_failure_says_why_and_stays_until_it_is_read() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    let job = began("Installing LibreOffice");
    job.failed("480 MB needed, 210 MB free");
    let row = &rows()[0];
    assert_eq!(
        row.outcome_line().as_deref(),
        Some("Couldn’t finish: 480 MB needed, 210 MB free"),
        "{:?}",
        row.outcome_line()
    );

    dismiss(job.id());
    assert!(rows().is_empty());
}
