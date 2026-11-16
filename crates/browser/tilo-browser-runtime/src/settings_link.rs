//! Where a `settings:` link an app opens goes on the page: the session opens
//! Settings on the page the link names ([`ui::shell::settings_anchor`]), as a
//! notice's choice does ([`crate::shell::HostRequest::ShowInApp`]), and the
//! request stays for the page's own desktop.

/// Take from `queue` what the embedding page answers, leaving each request
/// `stays` keeps for the page's own desktop: a `settings:` link queued
/// between frames waits there for the desktop to show it, and the page's
/// status never carries it off to an embedding page that does nothing with
/// it.
pub(crate) fn take_for_page<T>(queue: &mut Vec<T>, stays: impl Fn(&T) -> bool) -> Vec<T> {
    let (kept, taken) = std::mem::take(queue)
        .into_iter()
        .partition(|request| stays(request));
    *queue = kept;
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link queued after a frame is still there for the desktop once the
    /// page reported its status, which took everything else.
    #[test]
    fn the_status_leaves_a_settings_link_for_the_desktop() {
        let mut queue = vec!["copy", "show-in-app", "named"];
        let taken = take_for_page(&mut queue, |request| *request == "show-in-app");
        assert_eq!(taken, ["copy", "named"]);
        assert_eq!(queue, ["show-in-app"]);
    }
}
