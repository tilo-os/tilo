# Tilo patch: a context published over another leaves it underneath

`ndk-context` 0.1.1 holds one context and asserts on publishing that none is there.
android-activity publishes the activity on its own thread before `android_main` runs, and takes
it back once `android_main` returns. A Tilo app publishes the application's `Context` before
that, as its file provider or its activity is created, because the Keystore holding the device's
key is reached before any window opens, and when no window ever opens. Under the assert the
application's context had to be taken back before android-activity published the activity, and
nothing was published again once the activity's was taken back. A crate reading the context in
between (netdev reading an interface's counters while the device's endpoint binds, on the
phone's start after sign-in) found none, panicked, and ended the process.

`src/lib.rs` now holds the published contexts as a stack under a lock: publishing one over
another keeps the earlier one underneath, `android_context` returns the one published last, and
releasing it returns the one under it again. The application's context stays published for the
process, and the activity's takes its place in one step while `android_main` runs. Pinned by
`android::tests::the_application_context_stays_under_the_activity` in `tilo-process`.

The single slot was also a `static mut` written and read from any thread without
synchronisation; the lock orders the reads and the publishing.

Everything else is crates.io `ndk-context` 0.1.1, verbatim. Remove this copy once a release
lets a context be published over another.
