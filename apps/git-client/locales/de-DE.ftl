git-remote-running = Remote-Vorgang läuft…
git-remote-busy = Warte, bis der aktuelle Remote-Vorgang abgeschlossen ist.
git-remote-worker-stopped = Der Remote-Prozess wurde unerwartet beendet. Prüfe den Repository-Zustand vor einem erneuten Versuch.

# Screens
git-screen-changes = Änderungen
git-screen-history = Verlauf
git-screen-branches = Branches
git-app-description = Änderungen, Verlauf, Branches und Remotes des Repositorys
git-view = Ansicht

# Window bar
git-diff-unified-name = Einspaltiger Diff
git-diff-split-name = Diff nebeneinander
git-syntax-name = Syntaxhervorhebung
git-dismiss-error = Fehler schließen

git-switched = Zu { $name } gewechselt
git-switched-stashed = Zu { $name } gewechselt; die Änderungen liegen als { $stash } im Stash

# Confirmation
git-confirm-discard-file-title = Änderungen an { $name } verwerfen?
git-confirm-discard-hunk-title = Diese Änderung verwerfen?
git-confirm-delete-branch-title = { $name } löschen?
git-confirm-discard-untracked-body = Die Datei wird aus deinem Arbeitsverzeichnis gelöscht. Das lässt sich nicht rückgängig machen.
git-confirm-discard-body = Deine Änderungen gehen verloren. Das lässt sich nicht rückgängig machen.
git-confirm-delete-branch-body = Commits, die nur dieser Branch enthält, gehen verloren.
git-confirm-discard = Verwerfen
git-confirm-switch-title = Zu { $name } wechseln?
git-confirm-switch-body = Das Arbeitsverzeichnis ist sauber; es wechselt nur der Branch.
git-confirm-switch-dirty-body = Du hast nicht festgeschriebene Änderungen. Lege sie in den Stash und wechsle, oder nimm sie mit; eine Änderung, die überschrieben würde, lehnt beides ab.
git-switch-stash = In den Stash legen und wechseln
git-switch-carry = Wechseln und Änderungen mitnehmen
git-switch-go = Wechseln
git-cancel = Abbrechen

# Changes
git-section-staged = Vorgemerkt
git-section-unstaged = Nicht vorgemerkt
git-section-untracked = Nicht verfolgt
git-stage-all = Alle vormerken
git-unstage-all = Alle zurücknehmen
git-stage-all-name = Alle Änderungen vormerken
git-unstage-all-name = Alle Änderungen zurücknehmen
git-stage = Vormerken
git-unstage = Zurücknehmen
git-stage-path = { $path } vormerken
git-unstage-path = { $path } zurücknehmen
git-change-open = Diff von { $path } öffnen, { $kind ->
    [added] hinzugefügt
    [modified] geändert
    [deleted] gelöscht
    [renamed] umbenannt
    [untracked] nicht verfolgt
   *[conflicted] im Konflikt
}
git-commit = Committen
git-commit-to = Auf { $branch } committen
git-commit-message = Commit-Nachricht
git-no-changes = Keine Änderungen zum Committen
git-tree-clean = Dein Arbeitsverzeichnis ist sauber
git-select-file = Wähle eine Datei, um ihre Änderungen zu sehen

# Diff
git-stage-file = Datei vormerken
git-unstage-file = Datei zurücknehmen
git-discard-file = Datei verwerfen
git-discard-file-name = Änderungen an der Datei verwerfen
git-stage-hunk = Abschnitt vormerken
git-unstage-hunk = Abschnitt zurücknehmen
git-discard-hunk = Abschnitt verwerfen
git-renamed-from = Umbenannt von { $path }
git-binary-file = Binärdatei
git-binary-file-detail = Es gibt keinen Text zum Vergleichen.
git-mode-only = Keine Änderungen anzuzeigen
git-mode-only-detail = Nur die Dateirechte haben sich geändert.

