# Browser continuity host boundary

The portable UI retains the basic WebView API for every app. It passes native
engine launches to `engine_host`, which removes app loader overrides and
excludes the launcher's reviewed migration projection before exec on Linux.
A missing projection needs no exclusion; an invalid projection or unavailable
mandatory confinement refuses launch. Other hosts refuse an unexpected
projection instead of silently launching an unconfined engine.

The same factory serves ordinary, private and dedicated service-session
engines. Session identity checks, import replies, child-exit confirmation and
Forget cleanup remain in the existing engine/profile lifecycle. Moving native
launch policy does not authorize reading migration files or expose credentials
through the rendering protocol.

Reviewed account-session imports use the Browser app's optional `secrets`
capability; ordinary browsing requires only its existing WebView/network
capabilities. Source selection and profile storage still require their typed,
host-admitted grants. A path or session identifier alone grants no authority.
Public avatars come only from the launcher's bound identity broker and never
trigger account-name discovery, config fallback or signing-share unlock.
