# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-diffmerge = Diff & Merge

d-so-tilo-diffmerge-keywords = diff, merge, compare, comparison, three-way, three-way merge, conflict, patch, diff tool, vergleich, zusammenführen, konflikt

d-rb35mnfw = Block
    .doc = One change of a comparison: the aligned rows it spans, and the first line they touch.
    .noun = { $count ->
        [one] block
       *[other] blocks
    }

d-csjrkfwp = First

d-cjhoemux = Last

d-w2shjabm = Kind
    .doc = `added` (only the right has these lines), `removed` (only the left) or `modified`.

d-vwniktmt = Preview

d-rb35mnfw-filter-first = First

d-rb35mnfw-filter-last = Last

d-h6nml6re = Hunk
    .doc = One change of a merge: the lines each side gives it, and how the person took it.
    .noun = { $count ->
        [one] hunk
       *[other] hunks
    }

d-47hfb7ea = Index
    .doc = Its place in the merge's hunks, which is what `take_ours` and its siblings take.

d-4qm5ujaf = Resolution
    .doc = `conflict` while the person has not said, `auto-ours` and `auto-theirs` for a side only one of them moved, and `ours`, `theirs`, `both` or `written` for what they said.

d-m2r6lioa = Base

d-msvfenbm = Ours

d-ojqeurrr = Theirs

d-h6nml6re-filter-index = Index

d-l7lqtoxg = Folder entry
    .doc = One file that differs between two folder trees.
    .noun = { $count ->
        [one] folder entry
       *[other] folder entries
    }

d-usttvqtv = Path

d-q6kpasi4 = Kind
    .doc = `added` (only the left tree has it), `removed` (only the right) or `modified`.

d-u5jul62r = Left len

d-4in5gnvl = Right len

d-dcnmuoj3 = By length
    .doc = The files were too large to compare by content, so only their lengths were compared.

d-l7lqtoxg-filter-left_len = Left len

d-l7lqtoxg-filter-right_len = Right len

d-l7lqtoxg-filter-by_length = By length

d-ybdnnlsv = Session
    .doc = One comparison or merge: the texts a person opened, what the device made of them, and the merged result everyone edits.
    .noun = { $count ->
        [one] session
       *[other] sessions
    }
    .untitled = Untitled comparison

d-j6uarraa = Name

d-odsfatc7 = Left label
    .doc = The two sides of a comparison, and the alignment the device made of them: one line per aligned row, an empty line where a side has none.

d-kcdcrgzi = Left

d-3q7df7eg = Right label

d-prwrxrg4 = Right

d-ebpdel6u = Left aligned

d-vjlsurzf = Right aligned

d-7obphfng = Kinds
    .doc = One character per aligned row: ` ` equal, `a` added, `r` removed, `m` modified.

d-ogpmdy55 = Blocks

d-ie5v4a4i = Added

d-hv4ba7bq = Removed

d-6a2srqu5 = Modified

d-2f7twk55 = Identical

d-34arfoar = Compared

d-mhtp452w = Base label
    .doc = The three sides of a merge, and the merged result everyone edits.

d-5ozczfsi = Base

d-czecclw6 = Ours label

d-75vbkvt3 = Ours

d-6iwqekxm = Theirs label

d-zm3cqlts = Theirs

d-c7es3yev = Merged

d-twukcquq = Composed
    .doc = What the merged result was when the sides last composed it: a hand edit leaves it behind.

d-gvqybypj = Hunks

d-bolkamra = Changes

d-mjnnfnbm = Conflicts

d-n7puwfba = Merging

d-yif52d5b = Folder choice left
    .doc = The two folders the person chose to compare, and the last comparison made of them.

d-cr3zcxiz = Folder choice right

d-w5y6trs7 = Folder left label

d-7i3hhhy2 = Folder right label

d-mbawdpwr = Folder left root

d-utsx5nqp = Folder right root

d-tn6u7t4i = Folder entries

d-7ceymc4l = Folder truncated

d-z4cg3ljy = Folding

d-toc5vveo = Saved path
    .doc = Where the merged result was last written, and what it was then.

d-avjlapbd = Saved name

d-nkku72cz = Saved

d-ttqrgoca = Notice
    .doc = What the window last said: a result, or what went wrong.

d-dr64yk6f = Notice kind

d-ybdnnlsv-filter-added = Added

d-ybdnnlsv-filter-removed = Removed

d-ybdnnlsv-filter-modified = Modified

d-ybdnnlsv-filter-identical = Identical

d-ybdnnlsv-filter-compared = Compared

d-ybdnnlsv-filter-changes = Changes

d-ybdnnlsv-filter-conflicts = Conflicts

d-ybdnnlsv-filter-merging = Merging

d-ybdnnlsv-filter-folder_truncated = Folder truncated

d-ybdnnlsv-filter-folding = Folding

