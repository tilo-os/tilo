//! The app's log: warnings and errors on standard error, which the
//! launcher carries into the session's journal, each line naming the app
//! and its window.

/// Send the app's warnings and errors to its standard error, which the
/// launcher carries into the session's journal, each line naming the app and
/// the window (tab) it comes from: `app=so.tilo.settings tab=… WARN …`.
///
/// The default filter of a bare `tracing_subscriber::fmt::try_init` with
/// `env-filter` is errors only, and a sandbox clears `RUST_LOG`: a refused
/// settings write, a person record that could not open, an action a control
/// could not deliver, all logged as warnings, never reached the journal, and
/// a lost write left no trace. `RUST_LOG` still widens or narrows it.
pub(super) fn init_app_log() {
    let environment = crate::host::environment();
    let prefix = app_log_prefix(
        environment
            .identity
            .package_id
            .as_deref()
            .or(environment.identity.app_name.as_deref()),
        environment.identity.continuity_instance.as_deref(),
    );
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(AppLog::new(prefix, std::io::stderr))
        .try_init();
}

/// What starts every line the app logs: its id and its window's.
fn app_log_prefix(app: Option<&str>, tab: Option<&str>) -> String {
    format!("app={} tab={} ", app.unwrap_or("-"), tab.unwrap_or("-"))
}

/// A log writer that starts each line with the app's [`app_log_prefix`].
#[derive(Clone)]
struct AppLog<F> {
    prefix: std::sync::Arc<str>,
    sink: F,
}

impl<F> AppLog<F> {
    fn new(prefix: String, sink: F) -> Self {
        Self {
            prefix: prefix.into(),
            sink,
        }
    }
}

/// One event's writer: the prefix, then the formatted event.
struct AppLogLine<W> {
    prefix: std::sync::Arc<str>,
    started: bool,
    out: W,
}

impl<W: std::io::Write> std::io::Write for AppLogLine<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.started {
            self.out.write_all(self.prefix.as_bytes())?;
            self.started = true;
        }
        self.out.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl<'a, F, W> tracing_subscriber::fmt::MakeWriter<'a> for AppLog<F>
where
    F: Fn() -> W + 'static,
    W: std::io::Write,
{
    type Writer = AppLogLine<W>;

    fn make_writer(&'a self) -> Self::Writer {
        AppLogLine {
            prefix: std::sync::Arc::clone(&self.prefix),
            started: false,
            out: (self.sink)(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apps_warning_reaches_its_log_with_its_id_and_window() {
        #[derive(Clone, Default)]
        struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let sink = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .with_ansi(false)
            .with_writer(AppLog::new(
                app_log_prefix(Some("so.tilo.settings"), Some("1747")),
                move || sink.clone(),
            ))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("not by default");
            tracing::warn!(
                section = "appearance",
                "settings: the shell refused a write"
            );
        });
        let log = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "warnings by default, not information: {log}"
        );
        assert!(
            lines[0].starts_with("app=so.tilo.settings tab=1747 "),
            "each line names the app and its window: {log}"
        );
        assert!(lines[0].contains("WARN"), "{log}");
        assert!(lines[0].contains("the shell refused a write"), "{log}");
    }
}
