//! The platform a window with nothing on screen stands on: the packages it loads, the words its
//! window speaks, the clock and zone it reads and the content its apps keep. `tilolang
//! layout-check` lays its windows out on it, and a browser page runs its apps on it.

use std::sync::Arc;

use std::rc::Rc;
use tilo_clipboard::{ClipboardBackend, ClipboardImage, ClipboardTarget};
use tilo_content::ContentStore;
use tilolang_runtime::blob::FileValue;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{GlobalLocale, Locale, Platform};
use tilolang_tilo::tilolang::Compiled;

/// Browser and headless layout-check windows share the toolkit's catalogues, language and region
/// formats on their thread. Neither runs a desktop settings watcher or reads on-disk language
/// packs, so `words_revision` reflects only in-process `I18nManager` or region-format changes and
/// cannot notice an external catalogue changing while running.
static WEB_LOCALE: GlobalLocale = tilo_app_window::platform_locale!(GlobalLocale);

/// The compiled packages an instance loads: the app and what it uses, the app last; and the
/// packages it uses as metadata: the native apps' it calls (`use so.tilo.session as session`),
/// whose actions run at their own action APIs, and the platform's (the device's, Settings') whose
/// published resources it reads.
pub struct Packages {
    pub compiled: Rc<Vec<Compiled>>,
    pub platform: Vec<tilolang_tilo::tilolang::PackageMeta>,
    /// The clipboard its apps copy to and paste from: the browser's on a page, the page's own in
    /// memory when nothing lays it out.
    clipboard: Arc<dyn ClipboardBackend>,
    /// The content its apps keep and read: the file an action makes of what it holds
    /// (`File.of_text`), kept in memory, as a window laid out or run with nothing on disk holds
    /// it.
    content: ContentStore,
}

impl Packages {
    /// The packages a window of a page runs, with the page's browser clipboard.
    #[must_use]
    pub fn new(
        compiled: Rc<Vec<Compiled>>,
        platform: Vec<tilolang_tilo::tilolang::PackageMeta>,
    ) -> Self {
        Self {
            compiled,
            platform,
            clipboard: page_clipboard(),
            content: ContentStore::memory(),
        }
    }

    /// The same packages, with `clipboard` in place of the page's: what a test hands a page to
    /// see what its apps copy and paste.
    #[must_use]
    pub fn with_clipboard(mut self, clipboard: Arc<dyn ClipboardBackend>) -> Self {
        self.clipboard = clipboard;
        self
    }
}

/// The clipboard a page copies to: the browser's, the one the Rust apps of the page copy to
/// and paste from ([`tilo_clipboard::system`]); a page laid out with no browser keeps it in its
/// own memory, so laying a window out never touches a person's clipboard.
fn page_clipboard() -> Arc<dyn ClipboardBackend> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        tilo_clipboard::system()
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        Arc::new(tilo_clipboard::InProcessClipboard::default())
    }
}

/// What a pasted image's media type is, from the type the selection offers.
fn image_of(image: &ClipboardImage) -> (String, Vec<u8>) {
    (image.mime.to_owned(), image.bytes.clone())
}

