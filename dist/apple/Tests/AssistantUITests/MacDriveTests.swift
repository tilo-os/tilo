#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The Mac app, made as its program makes it and driven while it runs, with nothing on a
/// screen. `Drive.swift` says how, and what that leaves unproven.
@MainActor
final class MacDriveTests: XCTestCase {
    private func place(of words: String, in text: String) -> Int {
        (text as NSString).range(of: words).location
    }

    // MARK: The message field

    func testTheFieldTakesWhatIsTypedFormatsItInPlaceAndReturnSendsEveryCharacter() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive, field = try run.field()
        drive.click(field)
        XCTAssertTrue(run.window.firstResponder === field, "a click in the field gives it the keyboard")

        let typed = "Tell **Maya** the `q3` deck is ~~late~~ ready"
        drive.type(typed)
        await drive.settle()
        XCTAssertEqual(field.string, typed, "every character stays as typed")
        XCTAssertEqual(run.model.drafts[.main]?.text, typed)
        let storage = try XCTUnwrap(field.textStorage)
        func font(_ words: String) throws -> NSFont {
            try XCTUnwrap(storage.attribute(.font, at: place(of: words, in: typed), effectiveRange: nil) as? NSFont)
        }
        XCTAssertTrue(NSFontManager.shared.traits(of: try font("Maya")).contains(.boldFontMask), "bold shows as bold while it is typed")
        XCTAssertLessThan(try font("**Maya").pointSize, 1, "its signs are out of sight once the caret has left the word")
        XCTAssertTrue(try font("q3").isFixedPitch)
        XCTAssertNotNil(storage.attribute(FieldStyle.ground, at: place(of: "q3", in: typed), effectiveRange: nil))
        XCTAssertNotNil(storage.attribute(.strikethroughStyle, at: place(of: "late", in: typed), effectiveRange: nil))
        XCTAssertNil(storage.attribute(.strikethroughStyle, at: place(of: "ready", in: typed), effectiveRange: nil))

