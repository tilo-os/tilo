git-remote-running = Remote-Vorgang läuft…
git-remote-busy = Warte, bis der aktuelle Remote-Vorgang abgeschlossen ist.
git-remote-worker-stopped = Der Remote-Prozess wurde unerwartet beendet. Prüfe den Repository-Zustand vor einem erneuten Versuch.
git-remote-fetching = Wird abgerufen…
git-remote-pulling = Wird gepullt…
git-remote-pushing = Wird gepusht…
git-error-no-repository = Kein Git-Repository unter { $path }

# Screens
git-screen-changes = Änderungen
git-screen-history = Verlauf
git-screen-branches = Branches
git-app-description = Änderungen, Verlauf, Branches und Remotes des Repositorys

# Header
git-diff-unified = Einspaltig
git-diff-split = Geteilt
git-diff-unified-name = Einspaltiger Diff
git-diff-split-name = Diff nebeneinander
git-syntax = Syntax
git-syntax-name = Syntaxhervorhebung
git-sidebar-show = Seitenleiste einblenden
git-sidebar-hide = Seitenleiste ausblenden
git-dismiss-error = Fehler schließen

# Confirmation
git-confirm-discard-file-title = Änderungen an { $name } verwerfen?
git-confirm-discard-hunk-title = Diese Änderung verwerfen?
git-confirm-delete-branch-title = { $name } löschen?
git-confirm-discard-untracked-body = Die Datei wird aus deinem Arbeitsverzeichnis gelöscht. Das lässt sich nicht rückgängig machen.
git-confirm-discard-body = Deine Änderungen gehen verloren. Das lässt sich nicht rückgängig machen.
git-confirm-delete-branch-body = Commits, die nur dieser Branch enthält, gehen verloren.
git-confirm-discard = Verwerfen
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
git-fact-refs = Branches und Tags
git-changed-files = Geänderte Dateien

# Branches
git-section-branches = Branches
git-section-remote-branches = Remote-Branches
git-section-tags = Tags
git-section-stashes = Stashes
git-section-remotes = Remotes
git-fetch = Abrufen
git-pull = Pullen
git-push = Pushen
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

# Palette
git-action-open-diff = Diff öffnen
git-action-open-commit = Commit öffnen
git-action-check-out-branch = Branch auschecken
git-action-fetch-remote = Remote abrufen
git-action-layout-split = Diff-Ansicht: nebeneinander
git-action-layout-unified = Diff-Ansicht: einspaltig
git-action-syntax-off = Syntaxhervorhebung ausschalten
git-action-syntax-on = Syntaxhervorhebung einschalten
git-action-whitespace-hide = Leerzeichen ausblenden
git-action-whitespace-show = Leerzeichen anzeigen
git-action-wrap-off = Lange Zeilen nicht umbrechen
git-action-wrap-on = Lange Zeilen umbrechen
git-action-show-changes = Änderungen anzeigen
git-action-show-history = Verlauf anzeigen
git-action-show-branches = Branches und Remotes anzeigen
git-action-commit = Vorgemerkte Änderungen committen
git-action-commit-description = Einen Commit aus den vorgemerkten Änderungen erstellen
git-action-stash = Änderungen im Arbeitsverzeichnis stashen
git-action-stash-message = Stash-Nachricht
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
