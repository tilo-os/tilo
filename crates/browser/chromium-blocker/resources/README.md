# Bundled filter resources

`resources.json` is generated for adblock-rust 0.12.1 from these upstream projects:

- [uBlock Origin, Brave fork](https://github.com/brave/uBlock/tree/4dfbb048607ec4fd19c1c252fd255df6cc653720), copyright Raymond Hill and contributors, GPL-3.0-or-later. Scriptlet function bodies, dependencies and aliases come from `src/js/resources/`; replacement bodies and aliases come from `src/web_accessible_resources/` and `src/js/redirect-resources.js`.
- [Brave adblock-resources](https://github.com/brave/adblock-resources/tree/6da9b247c3368baa8b4f24cd02046e18702a6527), copyright Brave Software and contributors, MPL-2.0. Additional replacement/scriptlet resources come from `dist/resources.json`.

The upstream licenses are in `LICENSE.uBlock.txt` and `LICENSE.Brave.txt`.
`provenance.json` records immutable revisions, downloaded input hashes, output
hash, resource count, and explicit omissions. The corresponding source is
available at the pinned repository links above and the updater's archive URL.

Regenerate with Python 3.12+ and Node 22+:

```sh
python3 scripts/browser/update-blocker-resources.py
python3 scripts/browser/update-blocker-resources.py --check
```

The updater downloads only pinned, hash-verified inputs and invokes their ESM
resource registries. It preserves `requiresTrust` as permission mask 1; ordinary
filter lists have no permission to invoke those resources. A parameterized
redirect (`click2load.html`) is omitted because the engine's resource model does
not express its URL arguments. The conversion does not imply full uBlock Origin
syntax or execution compatibility.

The embedding executor must provide lexical `scriptletGlobals = {}` around
injected scriptlet code. The native local HTTP fixture verifies an actual
`set-constant` execution with its dependency chain and both regular and empty
redirect replacements.
