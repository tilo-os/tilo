//! The HTML around a page: header, navigation, the page, its contents and
//! the search dialog. Everything a page shows comes from its record and its
//! rendered body; nothing here knows any page.

use std::fmt::Write as _;

use crate::book::{Book, Page};
use crate::html::escape;
use crate::markdown::Rendered;

/// The files every page names, by their content-addressed paths.
#[derive(Debug, Clone)]
pub struct Assets {
    pub stylesheet: String,
    pub script: String,
    pub icon: String,
    /// Fonts the stylesheet uses first, preloaded.
    pub preload_fonts: Vec<String>,
}

pub struct Context<'a> {
    pub book: &'a Book,
    pub assets: &'a Assets,
    pub page: &'a Page,
    pub rendered: &'a Rendered,
    pub previous: Option<&'a Page>,
    pub next: Option<&'a Page>,
}

const MARK: &str = r#"<svg class="mark" viewBox="10.98 16 78.04 68" aria-hidden="true"><path d="M10.98 31.85Q10.98 27.97 14.4 26.12L20.26 22.97Q24.84 20.51 24.84 25.71V74.29Q24.84 79.49 20.26 77.03L14.4 73.88Q10.98 72.03 10.98 68.15Z" class="wing"/><path d="M89.02 31.85Q89.02 27.97 85.6 26.12L79.74 22.97Q75.16 20.51 75.16 25.71V74.29Q75.16 79.49 79.74 77.03L85.6 73.88Q89.02 72.03 89.02 68.15Z" class="wing"/><rect x="29" y="16" width="42" height="68" rx="6" class="body"/></svg>"#;

const ICON_MENU: &str =
    r#"<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3 6h14M3 10h14M3 14h14"/></svg>"#;
const ICON_SEARCH: &str = r#"<svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5.5"/><path d="m13 13 4 4"/></svg>"#;
const ICON_THEME: &str = r#"<svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="6.5"/><path d="M10 3.5v13A6.5 6.5 0 0 0 10 3.5Z" class="fill"/></svg>"#;

/// Sets the colour theme a visitor chose before the page paints, so a dark
/// page never flashes light.
const THEME_BOOT: &str = r#"<script>try{var t=localStorage.getItem("docs-theme");if(t==="light"||t==="dark")document.documentElement.dataset.theme=t}catch(e){}</script>"#;

#[must_use]
pub fn page(context: &Context<'_>) -> String {
    let Context {
        book,
        assets,
        page,
        rendered,
        ..
    } = context;
    let mut html = String::with_capacity(rendered.html.len().saturating_add(16 * 1024));
    let title = if page.slug.is_empty() {
        book.title.clone()
    } else {
        format!("{} – {}", page.title, book.title)
    };
    let canonical = book.absolute(&page.url);
    let markdown = page.markdown_url(&book.base);
    head(
        &mut html,
        book,
        assets,
        &Meta {
            title: &title,
            description: &page.description,
            page: Some((&page.title, &canonical, &markdown)),
        },
    );
    html.push_str("<div class=\"layout\">\n");
    navigation(&mut html, book, Some(page));
    article(&mut html, context);
    contents(&mut html, rendered);
    html.push_str("</div>\n");
    search_dialog(&mut html);
    html.push_str("</body>\n</html>\n");
    html
}

/// The page for an address in the book that names no page.
#[must_use]
pub fn not_found(book: &Book, assets: &Assets) -> String {
    let mut html = String::with_capacity(16 * 1024);
    let title = format!("No page here – {}", book.title);
    head(
        &mut html,
        book,
        assets,
        &Meta {
            title: &title,
            description: "There is no page at this address.",
            page: None,
        },
    );
    html.push_str("<div class=\"layout\">\n");
    navigation(&mut html, book, None);
    let _ = write!(
        html,
        "<main id=\"content\" class=\"page\">\n<article>\n<h1>No page here</h1>\n\
         <p>Nothing in the docs is at this address. It may have moved: search for what you were after, \
         or start from the <a href=\"{}\">overview</a>.</p>\n\
         <p><button type=\"button\" class=\"search-again\" data-search>Search the docs</button></p>\n\
         </article>\n</main>\n<aside class=\"toc\" aria-hidden=\"true\"></aside>\n</div>\n",
        escape(&book.base)
    );
    search_dialog(&mut html);
    html.push_str("</body>\n</html>\n");
    html
}

