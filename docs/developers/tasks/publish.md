# Build and submit a release

For local signing and namespace registration:

```bash
tilo-app keys new
tilo-app apply --domain example.com --contact you@example.com
tilo-app apply --status
```

Replace the example domain and contact with yours. Publish the proof line
where only you can write it; `apply` explains the accepted DNS/file locations.
App Publisher on Tilo OS provides the same key and application workflows.
Wait for namespace approval before submitting packages under it.

```bash
tilo-app package
tilo-app publish --metadata store.json
```

`store.json` contains localized `name`, `tagline`, `description`, optional
images and project URLs, and the app UI's `supported_languages`. Each text
map needs an `en` fallback; language lists are sorted and unique. Image URLs
use credential-free HTTPS and each image needs an `alt` description. See the
[publishing guide](../../publishing.md) for the exact schema and proof rules.

For the supported platform/architecture matrix:

```bash
tilo-app add ci --forge github --json
```

This writes a small caller and reusable `tilo-build.yml`. GitHub can import
that workflow from your own build repository at an exact commit. It builds
the caller's code with its pinned SDK; normal pushes and PRs check/package,
and the generated caller requests publication for `v*` tags. Publication is
off by default in the reusable workflow and refused for PR events.

Put the signing key in the `TILO_PUBLISHER_KEY` repository secret, never in
source. Every release still queues registry owner review; building or
uploading is not approval.
