# Source application artwork

These are genuine SVG path assets, bundled as UTF-8 text with LF line endings.
They identify registry-discovered sources only, not a destination service,
installation status, or a promise that a sign-in will work. Product names,
logos and trademarks belong to their respective owners; identification does
not imply endorsement. The presentation layer never opens host icon files
or profile directories.

## Browser logos

Source repository: <https://github.com/alrra/browser-logos>, revision
`58881b84c4d73adc03c06fa2c275a7abee02d935`.

Each file comes from `src/<name>/<name>.svg`, where `<name>` is `chrome`,
`chromium`, `firefox`, or `brave`. The vectors are unmodified apart from
normalizing line endings to LF. Upstream's [legal notice](https://github.com/alrra/browser-logos/blob/58881b84c4d73adc03c06fa2c275a7abee02d935/README.md#legal)
states that all logos and trademarks are the property of their respective
owners; everything else is MIT-licensed. The MIT notice is reproduced in
`LICENSE-browser-logos.txt`; it does not grant trademark rights.

## Native application logos

Source repository: <https://github.com/homarr-labs/dashboard-icons>, revision
`6d31345986eba2067a63eb41c1a5a87d4e8e850a`.

Slack and Discord come from `svg/slack.svg` and `svg/discord.svg`, respectively,
unmodified apart from LF line-ending normalization. The repository is
Apache-2.0-licensed (`LICENSE-dashboard-icons.txt`) and its [disclaimer](https://github.com/homarr-labs/dashboard-icons/blob/6d31345986eba2067a63eb41c1a5a87d4e8e850a/README.md)
reserves the owners' product names and trademarks. The collection license
does not waive those rights.

## Bundled text SHA-256

| File | SHA-256 |
| --- | --- |
| `chrome.svg` | `98045c791131de00b7c81c5e4bd1ce686891661c0e2c821d588c351a5add919d` |
| `chromium.svg` | `129d6af41f1041493d3e3aeba786a05a72ea0350bdef98bd37f00318ee93acce` |
| `firefox.svg` | `3f3742d1e78c0db6c2d3531250f70c30675ee5310de32fa89befaf738c54efd0` |
| `brave.svg` | `efda38982b159ade8cd672a7196c82d01d1cb5c0162467c242484dfc03e6f76b` |
| `slack.svg` | `62e556a75b94516fd8dcfa9c8ee4eae76268b4087ee927d91a5f2d3115d54118` |
| `discord.svg` | `c04059c1d13d99239ebd0b7df807e8c3d2f46439bd18a70c5fd3a0631bd7b310` |

The shared SDK `Image::from_svg` API parses these into vector paths and
preserves the original accessible application name and 64 × 64 logical
footprint. It uses the same SVG subset as web content, including its
first-stop approximation for gradients; no app-local rasterizer is used.
