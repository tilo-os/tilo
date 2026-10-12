# Observable live search capture

The headless full-loop example previously proved nonblank pixels and an open semantic input target, but did not assert painted pixels inside that target. A real closed-desktop capture passes the nonblank predicate. The new positive guard rejects that capture and accepts actual open-query pixels using the production named input bounds. Existing state, modifier-mask, target, nonblank and Escape assertions remain.

Source 3ff688550948a466898c026785d2785a1ada2ab7 passed the tracked private live journey against compositor candidate 840309547ab0f36f11d2c3c4c2fd16dd5069db92. Keyboard input, wheel pointer activation, search query, semantic targets, positive query pixels and closed-frame difference passed. Profiles, socket and compositor were cleaned up. Python compilation and pre-commit contracts pass. The compositor build completed in 3m11s on the SSH runner; no laptop Cargo builds ran.

No watched-mode or renderer policy change is part of this increment. An initial visual-preview diagnosis of a missing palette was withdrawn after independent ImageMagick and stdlib PNG decoding found query pixels in both captures. The qualification here is the reproducible closed-capture rejection and positive live pixel assertion; no new production animation failure is claimed.

Representative search/Escape evidence is published on the PR. Raw images stay outside Git. Successful sandboxed app mapping and VM/device coverage remain open. Library prepared uploads remain unavailable; no new Library receipts are claimed.