impl Platform for Packages {
    fn load(&self, host: &Host) -> u64 {
        // The platform's resources stand as a device that has none of them, read as each
        // declares when nothing is published: a view over the sound or the battery lays out as
        // it does on a computer without them, and the region and formats read the defaults a
        // new install starts from rather than trapping on a section no session has written.
        for meta in &self.platform {
            if let Err(error) = host.runtime().load_foreign(meta.clone()) {
                eprintln!("tilolang: a platform package did not load: {error}");
            }
        }
        for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
            if !self
                .platform
                .iter()
                .any(|meta| ty.starts_with(&format!("{}#", meta.app)))
            {
                continue;
            }
            match tilolang_tilo::runtime::ResourceRef::parse_id(&published, &ty) {
                // A root the host serves itself (the workspace's members) reads as it serves it.
                Some(target) if host.runtime().serves(&target) => {}
                Some(target) => {
                    let reading = tilolang_tilo::platform_apps::platform_root_defaults(&published);
                    if let Err(error) = host.runtime().set_reading(&target, reading) {
                        eprintln!("tilolang: `{published}` reads nothing: {error}");
                    }
                }
                None => eprintln!("tilolang: `{published}` names no resource"),
            }
        }
        for compiled in self.compiled.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                eprintln!("tilolang: a package did not load: {error}");
            }
        }
        0
    }

    fn locale(&self) -> Option<&dyn Locale> {
        Some(&WEB_LOCALE)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_app_window::i18n::phrase(id, args)
    }

    fn now(&self) -> String {
        tilolang_tilo::instance::wall_now()
    }

    fn moment(&self) -> tilolang_tilo::instance::Zoned {
        tilolang_tilo::instance::wall_moment()
    }

    /// `clipboard.copy` and `clipboard.copy_image`: the browser offers a page a text copy inside
    /// the person's click and nothing else, so a text is offered as the person's copy and an image
    /// answers false, as a device that carries no image does.
    fn copy_clipboard(&self, mime: &str, bytes: &[u8]) -> bool {
        if mime != "text/plain" {
            return false;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return false;
        };
        self.clipboard.set_text(ClipboardTarget::Clipboard, text)
    }

    /// `clipboard.paste` and `clipboard.paste_image`: what the person's selection holds, read
    /// from the browser's clipboard events; outside one a read answers nothing, as a page that
    /// was not granted it does.
    fn paste_clipboard(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        for wanted in types {
            if *wanted == "text/plain" {
                if let Some(text) = self.clipboard.text(ClipboardTarget::Clipboard) {
                    return Some(((*wanted).to_owned(), text.into_bytes()));
                }
                continue;
            }
            if let Some(image) = self.clipboard.image()
                && wanted.eq_ignore_ascii_case(image.mime)
            {
                return Some(image_of(&image));
            }
        }
        None
    }

    /// The content at `hash`, when this host holds it: a file an action reads (`.text()`), and a
    /// scenario's file a view shows.
    fn read_content(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.read(hash)
    }

    /// `File.of_text` and what else makes a file of what it holds: its bytes are kept in this
    /// host's own store, so a file a scenario makes is one its views read.
    fn keep_content(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let hash = self
            .content
            .put(bytes)
            .map_err(|error| format!("`{name}` could not be kept: {error}"))?;
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }

    /// `file.save`: the page downloads the bytes under the name the app offers, as the page's
    /// Rust apps save. A browser grants a page a picker inside the person's click and nowhere
    /// else, so a save is the browser's download and never a chooser to wait for.
    ///
    /// # Errors
    ///
    /// Why the browser did not start the download, or that this window saves nothing at all.
    fn save_file(
        &self,
        request: &tilolang_tilo::runtime::blob::SaveRequest,
        content: &[u8],
    ) -> Result<(), String> {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        return tilo_dialogs::download(&request.name, content);
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let _ = (request, content);
            Err("a window laid out with nothing on disk saves no files".to_owned())
        }
    }

    /// `file.choose`: a page cannot answer one. The browser's picker answers a promise, and the
    /// runtime asks for the chosen files before the action that asked can go on: a file reaches
    /// a page by the person dropping it on the window, or is read from the page's own content.
    ///
    /// # Errors
    ///
    /// Always, saying what a page offers instead.
    fn choose_files(
        &self,
        request: &tilolang_tilo::runtime::blob::FileRequest,
    ) -> Result<Vec<FileValue>, String> {
        let _ = request;
        Err("a browser page cannot open a file chooser here: drop the file on the window, or open it from the workspace's files".to_owned())
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restores thread-local region formats, clock zone and i18n state (including an unset
    /// `person_locale` override) even if an assertion fails.
    struct ThreadLocaleGuard {
        prev_locale: String,
        prev_person: Option<String>,
    }

    impl ThreadLocaleGuard {
        fn enter(person: Option<&str>) -> Self {
            tilo_app_window::i18n::ensure();
            Self {
                prev_locale: ui::i18n::I18nManager::current_locale(),
                prev_person: ui::i18n::set_person_locale(person),
            }
        }
    }

    impl Drop for ThreadLocaleGuard {
        fn drop(&mut self) {
            let _ = ui::i18n::set_person_locale(self.prev_person.as_deref());
            ui::i18n::I18nManager::set_locale(&self.prev_locale);
            ui::format::region::install_here(None, None);
        }
    }

    /// The web / layout-check host agrees with every other host on the shared table of dates,
    /// times, clocks, week starts and German typed dates, and moves `words_revision` on
    /// in-process display locale changes.
    #[test]
    fn a_web_packages_dates_week_and_language_agree_with_every_other_host() {
        use tilolang_tilo::instance::testing::{
            LocalAgreementHost, assert_platform_agrees, compile_probe,
        };

        let _guard = ThreadLocaleGuard::enter(Some("en-US"));
        ui::i18n::I18nManager::set_locale("en-US");

        let (compiled, actions) = compile_probe();
        let platform = Rc::new(Packages::new(Rc::new(vec![compiled]), Vec::new()));
        let utc = ui::clock::Zone::of(ui::format::TimeZone::UTC);
        let mut harness = LocalAgreementHost::open(
            "Packages",
            Rc::clone(&platform) as Rc<dyn Platform>,
            move |formats| {
                ui::format::region::install_here(Some(formats), Some(utc.clone()));
            },
            |locale| {
                let _ = ui::i18n::set_person_locale(Some(locale));
            },
        );
        assert_platform_agrees(&mut harness, &actions);

        let catalog_rev = || {
            platform
                .words_revision()
                .wrapping_sub(ui::format::region::revision())
        };
        let rev_before = catalog_rev();
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_ne!(
            catalog_rev(),
            rev_before,
            "switching display locale moves words_revision"
        );
    }

    /// A clipboard the browser refused: nothing written, nothing read back, as a page outside
    /// the person's click.
    #[derive(Default)]
    struct Refusing;

    impl ClipboardBackend for Refusing {
        fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
            false
        }

        fn text(&self, _: ClipboardTarget) -> Option<String> {
            None
        }

        fn name(&self) -> &'static str {
            "refusing"
        }
    }

    /// A selection holding one image, as a screenshot or a picture copied in a browser.
    struct Holding(ClipboardImage);

    impl ClipboardBackend for Holding {
        fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
            false
        }

        fn text(&self, _: ClipboardTarget) -> Option<String> {
            None
        }

        fn image(&self) -> Option<ClipboardImage> {
            Some(ClipboardImage {
                mime: self.0.mime,
                bytes: self.0.bytes.clone(),
            })
        }

        fn name(&self) -> &'static str {
            "holding"
        }
    }

    fn page(clipboard: Arc<dyn ClipboardBackend>) -> Packages {
        Packages::new(Rc::new(Vec::new()), Vec::new()).with_clipboard(clipboard)
    }

    #[test]
    fn a_page_copies_text_and_pastes_it_back() {
        let packages = page(Arc::new(tilo_clipboard::InProcessClipboard::default()));
        assert!(
            packages.copy_clipboard("text/plain", "42".as_bytes()),
            "the app's text is the person's copy"
        );
        assert_eq!(
            packages.paste_clipboard(&["text/plain"]),
            Some(("text/plain".to_owned(), b"42".to_vec()))
        );
        assert!(
            !packages.copy_clipboard("image/png", b"\x89PNG"),
            "a page offers a text copy and no image"
        );
    }

    #[test]
    fn a_copy_the_browser_refuses_answers_false_and_a_paste_nothing() {
        let packages = page(Arc::new(Refusing));
        assert!(
            !packages.copy_clipboard("text/plain", "42".as_bytes()),
            "a copy outside the person's click is refused, and the action is told so"
        );
        assert_eq!(
            packages.paste_clipboard(&["text/plain", "image/png"]),
            None,
            "a read outside a clipboard event is granted nothing"
        );
    }

    #[test]
    fn an_image_the_selection_holds_pastes_under_its_media_type() {
        let packages = page(Arc::new(Holding(ClipboardImage {
            mime: "image/png",
            bytes: vec![0x89, b'P', b'N', b'G'],
        })));
        assert_eq!(
            packages.paste_clipboard(&["image/png", "image/jpeg"]),
            Some(("image/png".to_owned(), vec![0x89, b'P', b'N', b'G'])),
            "an image pastes as its bytes under the type it was offered as"
        );
    }
}
