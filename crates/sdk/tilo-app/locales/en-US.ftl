## A filed feedback report, read out line by line
# One picked part of the screen: its place, what it was, and the app.
feedback-report-pick = { $index }. { $what } in { $app }
# One picked part from an app whose name is not known: its place and what it
# was, never the app's id.
feedback-report-pick-alone = { $index }. { $what }
# What a picked part was, when the app said what kind of thing it is.
feedback-report-part-kind = { $kind }
# What a picked part was, by the app's own name for it and its role.
feedback-report-part-named = The “{ $name }” { $role ->
    [Button] button
    [ToggleButton] button
    [Link] link
    [Tab] tab
    [MenuItem] menu item
    [Checkbox] checkbox
    [Switch] switch
    [TextInput] field
    [MultilineTextInput] field
    [Heading] heading
   *[other] { $role }
}
# What a picked part was, known only by its role.
feedback-report-part-role = { $role ->
    [ListItem] A list item
    [Row] A row
    [Cell] A cell
    [Button] A button
    [ToggleButton] A button
    [Link] A link
    [Image] A picture
    [TextInput] A field
    [MultilineTextInput] A field
    [StaticText] A text
    [Paragraph] A paragraph
    [Heading] A heading
    [List] A list
    [Group] A group
    [Window] A window
    [Toolbar] A toolbar
    [Menu] A menu
    [MenuItem] A menu item
    [Tab] A tab
   *[other] A part
}
feedback-report-note = “{ $note }”
# One text a picked part showed, as reviewed.
feedback-report-text = { $text }
# One of the reporter's last actions, by the label its app gives it;
# $outcome is done, refused or failed.
feedback-report-action = { $outcome ->
    [refused] { $label } (refused)
    [failed] { $label } (failed)
   *[done] { $label }
}
# An action whose app gave it no label, by its id and resource kind.
feedback-report-action-unnamed = { $action } on { $resource }
feedback-report-recent = Just before: { $actions }
# The recording a report carries: the app whose record it recorded, the
# times it starts and ends, and how many changes it steps through.
feedback-report-recording = Recording of { $app } from { $from } to { $to }, { $steps ->
    [one] one change
   *[other] { $steps } changes
}
feedback-report-slow = Slow: { $slow } of the last { $frames } frames over { $budget } ms, the slowest { $worst } ms
# A hidden span of a reported text, by what it hid.
feedback-report-hidden = { $kind ->
    [name] ‹name›
    [email] ‹email›
    [phone] ‹phone›
    [address] ‹address›
    [account] ‹account number›
    [credential] ‹credential›
   *[personal] ‹personal›
}