/// What a page's head says about it; `page` is its title, canonical address
/// and Markdown address, for a page of the book.
struct Meta<'a> {
    title: &'a str,
    description: &'a str,
    page: Option<(&'a str, &'a str, &'a str)>,
}

fn head(html: &mut String, book: &Book, assets: &Assets, meta: &Meta<'_>) {
    let _ = write!(
        html,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n<meta name=\"description\" content=\"{}\">\n",
        escape(meta.title),
        escape(meta.description),
    );
    if let Some((title, canonical, markdown)) = meta.page {
        let _ = write!(
            html,
            "<link rel=\"canonical\" href=\"{canonical}\">\n\
             <link rel=\"alternate\" type=\"text/markdown\" href=\"{markdown}\" title=\"This page as Markdown\">\n\
             <meta property=\"og:type\" content=\"article\">\n<meta property=\"og:title\" content=\"{title}\">\n\
             <meta property=\"og:description\" content=\"{description}\">\n<meta property=\"og:url\" content=\"{canonical}\">\n",
            canonical = escape(canonical),
            markdown = escape(markdown),
            title = escape(title),
            description = escape(meta.description),
        );
    } else {
        html.push_str("<meta name=\"robots\" content=\"noindex\">\n");
    }
    let _ = writeln!(
        html,
        "<meta name=\"color-scheme\" content=\"light dark\">\n<link rel=\"icon\" href=\"{}\" type=\"image/svg+xml\">",
        escape(&assets.icon)
    );
    for font in &assets.preload_fonts {
        let _ = writeln!(
            html,
            "<link rel=\"preload\" href=\"{}\" as=\"font\" type=\"font/ttf\" crossorigin>",
            escape(font)
        );
    }
    let _ = write!(
        html,
        "<link rel=\"stylesheet\" href=\"{stylesheet}\">\n{THEME_BOOT}\n<script src=\"{script}\" defer></script>\n</head>\n\
         <body data-base=\"{base}\">\n<a class=\"skip\" href=\"#content\">Skip to the page</a>\n",
        stylesheet = escape(&assets.stylesheet),
        script = escape(&assets.script),
        base = escape(&book.base),
    );
    header(html, book);
}

fn header(html: &mut String, book: &Book) {
    let _ = write!(
        html,
        "<header class=\"top\">\n\
         <button type=\"button\" class=\"icon-button menu\" aria-controls=\"nav\" aria-expanded=\"false\" aria-label=\"Show the pages\">{ICON_MENU}</button>\n\
         <a class=\"brand\" href=\"{base}\">{MARK}<span class=\"brand-name\">Tilo</span><span class=\"brand-docs\">Docs</span></a>\n\
         <button type=\"button\" class=\"search-open\" data-search aria-label=\"Search the docs\">{ICON_SEARCH}<span>Search</span><kbd>/</kbd></button>\n\
         <a class=\"top-link\" href=\"/\">Open Tilo</a>\n\
         <button type=\"button\" class=\"icon-button theme\" aria-label=\"Colour theme\" title=\"Colour theme\">{ICON_THEME}</button>\n\
         </header>\n",
        base = escape(&book.base),
    );
}

