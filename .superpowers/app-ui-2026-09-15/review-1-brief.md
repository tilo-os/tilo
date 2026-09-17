# App UI review group 1

Read-only review. Do not fix code, commit, or spawn agents. User requires every app reviewed/marked BEFORE fix agents are dispatched.

Apps: apps/agent, apps/api_tester, apps/backups, apps/browser, apps/calculator, apps/calendar, apps/camera, apps/chat, apps/code_editor, apps/contacts, apps/containers, apps/db_client, apps/diffmerge, apps/disks, apps/ebook, apps/feeds

Review actual primary screen composition and main user route. Look for crowded unrelated objects, inconsistent related/section spacing, excessive whitespace or chrome, mismatched alignment, information overload, weak hierarchy/primary action, narrow/200% limitations. Different apps need different density: editor/canvas apps should maximize workspace; settings should have readable groups. Identify shared framework cause vs app-authored constraint. Existing Store no-edit restriction is superseded by new user request, but this phase is review-only. Preserve unrelated dirty files; scratchpad is untracked work from another task and review-only for now.

Controller is building current binaries and generating screenshots into .superpowers/app-ui-2026-09-15/captures; coordinate with root for ready notice. While waiting inspect source, inventory screenshot entrypoints and primary user-flow states/fixtures. Do not independently build or run cargo (controller owns build queue). Never claim visual review from source alone or from unverified historical shots. You may read existing captures as historical context, explicitly label them. View actual fresh images with view_image when available, individually at sufficient resolution.

Write report .superpowers/app-ui-2026-09-15/review-1-report.md with one entry PER assigned app: status (visual-reviewed/source-only/blocked/windowless), exact capture path/provenance, main flow, concrete numbered prioritized findings with source file:line, desired visible change, framework/app ownership, and needed verification. Include even apps with no identified issue; don't invent findings. List shared patterns at end. Report back compactly with critical findings and missing visual evidence. This is a review/mark phase, not implementation.