d-joaluvq2 = Library
    .doc = The workspace's Diff & Merge: the comparisons and merges its windows opened.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-tzmh25hr = Sessions

d-htkhnxs4 = Open ask
    .noun = { $count ->
        [one] open ask
       *[other] open asks
    }

d-ym5ajbxk = Path

d-ou5hf2bx = Address

d-qqwavfqy = Name

d-bliesnie = Opened answer
    .noun = { $count ->
        [one] opened answer
       *[other] opened answers
    }

d-f4rrknsp = Label

d-ced6vdp4 = Text

d-bk2hllx2 = Diff ask
    .noun = { $count ->
        [one] diff ask
       *[other] diff asks
    }

d-vz6ogvrp = Left

d-3upzelrr = Right

d-df4q7mdq = Diff answer
    .noun = { $count ->
        [one] diff answer
       *[other] diff answers
    }

d-hahxq35a = Block firsts
    .doc = One list per fact of a change, so a change is a place among them.

d-jx4fqkfc = Block lasts

d-we3fy74x = Block kinds

d-xmvhnvex = Block previews

d-fejavrie = Left

d-zmrjesu2 = Right

d-psvy4e5w = Rows

d-akgopqc4 = Added

d-2wokc6qk = Removed

d-w6mvjrvf = Modified

d-frxu6oxq = Identical

d-df4q7mdq-filter-added = Added

d-df4q7mdq-filter-removed = Removed

d-df4q7mdq-filter-modified = Modified

d-df4q7mdq-filter-identical = Identical

d-44hld4lv = Merge ask
    .noun = { $count ->
        [one] merge ask
       *[other] merge asks
    }

d-vx52jl2g = Base

d-mrxlgkxk = Ours

d-nvyd6cgn = Theirs

d-2ll3o4vg = Merge answer
    .noun = { $count ->
        [one] merge answer
       *[other] merge answers
    }

d-2pwajyjm = Hunk indices
    .doc = One list per fact of a change, so a change is a place among them.

d-nyvwoswu = Hunk resolutions

d-imh4txwm = Hunk bases

d-fra5wt4r = Hunk ours

d-y56numrx = Hunk theirs

d-h6l3j5y3 = Merged

d-e7auw454 = Changes

d-oqot5haf = Conflicts

d-2ll3o4vg-filter-changes = Changes

d-2ll3o4vg-filter-conflicts = Conflicts

d-eq6hshlj = Compose ask
    .noun = { $count ->
        [one] compose ask
       *[other] compose asks
    }

d-55fy4avh = Base

d-fxqifsds = Ours

d-6te7azkd = Theirs

d-atnd6awc = Choices

d-weznhatv = Written

d-6rlq6x7d = Composed answer
    .noun = { $count ->
        [one] composed answer
       *[other] composed answers
    }

d-wino2rjt = Merged

d-niqoebpg = Changes

d-gzqibeka = Conflicts

d-6rlq6x7d-filter-changes = Changes

d-6rlq6x7d-filter-conflicts = Conflicts

d-ulxmmmdn = Folders ask
    .noun = { $count ->
        [one] folders ask
       *[other] folders asks
    }

d-jasvo2t2 = Left

d-2go4z2dd = Right

d-m2u5dv2u = Folders answer
    .noun = { $count ->
        [one] folders answer
       *[other] folders answers
    }

d-uytgg4wc = Left label

d-bxame25h = Left root

d-ewdcrys5 = Right label

d-ogpczjbg = Right root

d-wb25c6a6 = Entry paths
    .doc = One list per fact of a file, so a file is a place among them.

d-n2bmepxt = Entry kinds

d-bbb24qp5 = Entry left lens

d-mldndym5 = Entry right lens

d-7nr427ip = Entry by lengths

d-hqjkq6oq = Truncated

d-m2u5dv2u-filter-truncated = Truncated

d-eovvp7o6 = Save ask
    .noun = { $count ->
        [one] save ask
       *[other] save asks
    }

d-t2ifxuue = Path

d-zoq67abl = Text

d-gjccvifa = Saved answer
    .noun = { $count ->
        [one] saved answer
       *[other] saved answers
    }

d-tmjabrvg = Name

d-xllsrvhk = Sample ask
    .noun = { $count ->
        [one] sample ask
       *[other] sample asks
    }

d-ol4drxwd = Kind

d-ganpofmm = Sample diff
    .noun = { $count ->
        [one] sample diff
       *[other] sample diffs
    }

d-uj6xvzvl = Left label

d-cjwd2i2z = Left

d-vbhvxj6f = Right label

d-whunzkq3 = Right

d-6pi2tse7 = Sample merge
    .noun = { $count ->
        [one] sample merge
       *[other] sample merges
    }

d-opwxdlzs = Base label

d-l3kqbcv7 = Base