fn navigation(html: &mut String, book: &Book, current: Option<&Page>) {
    html.push_str("<nav id=\"nav\" class=\"nav\" aria-label=\"Pages\">\n");
    let mut open = None;
    for (number, page) in &book.pages {
        if open != Some(*number) {
            if open.is_some() {
                html.push_str("</ul>\n");
            }
            open = Some(*number);
            let title = book
                .sections
                .get(*number)
                .map_or("", |section| section.title.as_str());
            let _ = write!(
                html,
                "<p class=\"nav-section\">{}</p>\n<ul>\n",
                escape(title)
            );
        }
        let here = if current.is_some_and(|current| current.id == page.id) {
            " aria-current=\"page\""
        } else {
            ""
        };
        let _ = writeln!(
            html,
            "<li><a href=\"{}\"{here}>{}</a></li>",
            escape(&page.url),
            escape(&page.nav)
        );
    }
    html.push_str("</ul>\n</nav>\n");
}

fn article(html: &mut String, context: &Context<'_>) {
    let Context {
        book,
        page,
        rendered,
        previous,
        next,
        ..
    } = context;
    let section = book
        .pages
        .iter()
        .find(|(_, other)| other.id == page.id)
        .and_then(|(number, _)| book.sections.get(*number));
    html.push_str("<main id=\"content\" class=\"page\">\n<article>\n");
    if let Some(section) = section.filter(|_| !page.slug.is_empty()) {
        let _ = writeln!(html, "<p class=\"crumb\">{}</p>", escape(&section.title));
    }
    let _ = writeln!(html, "<h1>{}</h1>", escape(&page.title));
    html.push_str(&rendered.html);
    html.push_str("</article>\n<footer class=\"page-foot\">\n");
    if previous.is_some() || next.is_some() {
        html.push_str("<nav class=\"pager\" aria-label=\"Previous and next page\">");
        for (page, class, label) in [(previous, "previous", "Previous"), (next, "next", "Next")] {
            if let Some(page) = page {
                let _ = write!(
                    html,
                    "<a class=\"{class}\" href=\"{}\"><span>{label}</span>{}</a>",
                    escape(&page.url),
                    escape(&page.nav)
                );
            }
        }
        html.push_str("</nav>\n");
    }
    let origin = match &page.generator {
        Some(_) => format!(
            "Generated from <a href=\"{}\">{}</a>",
            escape(&page.source_url),
            escape(&page.source)
        ),
        None => format!(
            "<a href=\"{}\">Edit this page</a>",
            escape(&page.source_url)
        ),
    };
    let _ = write!(
        html,
        "<p class=\"page-meta\"><a href=\"{}\">This page as Markdown</a>{origin}</p>\n</footer>\n</main>\n",
        escape(&page.markdown_url(&book.base)),
    );
}

fn contents(html: &mut String, rendered: &Rendered) {
    let entries: Vec<_> = rendered
        .headings
        .iter()
        .filter(|heading| heading.level <= 3)
        .collect();
    if entries.len() < 2 {
        html.push_str("<aside class=\"toc\" aria-hidden=\"true\"></aside>\n");
        return;
    }
    html.push_str("<aside class=\"toc\"><nav aria-label=\"On this page\"><p class=\"toc-title\">On this page</p>\n<ul>\n");
    for heading in entries {
        let _ = writeln!(
            html,
            "<li class=\"level-{}\"><a href=\"#{}\">{}</a></li>",
            heading.level,
            escape(&heading.id),
            escape(&heading.text)
        );
    }
    html.push_str("</ul>\n</nav></aside>\n");
}

fn search_dialog(html: &mut String) {
    let _ = write!(
        html,
        "<dialog class=\"search\" aria-label=\"Search the docs\">\n\
         <form method=\"dialog\" class=\"search-box\" role=\"search\">{ICON_SEARCH}\
         <input type=\"search\" name=\"q\" placeholder=\"Search the docs\" autocomplete=\"off\" spellcheck=\"false\" aria-controls=\"search-results\">\
         <kbd>Esc</kbd></form>\n\
         <ul id=\"search-results\" class=\"results\" role=\"listbox\" aria-label=\"Results\"></ul>\n\
         <p class=\"search-empty\" hidden>Nothing matches. Try another word, or look through the pages on the left.</p>\n\
         </dialog>\n"
    );
}
