# Reader typography references

The original `serif-reader.png` and `math-reader.png` are the Linux references.
Windows uses the `-windows.png` references with the checked-in controlled font
catalog (`crates/sdk/ui-core/assets/fonts`). Both tests retain the same pixel comparison and
fail when an offscreen renderer is unavailable.

The Windows images were reviewed for text, line breaks, serif styling, formula
structure, and clipping. They are renderer outputs, not edited screenshots.
Keep baseline changes explicit; a successful render alone does not approve a
new reference image.