        // Shift Return makes a line and sends nothing. A list, and an emoji.
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1, [.shift])
        drive.type("1. first 😀")
        await drive.settle()
        let whole = typed + "\n1. first 😀"
        XCTAssertEqual(field.string, whole)
        XCTAssertEqual(run.source.calls("send"), [], "Shift Return sends nothing")
        let entry = try XCTUnwrap(storage.attribute(.paragraphStyle, at: place(of: "first", in: whole), effectiveRange: nil) as? NSParagraphStyle)
        XCTAssertGreaterThanOrEqual(entry.headIndent, 20, "a list's line hangs under its number as soon as it is one")
        drive.picture("mac-driven-field-typed")

        // Return sends what was typed, Markdown's characters and all.
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("the message to be sent") { run.source.calls("send").count == 1 }
        XCTAssertEqual(run.source.calls("send"), ["send(\(String(reflecting: whole)), files: [], main)"])
        await drive.until("the field to be empty again") { field.string.isEmpty }
        XCTAssertNil(run.model.drafts[.main])
        guard case .message(let sent)? = run.source.inner.items().last?.content else { return XCTFail("no message arrived") }
        XCTAssertEqual(sent.text, whole)
        XCTAssertEqual(sent.author, .person)
        await drive.until("the message in the conversation") { drive.shows("Tell Maya the q3 deck is late ready") }
        // An empty field sends nothing.
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        drive.type("   ")
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.settle(200)
        XCTAssertEqual(run.source.calls("send").count, 1)
    }

    func testAPastedParagraphAndALongTextThatScrollsInsideTheField() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive, field = try run.field()
        drive.click(field)
        // What Paste does, from a pasteboard of the test's own: the person's is neither read nor written.
        let general = NSPasteboard.general.changeCount
        let board = NSPasteboard(name: NSPasteboard.Name("so.tilo.assistant.tests.\(UUID().uuidString)"))
        defer { board.releaseGlobally() }
        let paragraph = "A paragraph copied elsewhere,\nwith a second line and *emphasis*."
        board.clearContents()
        board.setString(paragraph, forType: .string)
        XCTAssertTrue(field.readSelection(from: board, type: .string))
        await drive.settle()
        XCTAssertEqual(field.string, paragraph)
        XCTAssertEqual(run.model.drafts[.main]?.text, paragraph, "what is pasted is in the draft as it was copied")
        let scroll = try XCTUnwrap(field.enclosingScrollView)
        let twoLines = scroll.frame.height
        XCTAssertEqual(twoLines, 44, accuracy: 1.5, "the field is as tall as its two lines")

        // A long text: the field grows to its tallest and then scrolls inside itself.
        for line in 1...16 {
            drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1, [.shift])
            drive.type("line \(line)")
        }
        await drive.settle(300)
        XCTAssertEqual(scroll.frame.height, MarkdownField.tallest, accuracy: 0.5)
        XCTAssertGreaterThan(field.frame.height, MarkdownField.tallest + 100, "the words are taller than the field")
        XCTAssertEqual(scroll.documentVisibleRect.maxY, field.frame.height, accuracy: 2, "the line being typed is in view")
        let inWindow = scroll.convert(scroll.bounds, to: nil)
        XCTAssertGreaterThan(inWindow.minY, 30, "the field stays inside the window with room under it")
        XCTAssertTrue(drive.has(AssistantWords.newSubAgent.said, role: "AXButton"), "the window's bar is still there over it")
        XCTAssertEqual(run.source.calls("send"), [])
        drive.picture("mac-driven-field-long")

        // Emptied, it is one line tall again.
        XCTAssertTrue(drive.key("a", [.command]).contains(AssistantWords.selectAll.said))
        drive.key(Drive.Key.delete.0, code: Drive.Key.delete.1)
        await drive.settle(300)
        XCTAssertEqual(field.string, "")
        XCTAssertEqual(scroll.frame.height, 22, accuracy: 1.5)
        XCTAssertEqual(NSPasteboard.general.changeCount, general, "the person's own pasteboard was not written")
    }

    // MARK: The sidebar and the sub-agents

    func testTheSidebarOpensASubAgentsWindowAndComesBack() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        XCTAssertTrue(drive.node(AssistantWords.mainChat.said, role: "AXButton")?.isSelected ?? false)
        XCTAssertTrue(drive.has(AssistantWords.tasks.said, role: "AXHeading"), "the Tasks panel stands beside the main chat")

        drive.press("Usage drop: investigate, Coder, working", role: "AXButton")
        await drive.until("the sub-agent's window") { run.model.place == .chat(.subAgent("usage-drop")) }
        await drive.settle(300)
        let said = drive.tree()
        XCTAssertTrue(said.contains { $0.isHeading && $0.label.hasPrefix("Usage drop: investigate, Coder, The cloud machine, ")
            && $0.label.hasSuffix(AssistantWords.cannotReadMain.said) }, "its head says who it is, where it runs and that it cannot read the main chat")
        XCTAssertTrue(said.contains { $0.role == "AXTextArea" && $0.label == AssistantWords.messageSubAgent.said })
        XCTAssertTrue(drive.shows(AssistantWords.jobFromMain.said))
        XCTAssertTrue(drive.shows("is it only Safari?"))
        XCTAssertFalse(drive.has(AssistantWords.tasks.said, role: "AXHeading"), "a sub-agent's window is drawn without the panel")
        XCTAssertTrue(drive.node("Usage drop: investigate, Coder, working", role: "AXButton")?.isSelected ?? false)
        XCTAssertFalse(drive.node(AssistantWords.mainChat.said, role: "AXButton")?.isSelected ?? true)
        drive.picture("mac-driven-sub-agent")

        // The person writes to the sub-agent itself.
        let field = try run.field()
        drive.click(field)
        drive.type("and Firefox?")
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("the message to the sub-agent") { run.source.calls("send").count == 1 }
        XCTAssertEqual(run.source.calls("send"), ["send(\"and Firefox?\", files: [], subAgent(usage-drop))"])

        drive.press(AssistantWords.mainChat.said, role: "AXButton")
        await drive.until("the main chat again") { run.model.place == .chat(.main) }
        await drive.settle(300)
        XCTAssertTrue(drive.has(AssistantWords.tasks.said, role: "AXHeading"))
        XCTAssertTrue(drive.shows("remind me in 30 minutes to call Jonas"))
        XCTAssertTrue(drive.tree().contains { $0.role == "AXTextArea" && $0.label == AssistantWords.messageAssistant.said })
    }

    /// The first view of a window that takes typed words.
    private func writingPlace(in view: NSView?) -> NSView? {
        guard let view else { return nil }
        if view is NSTextView || view is NSTextField, view.acceptsFirstResponder { return view }
        for sub in view.subviews { if let found = writingPlace(in: sub) { return found } }
        return nil
    }

    func testTheIconAtTheTopRightAndThePlusAskForAJobAndStartASubAgent() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        let both = drive.nodes(AssistantWords.newSubAgent.said, role: "AXButton").sorted { $0.frame.minX < $1.frame.minX }
        XCTAssertEqual(both.count, 2, "the plus beside Sub-agents and the icon at the top right")
        let plus = both[0], icon = both[1]
        XCTAssertLessThan(plus.frame.maxX - run.window.frame.minX, Measure.sidebar)
        XCTAssertGreaterThan(icon.frame.minX - run.window.frame.minX, Measure.sidebar)

        // The icon: the job is typed, Return starts it.
        drive.press(icon)
        await drive.until("the question for the job") { run.window.heldSheets.count == 1 }
        await drive.settle(300)
        var sheet = try XCTUnwrap(run.window.heldSheets.first)
        XCTAssertTrue(drive.has(AssistantWords.newSubAgent.said, role: "AXHeading", in: sheet.contentView), drive.said(of: sheet.contentView))
        XCTAssertFalse(drive.node(AssistantWords.start.said, role: "AXButton", in: sheet.contentView)?.isEnabled ?? true,
                       "nothing starts without a job")
        var place = try XCTUnwrap(writingPlace(in: sheet.contentView), "the sheet has nowhere to write the job")
        sheet.makeFirstResponder(place)
        drive.type("Find the lease", in: sheet)
        await drive.settle(200)
        XCTAssertTrue(drive.node(AssistantWords.start.said, role: "AXButton", in: sheet.contentView)?.isEnabled ?? false,
                      "typed: \(drive.said(of: sheet.contentView))")
        drive.picture("mac-driven-job-sheet", of: sheet.contentView)
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1, in: sheet)
        await drive.until("the sub-agent to be asked for") { run.source.calls("startSubAgent").count == 1 }
        XCTAssertEqual(run.source.calls("startSubAgent"), ["startSubAgent(\"Find the lease\")"])
        await drive.until("the sheet to be put away") { run.window.heldSheets.isEmpty }
        await drive.until("the new sub-agent in the sidebar") { drive.has("Find the lease, working", role: "AXButton") }
        XCTAssertEqual(run.model.subAgents.first?.title, "Find the lease")
        XCTAssertEqual(run.model.subAgents.first?.startedBy, .person)

        // The plus asks the same. Escape puts the question away and starts nothing.
        drive.press(drive.nodes(AssistantWords.newSubAgent.said, role: "AXButton").min { $0.frame.minX < $1.frame.minX })
        await drive.until("the question for the job, from the plus") { run.window.heldSheets.count == 1 }
        await drive.settle(300)
        sheet = try XCTUnwrap(run.window.heldSheets.first)
        place = try XCTUnwrap(writingPlace(in: sheet.contentView))
        sheet.makeFirstResponder(place)
        drive.type("never mind", in: sheet)
        drive.key(Drive.Key.escape.0, code: Drive.Key.escape.1, in: sheet)
        await drive.until("the sheet to be put away by Escape") { run.window.heldSheets.isEmpty }
        await drive.settle(200)
        XCTAssertEqual(run.source.calls("startSubAgent").count, 1, "Escape starts nothing")
        XCTAssertFalse(run.model.asksForJob)
    }

    // MARK: The conversation

    func testTheAnswerButtonsUnderAMessageCallTheSource() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        for answer in ["Send it", "Show me the draft", "Not now"] { XCTAssertTrue(drive.has(answer, role: "AXButton"), answer) }
        drive.press("Show me the draft", role: "AXButton")
        await drive.until("the answer at the source") { run.source.calls("answer").count == 1 }
        XCTAssertEqual(run.source.calls("answer"), ["answer(maya-reply, show)"])
        await drive.until("the question to be settled") { drive.shows(AssistantWords.youChose.said(["answer": "Show me the draft"])) }
        XCTAssertFalse(drive.has("Send it", role: "AXButton"), "an answered question offers no answers")
        guard case .question(let asked)? = run.source.inner.items().first(where: { $0.id == "maya" })?.content else { return XCTFail() }
        XCTAssertEqual(asked.answer, "show")
        // See all tasks leads to the panel's first part.
        drive.press(AssistantWords.resolved.said, role: "AXButton")
        await drive.until("another part of the panel") { run.model.panelTab == .resolved }
        drive.press(AssistantWords.seeAllTasks.said, role: "AXButton")
        await drive.until("the panel's first part") { run.model.panelTab == .active }
    }

    func testATasksRepliesOpenUnderItsLineAndCloseAgain() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        XCTAssertFalse(drive.shows("The October figures are in the metrics sheet"))
        drive.press(AssistantWords.counted(2, one: .showReplyOne, many: .showReplyMany), role: "AXButton")
        await drive.until("the replies at the source") { run.source.calls("replies").count == 1 }
        XCTAssertEqual(run.source.calls("replies"), ["replies(maya, main)"])
        await drive.until("the replies under the line") { drive.shows("The October figures are in the metrics sheet") }
        XCTAssertTrue(drive.shows("The reply is drafted in your words"))
        XCTAssertEqual(run.model.panelTab, .active)
        XCTAssertTrue(drive.has(AssistantWords.tasks.said, role: "AXHeading"))
        // They stand between the line and the task's own piece.
        let said = drive.tree()
        let line = try XCTUnwrap(said.firstIndex { $0.label == AssistantWords.hideReplies.said })
        let reply = try XCTUnwrap(said.firstIndex { $0.value.contains("The October figures") })
        let piece = try XCTUnwrap(said.firstIndex { $0.name.contains("Maya, who invested in your seed") })
        XCTAssertLessThan(line, reply)
        XCTAssertLessThan(reply, piece)
        drive.picture("mac-driven-replies-open")

        drive.press(AssistantWords.hideReplies.said, role: "AXButton")
        await drive.until("the replies to be put away") { !drive.shows("The October figures are in the metrics sheet") }
        XCTAssertEqual(run.source.calls("replies").count, 1, "putting them away asks nothing")
        XCTAssertTrue(drive.has(AssistantWords.counted(2, one: .showReplyOne, many: .showReplyMany), role: "AXButton"))
    }

    // MARK: The Tasks panel

    func testThePanelsFourPartsSwitchAndASwitchPausesAnEvent() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        XCTAssertTrue(drive.node(AssistantWords.active.said, role: "AXButton")?.isSelected ?? false)
        XCTAssertTrue(drive.shows("Board deck for Friday: first draft"))

        drive.press(AssistantWords.upNext.said, role: "AXButton")
        await drive.until("Up next") { drive.shows(AssistantWords.nothingUpNext.said) }
        XCTAssertFalse(drive.shows("Board deck for Friday: first draft"))
        XCTAssertTrue(drive.node(AssistantWords.upNext.said, role: "AXButton")?.isSelected ?? false)

        drive.press(AssistantWords.resolved.said, role: "AXButton")
        await drive.until("Resolved") { drive.shows(AssistantWords.nothingResolved.said) }

        drive.press(AssistantWords.scheduled.said, role: "AXButton")
        await drive.until("Scheduled") { drive.shows("Every ten new mails") }
        XCTAssertTrue(drive.tree().contains { $0.isHeading && $0.label.hasPrefix(AssistantWords.events.said) })
        drive.picture("mac-driven-scheduled")
        // The switch of an event: on, and off is paused.
        func theSwitch() -> Node? { drive.tree().first { $0.label.hasPrefix("Every ten new mails") && !$0.isButton && $0.role != "AXStaticText" } }
        let on = try XCTUnwrap(theSwitch(), drive.said())
        XCTAssertEqual(on.value, "1", "\(on)")
        drive.press(on)
        await drive.until("the pause at the source") { run.source.calls("setOn").count == 1 }
        XCTAssertEqual(run.source.calls("setOn"), ["setOn(false, ten-mails)"])
        await drive.until("the switch to stand off") { theSwitch()?.value == "0" }
        XCTAssertEqual(run.model.automations.first { $0.id == "ten-mails" }?.isOn, false)
        XCTAssertEqual(run.model.automations.filter { !$0.isOn }.count, 1, "the others go on")
        drive.press(theSwitch())
        await drive.until("the event to go on again") { run.source.calls("setOn").last == "setOn(true, ten-mails)" }

        // Opening one begins its name in the main chat's field: it is changed by telling the assistant.
        drive.press(drive.tree().first { $0.isButton && $0.label.hasPrefix("Morning rundown") })
        await drive.until("its name in the field") { run.model.drafts[.main]?.text == "Morning rundown: " }
        await drive.until("the field to hold the keyboard") { run.window.firstResponder === (try? run.field()) }

        drive.press(AssistantWords.active.said, role: "AXButton")
        await drive.until("Active") { drive.shows("Board deck for Friday: first draft") }
        // The panel's own x puts it away.
        drive.press(AssistantWords.close.said, role: "AXButton")
        await drive.until("the panel to be put away") { !drive.has(AssistantWords.tasks.said, role: "AXHeading") }
        XCTAssertTrue(drive.has(AssistantWords.showTasks.said, role: "AXButton"))
        drive.press(AssistantWords.showTasks.said, role: "AXButton")
        await drive.until("the panel again") { drive.has(AssistantWords.tasks.said, role: "AXHeading") }
    }

    // MARK: The assistant view

    func testTheAssistantViewShowsItsStatusAndLeadsBackToTheChat() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        drive.press(AssistantWords.assistantView.said, role: "AXButton")
        await drive.until("the assistant view") { run.model.place == .assistantView }
        await drive.until("its first line") { drive.shows(AssistantWords.assistantActive.said) }
        for words in ["Claude Opus 5.5", AssistantWords.messagesToday.said, Wording().tokens(1_800_000), AssistantWords.connections.said,
                      AssistantWords.hasLearned.said, AssistantWords.whereItWorks.said, AssistantWords.workspaces.said] {
            XCTAssertTrue(drive.shows(words), "the assistant view says nothing of “\(words)”")
        }
        XCTAssertFalse(drive.has(AssistantWords.tasks.said, role: "AXHeading"))
        drive.picture("mac-driven-assistant-view")

        // A line's Open shows all of it; Escape puts it away.
        let rows = drive.tree()
        XCTAssertEqual(rows.filter { $0.isButton && $0.name == AssistantWords.open.said }.count, 5)
        let logins = try XCTUnwrap(rows.firstIndex { $0.name == AssistantWords.logins.said })
        let open = try XCTUnwrap(rows[logins...].first(where: \.isButton))
        XCTAssertEqual(open.name, AssistantWords.open.said)
        drive.press(open)
        // AppKit can request the same sheet twice from the offscreen window.
        await drive.until("the logins") {
            Set(run.window.heldSheets.map(ObjectIdentifier.init)).count == 1
        }
        await drive.settle(300)
        let sheet = try XCTUnwrap(run.window.heldSheets.first)
        XCTAssertTrue(drive.has(AssistantWords.logins.said, role: "AXHeading", in: sheet.contentView), drive.said(of: sheet.contentView))
        XCTAssertTrue(drive.shows("LinkedIn", in: sheet.contentView))
        drive.key(Drive.Key.escape.0, code: Drive.Key.escape.1, in: sheet)
        await drive.until("the logins to be put away") { run.window.heldSheets.isEmpty }

        // Add opens the chat with the words begun, where the assistant does the setup.
        drive.press(AssistantWords.add.said, role: "AXButton")
        await drive.until("the main chat") { run.model.place == .chat(.main) }
        await drive.until("the words begun in the field") { (try? run.field().string) == AssistantWords.addConnectionDraft.said }
    }

    // MARK: The menus and their keys

    /// A menu's line as a person reads it: its words and its key.
    private func said(_ item: NSMenuItem) -> String {
        guard !item.isSeparatorItem else { return "-" }
        guard !item.keyEquivalent.isEmpty else { return item.title }
        let mask = item.keyEquivalentModifierMask
        let signs = (mask.contains(.control) ? "⌃" : "") + (mask.contains(.option) ? "⌥" : "") + (mask.contains(.shift) ? "⇧" : "")
            + (mask.contains(.command) ? "⌘" : "")
        return "\(item.title) \(signs)\(item.keyEquivalent.uppercased())"
    }

    func testEveryLineOfTheMenusIsThereWithItsKey() async throws {
        let run = try await Started.app()
        defer { run.end() }
        typealias W = AssistantWords
        let menus = run.app.mainMenu.items.map { item -> (String, [String]) in
            item.submenu?.delegate?.menuNeedsUpdate?(item.submenu!)
            return (item.title, item.submenu?.items.map(said) ?? [])
        }
        XCTAssertEqual(menus.map(\.0), [W.appName, W.fileMenu.said, W.editMenu.said, W.viewMenu.said, W.windowMenu.said])
        XCTAssertEqual(menus[0].1, [W.about.said, "-", "\(W.assistantView.said)… ⌘,", W.signOut.said, "-", "\(W.hideApp.said) ⌘H", "\(W.hideOthers.said) ⌥⌘H",
                                    W.showAll.said, "-", "\(W.quit.said) ⌘Q"])
        XCTAssertEqual(menus[1].1, ["\(W.newSubAgent.said) ⌘N", "-", "\(W.closeWindow.said) ⌘W"])
        XCTAssertEqual(menus[2].1, ["\(W.undo.said) ⌘Z", "\(W.redo.said) ⇧⌘Z", "-", "\(W.cut.said) ⌘X", "\(W.copy.said) ⌘C",
                                    "\(W.paste.said) ⌘V", "\(W.selectAll.said) ⌘A"])
        XCTAssertEqual(menus[3].1, ["\(W.hideSidebar.said) ⌃⌘S", "\(W.hideTasks.said) ⌥⌘T", "-", "\(W.mainChat.said) ⌘1",
                                    "Usage drop: investigate ⌘2", "Find the Q3 board deck ⌘3", "YC batch against the thesis ⌘4",
                                    "Data room: open requests ⌘5"])
        XCTAssertEqual(menus[4].1, ["\(W.minimize.said) ⌘M", W.zoom.said, "-", "\(W.appName) ⌘0"])

        // The lines the system itself carries out are sent where the system takes them. They are
        // not pressed here: they would hide, close or end the very program that checks.
        func line(_ title: String) throws -> NSMenuItem {
            for menu in run.app.mainMenu.items { if let found = menu.submenu?.items.first(where: { $0.title == title }) { return found } }
            throw XCTSkip("no line called \(title)")
        }
        let systems: [(String, Selector, Bool)] = [
            (W.about.said, #selector(NSApplication.orderFrontStandardAboutPanel(_:)), true), (W.hideApp.said, #selector(NSApplication.hide(_:)), true),
            (W.hideOthers.said, #selector(NSApplication.hideOtherApplications(_:)), true),
            (W.showAll.said, #selector(NSApplication.unhideAllApplications(_:)), true), (W.quit.said, #selector(NSApplication.terminate(_:)), true),
            (W.closeWindow.said, #selector(NSWindow.performClose(_:)), false), (W.minimize.said, #selector(NSWindow.performMiniaturize(_:)), false),
            (W.zoom.said, #selector(NSWindow.performZoom(_:)), false), (W.cut.said, #selector(NSText.cut(_:)), false),
            (W.copy.said, #selector(NSText.copy(_:)), false), (W.paste.said, #selector(NSText.paste(_:)), false),
        ]
        for (title, action, toApp) in systems {
            let item = try line(title)
            XCTAssertEqual(item.action, action, title)
            XCTAssertTrue(toApp ? item.target === NSApp : item.target == nil, "\(title) goes to \(String(describing: item.target))")
        }
        // The window answers the lines that go to whatever has the keyboard or to the window itself.
        let field = try run.field()
        for action in [#selector(NSText.cut(_:)), #selector(NSText.copy(_:)), #selector(NSText.paste(_:))] {
            XCTAssertTrue(field.responds(to: action))
        }
        for action in [#selector(NSWindow.performClose(_:)), #selector(NSWindow.performMiniaturize(_:)), #selector(NSWindow.performZoom(_:))] {
            XCTAssertTrue(run.window.responds(to: action))
        }
        XCTAssertTrue(run.window.styleMask.isSuperset(of: [.closable, .miniaturizable, .resizable]))
    }

    func testTheMenusKeysDoWhatTheirLinesSay() async throws {
        let run = try await Started.app()
        defer { run.end() }
        typealias W = AssistantWords
        let drive = run.drive

        // Command N asks for a sub-agent's job.
        XCTAssertTrue(drive.key("n", [.command]).contains(W.newSubAgent.said))
        await drive.until("the question for the job") { run.window.heldSheets.count == 1 }
        await drive.settle(300)
        drive.key(Drive.Key.escape.0, code: Drive.Key.escape.1, in: try XCTUnwrap(run.window.heldSheets.first))
        await drive.until("the question to be put away") { run.window.heldSheets.isEmpty }

        // Command comma opens the assistant view, Command 1 the main chat, 2 to 5 the sub-agents.
        XCTAssertTrue(drive.key(",", [.command]).contains(W.assistantView.said))
        await drive.until("the assistant view") { run.model.place == .assistantView }
        // Beside the assistant view there is no Tasks panel to show or hide.
        XCTAssertTrue(drive.key("t", [.command, .option]).contains("off"))
        drive.key("1", [.command])
        await drive.until("the main chat") { run.model.place == .chat(.main) }
        for (key, agent) in zip(["2", "3", "4", "5"], ["usage-drop", "board-deck", "yc-batch", "data-room"]) {
            drive.key(key, [.command])
            await drive.until("sub-agent \(agent)") { run.model.place == .chat(.subAgent(SubAgent.ID(agent))) }
        }
        // There is no fifth sub-agent: Command 6 is nobody's.
        XCTAssertEqual(drive.key("6", [.command]), "the keyboard's holder")
        await drive.settle()
        XCTAssertEqual(run.model.place, .chat(.subAgent("data-room")))
        drive.key("1", [.command])
        await drive.until("the main chat again") { run.model.place == .chat(.main) }

        // Control Command S puts the sidebar away and brings it back; the line says which.
        XCTAssertTrue(drive.key("s", [.control, .command]).contains(W.hideSidebar.said))
        await drive.until("the sidebar to be away") { !drive.has(W.mainChat.said, role: "AXButton") }
        XCTAssertTrue(drive.has(W.showSidebar.said, role: "AXButton"), "the bar offers the sidebar back")
        XCTAssertTrue(drive.key("s", [.control, .command]).contains(W.showSidebar.said))
        await drive.until("the sidebar again") { drive.has(W.mainChat.said, role: "AXButton") }

        // Option Command T does the same for the Tasks panel.
        XCTAssertTrue(drive.key("t", [.option, .command]).contains(W.hideTasks.said))
        await drive.until("the panel to be away") { !drive.has(W.tasks.said, role: "AXHeading") }
        XCTAssertTrue(drive.key("t", [.option, .command]).contains(W.showTasks.said))
        await drive.until("the panel again") { drive.has(W.tasks.said, role: "AXHeading") }

        // Select all, Undo and Redo go to the message field.
        let field = try run.field()
        drive.click(field)
        drive.type("first")
        await drive.settle(250)
        XCTAssertTrue(drive.key("a", [.command]).contains(W.selectAll.said))
        XCTAssertEqual(field.selectedRange(), NSRange(location: 0, length: 5))
        drive.type("x")
        await drive.settle(250)
        XCTAssertEqual(field.string, "x")
        XCTAssertTrue(drive.key("z", [.command]).contains(W.undo.said))
        await drive.settle(250)
        XCTAssertEqual(field.string, "first", "Undo takes back what was typed last")
        XCTAssertEqual(run.model.drafts[.main]?.text, "first")
        XCTAssertTrue(drive.key("z", [.command, .shift]).contains(W.redo.said))
        await drive.settle(250)
        XCTAssertEqual(field.string, "x")
        XCTAssertEqual(run.source.calls.filter { !$0.hasPrefix("models") }, [], "none of this told the source anything")
    }

    func testBeforeADeviceIsPairedTheWindowsOwnLinesAreOff() async throws {
        let run = try await Started.app(MemorySource.Content())
        defer { run.end() }
        let drive = run.drive
        XCTAssertTrue(drive.has(AssistantWords.wayInTitle.said, role: "AXHeading"), drive.said())
        XCTAssertTrue(drive.has(AssistantWords.pasteLink.said, role: "AXButton"))
        XCTAssertFalse(drive.has(AssistantWords.mainChat.said), "the app starts empty, at the way in")
        XCTAssertTrue(drive.key("n", [.command]).contains("off"))
        XCTAssertTrue(drive.key(",", [.command]).contains("off"))
        XCTAssertTrue(drive.key("s", [.control, .command]).contains("off"))
        await drive.settle()
        XCTAssertFalse(run.model.asksForJob)
        XCTAssertEqual(run.model.place, .chat(.main))
        XCTAssertEqual(run.window.heldSheets.count, 0)
        drive.picture("mac-driven-way-in")
    }

    // MARK: The item in the menu bar

    func testTheMenuBarItemOpensTheWindowAndWaitingTasks() async throws {
        let run = try await Started.app()
        defer { run.end() }
        typealias W = AssistantWords
        let menu = run.app.statusMenu
        func lines() -> [String] {
            menu.delegate?.menuNeedsUpdate?(menu)
            return menu.items.map(said)
        }
        XCTAssertEqual(lines(), [W.openApp.said, "-", W.counted(3, one: .waitsOne, many: .waitsMany), "Find the Q3 board deck",
                                 "Reply to Maya at Northwind", "Board deck for Friday: first draft", "-",
                                 W.signOut.said, "-", "\(W.quit.said) ⌘Q"])
        XCTAssertNil(menu.items[2].action, "how many things wait is said, not pressed")
        XCTAssertEqual(menu.items.last?.action, #selector(NSApplication.terminate(_:)))
        XCTAssertTrue(menu.items.last?.target === NSApp)

        // Its first line asks the window to the front. Here the window only writes that down.
        XCTAssertEqual(run.window.asked.filter { $0 == "makeKeyAndOrderFront" }, [])
        menu.performActionForItem(at: 0)
        XCTAssertEqual(run.window.asked.filter { $0 == "makeKeyAndOrderFront" }.count, 1)
        XCTAssertFalse(NSApp.isActive, "a check never comes to the front")

        // Something that waits and stands in the chat opens the window at it.
        run.model.place = .assistantView
        menu.performActionForItem(at: 4)
        XCTAssertEqual(run.model.place, .chat(.main))
        XCTAssertEqual(run.window.asked.filter { $0 == "makeKeyAndOrderFront" }.count, 2)
        // Something that waits and stands in no conversation opens the Tasks panel.
        run.model.showsPanel = false
        run.model.panelTab = .resolved
        menu.performActionForItem(at: 3)
        XCTAssertTrue(run.model.showsPanel)
        XCTAssertEqual(run.model.panelTab, .active)

        // Answered, a thing no longer waits.
        run.model.answer("maya-reply", with: "send")
        await run.drive.until("two things to wait") { run.model.waiting == 3 }
        XCTAssertEqual(lines()[2], W.counted(3, one: .waitsOne, many: .waitsMany), "the task stays ready for review until the assistant says otherwise")
    }

    func testBeforePairingTheMenuBarOffersTheWindowAndQuit() async throws {
        let run = try await Started.app(MemorySource.Content())
        defer { run.end() }
        typealias W = AssistantWords
        let menu = run.app.statusMenu
        menu.delegate?.menuNeedsUpdate?(menu)
        XCTAssertEqual(menu.items.map(said), [W.openApp.said, "-", W.waitsNone.said, "-", "\(W.quit.said) ⌘Q"])
    }

    // MARK: The window

    func testTheWindowIsTheDrawingsAndItsThreeButtonsStandClearOfWhatItShows() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive, window = run.window
        XCTAssertTrue(window.styleMask.contains(.fullSizeContentView))
        XCTAssertEqual(window.titleVisibility, .hidden)
        XCTAssertTrue(window.titlebarAppearsTransparent)
        XCTAssertEqual(window.title, AssistantWords.appName)
        // It is never dragged smaller than its smallest, and never made larger than it is asked to be.
        XCTAssertEqual(window.delegate?.windowWillResize?(window, to: NSSize(width: 300, height: 200)), MacApp.smallest)
        XCTAssertEqual(window.delegate?.windowWillResize?(window, to: NSSize(width: 1400, height: 300)), NSSize(width: 1400, height: 560))
        XCTAssertEqual(window.delegate?.windowWillResize?(window, to: NSSize(width: 1400, height: 900)), NSSize(width: 1400, height: 900))
        print("NOTE the window's own smallest content size reads \(window.contentMinSize), its smallest frame \(window.minSize)")
        XCTAssertFalse(window.isRestorable, "the system writes nothing of the window down for its next start")
        XCTAssertEqual(window.contentView?.frame.size, CGSize(width: 1280, height: 820))
        // The window's three buttons, in the window's own measure from its foot.
        let buttons = try [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton].map { kind -> CGRect in
            let button = try XCTUnwrap(window.standardWindowButton(kind))
            return button.convert(button.bounds, to: nil)
        }
        let lights = buttons.reduce(buttons[0]) { $0.union($1) }
        func inWindow(_ node: Node?) throws -> CGRect {
            try XCTUnwrap(node).frame.offsetBy(dx: -window.frame.minX, dy: -window.frame.minY)
        }
        // Over the sidebar, above its first line.
        let head = try inWindow(drive.node(AssistantWords.assistantView.said, role: "AXButton"))
        XCTAssertLessThan(lights.maxX, Measure.sidebar)
        XCTAssertGreaterThanOrEqual(lights.minY, head.maxY, "the three buttons \(lights) stand above the sidebar's head \(head)")
        // The whole frame of the window, with its buttons, as it would stand on a screen.
        drive.picture("mac-driven-window")

        // With the sidebar away the bar's first control stands right of them.
        drive.key("s", [.control, .command])
        await drive.until("the sidebar to be away") { drive.has(AssistantWords.showSidebar.said, role: "AXButton") }
        await drive.settle(200)
        let show = try inWindow(drive.node(AssistantWords.showSidebar.said, role: "AXButton"))
        XCTAssertGreaterThan(show.minX, lights.maxX, "Show sidebar \(show) stands clear of the three buttons \(lights)")
        drive.picture("mac-driven-window-no-sidebar")

        // At its smallest size nothing is lost: the field, the bar and the panel are all there.
        window.setContentSize(MacApp.smallest)
        await drive.settle(400)
        XCTAssertTrue(drive.has(AssistantWords.tasks.said, role: "AXHeading"))
        let field = try run.field()
        let place = field.convert(field.bounds, to: nil)
        XCTAssertGreaterThan(place.width, 150, "the field keeps room to write in at the smallest window")
        XCTAssertTrue(CGRect(origin: .zero, size: window.contentView?.frame.size ?? .zero).contains(place))
        drive.picture("mac-driven-window-smallest")
    }
}
#endif