d-wxfngzy5 = Ours label

d-ubqyr5rh = Ours

d-amhqg7cn = Theirs label

d-wfaxbaim = Theirs

d-4npoq5w6 = New comparison
    .doc = Starts an empty comparison: a place to open two files in.
    .did = started a comparison

d-3zvrc4ue = Compare two texts
    .doc = Compares two texts: what a script, an agent or another app asks of Diff & Merge.
    .did = compared two texts

d-3zvrc4ue-left_label = Left label

d-3zvrc4ue-left = Left

d-3zvrc4ue-right_label = Right label

d-3zvrc4ue-right = Right

d-u3q77h6r = Open files
    .doc = Opens the two files at `left` and `right` on this device and compares them.
    .did = opened two files
    .w-the-left-and-right-paths-are = the left and right paths are given together, or not at all

d-u3q77h6r-left = Left

d-u3q77h6r-right = Right

d-ybuyyio5 = Open one file
    .doc = Opens one file on this device against nothing: what a folder comparison shows for a file only one tree holds.
    .did = opened a file

d-ybuyyio5-path = Path

d-ybuyyio5-side = Side

d-a5bd6gjr = Choose a file to compare
    .doc = The action the person's file chooser fills for the left side: the left file, and then the right one.

d-a5bd6gjr-file = File

d-ho2xs7xs = Choose a file to compare
    .doc = The action the person's file chooser fills for the right side.

d-ho2xs7xs-file = File

d-mplbwhfp = Refresh comparison
    .doc = Asks the device to align the two sides, and holds what it made: the aligned columns, the rows that changed and how many of each.

d-dig6frbi = Merge three texts
    .doc = Merges three texts: a common base and the two versions of it a person has.
    .did = opened a merge

d-dig6frbi-base_label = Base label

d-dig6frbi-base = Base

d-dig6frbi-ours_label = Ours label

d-dig6frbi-ours = Ours

d-dig6frbi-theirs_label = Theirs label

d-dig6frbi-theirs = Theirs

d-hyqhq7vc = Refresh merge
    .doc = Asks the device to hunk the merge and compose the result with what the person took so far.

d-gxa4tbvv = Take ours
    .doc = Takes our side for a change.
    .did = took our side of a change

d-gxa4tbvv-hunk = Hunk

d-edz7ig5d = Take theirs
    .doc = Takes their side for a change.
    .did = took their side of a change

d-edz7ig5d-hunk = Hunk

d-orktsg7l = Take both
    .doc = Takes both sides for a change, ours first.
    .did = took both sides of a change

d-orktsg7l-hunk = Hunk

d-piejioa3 = Resolve with my own text
    .doc = Resolves a change with the person's own text.
    .did = resolved a change by hand
    .w-the-merged-result-was-edited-by = the merged result was edited by hand; resolve the rest in it

d-piejioa3-hunk = Hunk

d-piejioa3-text = Text

d-ju7jzjp2 = Take

d-ju7jzjp2-hunk = Hunk

d-ju7jzjp2-resolution = Resolution

d-dr6wocoa = Compose into

d-i2hnabtp = Compare folders
    .doc = Compares the two folders at `left` and `right` on this device: what only one holds, and what both hold but differ in.
    .did = compared two folders
    .w-the-left-and-right-folder-paths = the left and right folder paths are given together, or not at all
    .w-choose-the-two-folders-at-the = choose the two folders at the window

d-i2hnabtp-left = Left

d-i2hnabtp-right = Right

d-hidn6xge = Hold folders

d-hidn6xge-answer = Answer

d-vhcex4xq = Chosen folder left
    .doc = The action the person's folder chooser fills for the left tree.

d-vhcex4xq-folder = Folder

d-ar4jpl46 = Chosen folder right
    .doc = The action the person's folder chooser fills for the right tree: both chosen, the folders are compared.

d-ar4jpl46-folder = Folder

d-pt6uy5oc = Open the file
    .doc = Opens one file of the folder comparison: both its sides when both trees hold it, else the one that does, against nothing.
    .did = opened a file a folder comparison found

d-pt6uy5oc-entry = Entry

d-7ngrbhzk = Write the merged result
    .doc = Replaces the merged result: what a person types into it, and what an agent or a script writes when the result is theirs to change.
    .did = wrote the merged result
    .w-no-merge-is-open-so-there = no merge is open, so there is no merged result to write

d-7ngrbhzk-text = Text

d-rmbq73ft = Save merged result
    .doc = Writes the merged result to `path` on this device.
    .did = saved the merged result
    .w-no-merge-is-open-so-there = no merge is open, so there is no merged result to save
    .w-the-merged-result-is-saved-to = the merged result is saved to a path, or through the file chooser

d-rmbq73ft-path = Path

d-e2sbbkxz = Save bytes
    .doc = The merged result as a file the person places with the system's save chooser, which the window shows.

