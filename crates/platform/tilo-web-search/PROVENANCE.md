# metasearch2 provenance

The engine scrapers in `src/engines/` (Bing, Brave, Google, Marginalia), the
selector-driven result reader in `src/engines/mod.rs` and the rank merge in
`src/merge.rs` are ported from metasearch2.

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
`u=a1` links, Google's `/url?q=`), Marginalia's short-query rule, the
engine weights, and the merge rule (a result at position `n` scores
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
  than worked around, which is why Google, `DuckDuckGo` and Marginalia are
  not asked by default.
- A challenge page is reported as blocked and a page without a result list
  as a layout error, so neither is mistaken for "no results".
- Result addresses keep their scheme and lose tracking parameters and
  fragments; duplicates are matched on host without `www.`, decoded path
  without trailing slash, and query.
- `DuckDuckGo`'s no-JavaScript endpoint was added; Google Scholar, Stract,
  Yep, RightDao, the instant answers and the post-search fetchers were not
  ported.
