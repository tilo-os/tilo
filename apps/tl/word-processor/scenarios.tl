/// A letter, half written: the template a person starts from, with the first paragraph written.
scenario "a letter" {
    let d = root Document
    d.title = "Letter to the landlord"
    d.started = "letter"
    d.body = RichText.from_markdown(
        "# Letter\n\nAda Lovelace\n12 Analytical Way\nLondon\nada@example.org\n\n23 June 2026\n\nThe Property Office\n8 Counting Street\nLondon\n\nDear Sir or Madam,\n\nThe radiator in the front room has not worked since the start of the month. I wrote on the second of June and have not had an answer.\n\nSincerely,\n\nAda Lovelace\n",
    )
}

/// A report with its headings, its list and its table: what a person writes in a morning, and
/// what the outline and the table of contents read.
scenario "a report" {
    let d = root Document
    d.title = "Q3 report"
    d.started = "report"
    d.body = RichText.from_markdown(
        "# Q3 report\n\n**Author:** Ada Lovelace\n**Date:** 23 June 2026\n**Status:** Draft\n\n## Executive Summary\n\nThe quarter closed four points over plan, mostly on the strength of two large workspaces that renewed early. Support load stayed flat while the number of workspaces grew by a fifth.\n\n## Background\n\nWe planned for three new workspaces a month and closed two a week from July. The change was not a campaign: it came from the two teams who moved their notes over in the spring and told others.\n\n## Findings\n\n### Renewals came early\n\nThe two largest accounts renewed in July, a quarter before they had to. Both named the shared notebook as the reason.\n\n### Support stayed flat\n\nTickets per workspace fell from 0.8 to 0.6. The two bugs that caused most of them were fixed in June.\n\n| Month | Workspaces | Tickets |\n| --- | --- | --- |\n| July | 41 | 24 |\n| August | 47 | 26 |\n| September | 52 | 27 |\n\n## Recommendations\n\n- Write the migration guide for teams moving their notes in.\n- Keep the support rotation as it is; it is carrying the growth.\n- Ask the two renewing teams what would have made them renew later.\n",
    )
    // Its table of contents is a field, drawn from the headings as they stand.
    invoke(generate_toc(d))
}

/// A draft two people are reading: one comment waiting, one answered, one taken care of.
scenario "a commented draft" {
    let d = root Document
    d.title = "Launch note"
    d.body = RichText.from_markdown(
        "# Launch note\n\nWe are opening the lounge to everyone in the alpha on the first of next month. The first week is invite only, so the people who waited longest come first.\n\n## What opens\n\nThe lounge, the notes app and the shared files. Chat stays with the first group until November.\n\n## What we ask\n\nRead it once on your phone before the morning it opens.\n",
    )
    let ada = person("Ada")
    let grace = person("Grace")
    // One remark is anchored to the words it is about: its mark is in the page and its marker
    // stands in the margin beside it.
    invoke(
        comment_on(d, RichText("Say which morning, in which zone."), at: "¶2", start: 0, end: 2),
        by: grace,
        at: 2026-06-23T09:12Z,
    )
    let answered = invoke(
        comment_on(d, RichText("Is the notes app the old one or the new one?")),
        by: ada,
        at: 2026-06-23T09:20Z,
    )
    invoke(
        discussion.reply(answered, RichText("The new one: the shared notebook.")),
        by: grace,
        at: 2026-06-23T09:31Z,
    )
    let settled = invoke(
        comment_on(d, RichText("The header should say the month.")),
        by: ada,
        at: 2026-06-23T08:40Z,
    )
    invoke(discussion.resolve(settled, true), by: ada, at: 2026-06-23T08:55Z)
}

/// A document people reacted to, as the bar's pills show.
scenario "reactions" {
    let d = root Document
    d.title = "Thank you"
    d.body = RichText.from_markdown(
        "# Thank you\n\nFor a year of patient reports, careful screenshots and the two of you who rebuilt the importer on a weekend. The lounge stands because you kept telling us what broke.\n",
    )
    let ada = person("Ada")
    let grace = person("Grace")
    let leo = person("Leo")
    invoke(react(d, "🎉"), by: ada, at: 2026-06-22T10:00Z)
    invoke(react(d, "🎉"), by: grace, at: 2026-06-22T10:04Z)
    invoke(react(d, "🎉"), by: leo, at: 2026-06-22T10:09Z)
    invoke(react(d, "❤️"), by: grace, at: 2026-06-22T10:05Z)
}