d-yrw32qav = Load sample
    .doc = Opens the sample comparison and the sample merge, so a person sees how each reads before they have two files at hand.
    .did = opened the sample

d-boukprzo = Load the sample comparison
    .doc = Fills a comparison with the samples: the two-way comparison and the three-way merge, so a person sees how each reads before they have two files at hand.
    .did = opened the sample

d-v5nirqau = Show the sample folders
    .doc = Shows the sample folder comparison: no disk is read for it.

d-ehbmbhgm = Refuse

d-ehbmbhgm-why = Why

d-6d7v4sf5 = Close the comparison

d-6d7v4sf5-s = S

d-kpjayokp = Clear notice

d-yn3g5osr = Open files~present

d-gruep3mp = Resolve~present

d-mlfvdm4x = Compare folders~present

d-p2ummvz2 = Edit merged~present

d-syfa37pn = Save merged~present

d-vs7efbe6 = Library
    .w-diff-merge = Diff & Merge
    .w-compare-files = Compare files
    .w-more = More
    .w-load-sample = Load sample
    .w-open-two-files-to-compare-them = Open two files to compare them
    .w-start-a-comparison-then-choose-the = Start a comparison, then choose the two files in its window; or load a sample to see how one reads.
    .w-comparisons = Comparisons
    .w-close = Close
    .w-untitled-comparison = Untitled comparison
    .w-merge = Merge
    .w-folders = Folders
    .w-comparison = Comparison

d-kxbrjtkd = Session window
    .w-untitled-comparison = Untitled comparison
    .w-diff = Diff
    .w-merge = Merge
    .w-folders = Folders
    .w-previous-change = Previous change
    .w-next-change = Next change
    .w-open = Open
    .w-compare-files = Compare files…
    .w-choose-the-right-file = Choose the right file…
    .w-compare-folders = Compare folders…
    .w-load-sample = Load sample
    .w-save = Save…
    .w-save-to = Save to { $saved_name }
    .w-dismiss = Dismiss
    .w-no-merge-open = No merge open
    .w-a-three-way-merge-starts-from = A three-way merge starts from a common base and two edited versions of it. Load a sample to try one.
    .w-base = Base
    .w-ours = Ours
    .w-theirs = Theirs
    .w-changes = Changes
    .w-the-three-sides-agree-the-merged = The three sides agree: the merged result needs nothing.
    .w-change = Change { $value }
    .w-take-ours = Take ours
    .w-take-theirs = Take theirs
    .w-take-both = Take both
    .w-my-own-lines-for-this-change = My own lines for this change
    .w-resolve = Resolve
    .w-write-my-own = Write my own
    .w-open-two-files-to-compare-them = Open two files to compare them
    .w-choose-the-left-file-then-the = Choose the left file, then the right one, or load a sample to see how a comparison reads.
    .w-choose-the-left-file = Choose the left file
    .w-choose-the-right-file-2 = Choose the right file
    .w-the-files-are-identical = The files are identical
    .w-every-line-of-and-agrees = Every line of { $left_label } and { $right_label } agrees.
    .w-compare-other-files = Compare other files
    .w-left = Left
    .w-right = Right
    .w-left-2 = left
    .w-right-2 = right
    .w-and = { $left_label } and { $right_label }
    .w-both = both
    .w-lines = lines { $value }–{ $value2 }
    .w-added-removed-modified = { $count } added, { $count2 } removed, { $count3 } modified
    .w-no-changes-to-merge = No changes to merge
    .w-changes-resolved-conflicts-left = { $count } changes, { $count2 } resolved, { $count3 } conflicts left
    .w-no-files-open = No files open
    .w-added-removed-modified-change-of = { $count } added, { $count2 } removed, { $count3 } modified. Change { $count4 } of { $count5 }

d-p7mn4tzu = Source head

d-lbu6ypgz = Merged section
    .w-merged-result = Merged result
    .w-merged-result-conflicts-left = Merged result · { $count } conflicts left
    .w-the-merged-result-was-edited-by = The merged result was edited by hand: the rest is resolved in it, and taking a side would replace your edit.
    .w-save = Save…
    .w-save-to = Save to { $saved_name }
    .w-saved = Saved
    .w-unsaved = Unsaved

d-yt5batot = Folders page
    .w-compare-two-folders = Compare two folders
    .w-choose-two-folders-to-list-the = Choose two folders to list the files only one holds and the files both hold but differ in.
    .w-choose-the-left-folder = Choose the left folder
    .w-show-the-sample-folders = Show the sample folders
    .w-and-files-differ = { $folder_left_label } and { $folder_right_label }: { $count } files differ
    .w-choose-the-folders-again = Choose the folders again
    .w-only-the-first-part-of-the = Only the first part of the folders was compared
    .w-files-that-differ = Files that differ
