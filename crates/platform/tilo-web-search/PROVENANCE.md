# Provenance

Two upstream projects contributed to this crate. metasearch2 provided most
engine scrapers and the merge. SearXNG provided the way Google is asked.

## metasearch2

The engine scrapers in `src/engines/` (Bing, Brave, Marginalia), the
selector-driven result reader in `src/engines/mod.rs` and the rank merge in
`src/merge.rs` are ported from metasearch2. Its Google scraper was ported
too, and later replaced (see SearXNG below).

- Project: metasearch2, by mat-1
- Repository: https://github.com/mat-1/metasearch2
- Upstream revision: `910d52a2bc7af9498e9c9a7fac7b6726c1f1d4a1` (2026-08-31)
- Upstream files: `src/engines/search/{bing,brave,google,marginalia}.rs`,
  `src/parse.rs`, `src/engines/ranking.rs`
- License: CC0-1.0 (public domain dedication). `LICENSE-CC0` is the
  repository's `LICENSE` at that revision, byte for byte. CC0 asks for no
  attribution; it is kept here so the origin stays traceable.
- Ported: 2026-10-14

What was taken: each engine's request parameters, its CSS selectors for the
result list, title, link and snippet, its link unwrapping (Bing's base64
`u=a1` links), Marginalia's short-query rule, the engine weights (Google's
was raised so that Google leads), and the merge rule (a result at position `n` scores
`weight / n`, scores add across engines, the most trusted engine's title and
snippet win).

What changed, and why:

- HTML is read by this crate's own bounded parser and selector matcher
  (`src/html.rs`) instead of `scraper`/`html5ever`, so the crate adds no
  dependencies and every page is parsed under fixed size, node and depth
  limits.
- Requests go through `tilo-net` (Tilo's one TLS stack and trust source)
  instead of a browser-impersonating client, with a user agent that says what
  is asking. Engines that refuse such a client are reported as blocked rather
  than worked around, which is why `DuckDuckGo` and Marginalia are not asked
  by default. metasearch2 reaches Google's own result page by imitating
  Firefox's TLS handshake with BoringSSL (`wreq`). Tilo allows only rustls,
  so Google is asked another way instead.
- A challenge page is reported as blocked and a page without a result list
  as a layout error, so neither is mistaken for "no results".
- Result addresses keep their scheme and lose tracking parameters and
  fragments; duplicates are matched on host without `www.`, decoded path
  without trailing slash, and query.
- `DuckDuckGo`'s no-JavaScript endpoint was added; Google Scholar, Stract,
  Yep, RightDao, the instant answers and the post-search fetchers were not
  ported.

## SearXNG

`src/engines/google.rs` asks Google the way SearXNG's `google_cse` engine
does. Google's own result page needs JavaScript, and the no-script layouts
(`gbv=1`, text browsers, the WML page for old phones) now refuse clients. The
search element sites embed still answers a plain HTTP client.

- Project: SearXNG
- Repository: https://github.com/searxng/searxng
- Upstream revision: `b00212a8366131936856b726ed4dd9bd281e4d7e` (2026-10-15)
- Upstream file: `searx/engines/google_cse.py`
- License: AGPL-3.0-or-later. Tilo is AGPL-3.0-only, which that licence
  allows, and the repository's `LICENSE` carries the text.
- Ported: 2026-10-15

What was taken:

- The technique: fetch `cse.js` for the engine id to get a token, then ask
  `cse.google.com/cse/element/v1` with it and read the JSONP answer.
- The query parameters and the referrer.
- The public engine id SearXNG uses (blackle.com's).
- The JSON fields read.
- Reading `"code": 429` as a rate limit.

The Rust code is Tilo's own. It differs in these ways:

- The token is kept in memory for at most an hour and dropped as soon as a
  query fails, instead of in a shared cache.
- No cookies are sent. SearXNG sends `CONSENT=YES+`, but the element answers
  without it.
- No interface language, region, time range or paging is set.
- A rate limit, a `/sorry/` page or a CAPTCHA makes the search leave Google
  alone for a while (5 minutes, doubling up to an hour), shared with the
  other engines' backoff in `src/search.rs`.

Google ends whole-web search for these engines on 2027-01-01. When it does,
this engine fails or answers with nothing, and the search carries on with
the others.
