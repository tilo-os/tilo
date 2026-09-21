//! Runnable lifecycle smoke test for the exact generated starter source.

fn main() {
    ui::launch::run_or_exit(&ui::launch::LaunchSpec::new(
        "dev.tilo.template-smoke",
        tilo_app_template_smoke::starter::app_root,
    ));
}