# History
git-no-commits = Noch keine Commits
git-commit-row-name = { $summary } von { $author }, { $age }
git-fact-commit = Commit
git-fact-date = Datum
git-fact-parents = { $count ->
    [one] Vorgänger
   *[other] Vorgänger
}
git-date = { $day }. { $month ->
    [1] Januar
    [2] Februar
    [3] März
    [4] April
    [5] Mai
    [6] Juni
    [7] Juli
    [8] August
    [9] September
    [10] Oktober
    [11] November
   *[12] Dezember
} { $year }, { $time }
git-more-refs = +{ $count }
git-fact-refs = Branches und Tags
git-changed-files = Geänderte Dateien

# Branches
git-section-branches = Branches
git-section-remote-branches = Remote-Branches
git-section-tags = Tags
git-section-stashes = Stashes
git-section-remotes = Remotes
git-fetch-name = Vom Remote abrufen
git-pull-name = Vom Remote pullen
git-push-name = Zum Remote pushen
git-check-out = Auschecken
git-check-out-name = { $name } auschecken
git-delete-branch = Branch löschen
git-delete-name = { $name } löschen
git-apply-stash = Stash anwenden
git-apply-stash-name = Stash { $index } anwenden
git-create-branch = Branch erstellen
git-branch-name = Branch-Name
git-new-branch-name = Name des neuen Branches
git-tracking-none = { $current ->
    [yes] Ausgecheckt, kein Upstream-Branch
   *[no] Kein Upstream-Branch
}
git-tracking-even = { $current ->
    [yes] Ausgecheckt, auf dem Stand von { $upstream }
   *[no] Auf dem Stand von { $upstream }
}
git-tracking-ahead = { $current ->
    [yes] Ausgecheckt, { $ahead } vor { $upstream }
   *[no] { $ahead } vor { $upstream }
}
git-tracking-behind = { $current ->
    [yes] Ausgecheckt, { $behind } hinter { $upstream }
   *[no] { $behind } hinter { $upstream }
}
git-tracking-diverged = { $current ->
    [yes] Ausgecheckt, { $ahead } vor, { $behind } hinter { $upstream }
   *[no] { $ahead } vor, { $behind } hinter { $upstream }
}

# Open a repository
git-open-title = Repository öffnen
git-open-hint = Gib den Pfad eines Git-Repositorys auf diesem Computer ein.
git-open = Öffnen
git-choose = Auswählen…
git-choose-name = Repository-Ordner auswählen
git-choose-title = Repository auswählen
git-open-name = Repository öffnen
git-repository-path = Repository-Pfad

git-collection-repository = Repository

git-section-worktrees = Arbeitsverzeichnisse
git-create-worktree = Worktree erstellen
git-worktree-branch = Neuer Worktree-Branch
git-worktree-path = Absoluter Zielpfad
git-worktree-start = Ausgangspunkt (Branch, Tag oder Commit)
git-worktree-host-required = Ein Worktree benötigt ein aktives Repository.
git-worktree-running = Worktree wird erstellt…
git-worktree-read-failed = Arbeitsverzeichnisse konnten nicht gelesen werden
git-worktree-read-error = Arbeitsverzeichnisse konnten nicht gelesen werden: { $error }

## History of the record

git-client-git-client-repository = Repository { $new } geöffnet
git-client-record-repository = Zweige und Verlauf des Repositorys festgehalten

# Conflicts and a commit's changed files
git-conflict-resolved = { $side } für { $file } behalten
git-conflict-ours = unsere Seite
git-conflict-theirs = ihre Seite
git-conflict-take-ours = Unsere nehmen
git-conflict-take-theirs = Ihre nehmen
git-conflict-explainer = Diese Datei ist im Konflikt.
git-back-to-commit = Zurück zum Commit
git-commit-file-heading = { $file } in { $commit }
git-commit-file-name = { $file } ({ $kind })
git-show-commit-file = Die Änderungen dieser Datei im Commit zeigen

# Diff controls in the window bar
git-whitespace-name = Leerraum anzeigen
git-wrap-name = Lange Zeilen umbrechen
