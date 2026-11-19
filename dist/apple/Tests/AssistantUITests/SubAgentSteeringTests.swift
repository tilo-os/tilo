#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Verifies the rich sub-agent header bar, mid-flight steering bar, active composer delivery
/// without stopping, and Code Factory active sub-agent strip planning for iPhone and Mac.
@MainActor
final class SubAgentSteeringTests: XCTestCase {
    func testSubAgentLocationClassifiesServerMacFolderAndCodingSandbox() {
        XCTAssertEqual(SubAgentLocation.classify(""), .server)
        XCTAssertEqual(SubAgentLocation.classify("your Tilo server"), .server)
        XCTAssertEqual(SubAgentLocation.classify("The cloud machine"), .server)
        XCTAssertEqual(SubAgentLocation.server.label, "Server")

        XCTAssertEqual(SubAgentLocation.classify("Mac folder: ~/dev/tilo"), .macFolder)
        XCTAssertEqual(SubAgentLocation.classify("/Users/xaver/dev/tilo"), .macFolder)
        XCTAssertEqual(SubAgentLocation.classify("~/projects/app"), .macFolder)
        XCTAssertEqual(SubAgentLocation.classify("Local folder"), .macFolder)
        XCTAssertEqual(SubAgentLocation.macFolder.label, "Mac folder")

        XCTAssertEqual(SubAgentLocation.classify("Claude Code on the cloud machine"), .codingSandbox)
        XCTAssertEqual(SubAgentLocation.classify("Claude Code on the workspace's machine"), .codingSandbox)
        XCTAssertEqual(SubAgentLocation.classify("the workspace's machine"), .codingSandbox)
        XCTAssertEqual(SubAgentLocation.classify("Codex sandbox"), .codingSandbox)
        XCTAssertEqual(SubAgentLocation.codingSandbox.label, "Coding sandbox")
    }

    func testSubAgentStatusBadgeCoversAllStatesAndStopEligibility() {
        let cases: [(SubAgent.State, String, Bool, Bool, Bool)] = [
            (.working, "Working", true, true, false),
            (.needsYou, "Needs you", true, true, true),
            (.done, "Done", false, false, false),
            (.failed, "Failed", false, false, false),
            (.stopped, "Stopped", false, false, false),
        ]
        for (state, label, isActive, canStop, needsAttention) in cases {
            let badge = SubAgentStatusBadge(state: state)
            XCTAssertEqual(badge.label, label)
            XCTAssertEqual(badge.isActive, isActive)
            XCTAssertEqual(badge.canStop, canStop)
            XCTAssertEqual(badge.needsAttention, needsAttention)
        }
    }

    func testPhoneNowStripPlanHandlesEmptySingleAndParallelActiveSubAgents() async {
        // Empty when all sub-agents are finished/failed/stopped.
        var idleContent = Drawing.content()
        idleContent.subAgents = [
            SubAgent(id: "done-1", title: "Release notes", role: "Writer", state: .done, runsOn: "your Tilo server", started: Drawing.now),
            SubAgent(id: "fail-1", title: "Nightly check", role: "Coder", state: .failed, runsOn: "Claude Code on the workspace's machine", started: Drawing.now),
            SubAgent(id: "stop-1", title: "Old draft", role: "Researcher", state: .stopped, runsOn: "Mac folder", started: Drawing.now),
        ]
        let idleModel = AssistantModel(source: MemorySource(idleContent, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await idleModel.prime()
        let idlePlan = idleModel.subAgentStripPlan(in: .main)
        XCTAssertTrue(idlePlan.isEmpty)
        XCTAssertNil(idlePlan.groupPill)

        // Single active sub-agent: no grouped pill, shows the single pill directly.
        var oneContent = Drawing.content()
        oneContent.subAgents = [
            SubAgent(id: "single", title: "Fix Safari sign-in", role: "Coder", state: .working, runsOn: "Coding sandbox", started: Drawing.now),
        ]
        let oneModel = AssistantModel(source: MemorySource(oneContent, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await oneModel.prime()
        let onePlan = oneModel.subAgentStripPlan(in: .main)
        XCTAssertNil(onePlan.groupPill)
        XCTAssertEqual(onePlan.entries.count, 1)
        XCTAssertEqual(onePlan.entries[0].agent.id, "single")
        XCTAssertEqual(onePlan.entries[0].badge.label, "Working")
        XCTAssertFalse(onePlan.entries[0].isSelected)

        // Multiple active sub-agents (parallel Code Factory): grouped pill prioritizes needsYou,
        // entries put needsYou first, and viewing one highlights it and targets the next on tap.
        let parallelModel = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
        await parallelModel.prime()
        let fromMain = parallelModel.subAgentStripPlan(in: .main)
        XCTAssertEqual(fromMain.entries.map(\.agent.id), ["board-deck", "usage-drop", "yc-batch"])
        XCTAssertEqual(fromMain.entries[0].badge.label, "Needs you")
        XCTAssertEqual(fromMain.groupPill?.count, 3)
        XCTAssertEqual(fromMain.groupPill?.needsYouCount, 1)
        XCTAssertEqual(fromMain.groupPill?.workingCount, 2)
        XCTAssertEqual(fromMain.groupPill?.target, "board-deck")
        XCTAssertEqual(fromMain.groupPill?.detailLabel, "1 needs you")

        // When viewing board-deck, it is marked selected and the group pill cycles to usage-drop.
        let fromBoardDeck = parallelModel.subAgentStripPlan(in: .subAgent("board-deck"))
        XCTAssertTrue(fromBoardDeck.entries[0].isSelected)
        XCTAssertFalse(fromBoardDeck.entries[1].isSelected)
        XCTAssertEqual(fromBoardDeck.groupPill?.target, "usage-drop")
    }

    func testMacSubAgentHeaderDisplaysMetadataStatusStopAndMidFlightSteeringWithoutCancelling() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive

        // Open the working sub-agent ("usage-drop": Coder, working, Coding sandbox).
        drive.press("Usage drop: investigate, Coder, working", role: "AXButton")
        await drive.until("the sub-agent opens") { started.model.place == .chat(.subAgent("usage-drop")) }
        await drive.settle(300)

        XCTAssertTrue(drive.shows("Working"), "live status badge is shown: \(drive.said())")
        XCTAssertTrue(drive.shows("Coder"), "role badge is shown: \(drive.said())")
        XCTAssertTrue(drive.shows("Coding sandbox"), "work location badge is shown: \(drive.said())")
        XCTAssertTrue(drive.shows("Steer this sub-agent while it works…"), "mid-flight steering guidance is shown: \(drive.said())")
        XCTAssertNotNil(drive.node(AssistantWords.stop.said, role: "AXButton"), "1-tap Stop button is visible while working")

        // Even while the sub-agent is actively answering, typing and sending delivers mid-flight
        // without calling stop or stopAnswering.
        started.source.inner.setAnswering(true, in: .subAgent("usage-drop"))
        await drive.until("answering active") { started.model.isWriting(.subAgent("usage-drop")) }
        let field = try started.field()
        drive.click(field)
        drive.type("Focus on Safari 18 cookies")
        await drive.settle(150)
        XCTAssertTrue(drive.shows("Sends mid-flight"), "drafting while working confirms mid-flight delivery")
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("steering note delivered") { started.source.calls("send").count == 1 }
        XCTAssertEqual(started.source.calls("send"), ["send(\"Focus on Safari 18 cookies\", files: [], subAgent(usage-drop))"])
        XCTAssertEqual(started.source.calls("stop"), [], "mid-flight steering must never stop the sub-agent")
        XCTAssertEqual(started.source.calls("stopAnswering"), [], "mid-flight steering must never cancel answering")

        // Pressing 1-tap Stop in the header calls source.stop("usage-drop").
        let stopButtons = drive.nodes(AssistantWords.stop.said, role: "AXButton")
        XCTAssertFalse(stopButtons.isEmpty, "Stop button present in header")
        drive.press(stopButtons.first)
        await drive.until("sub-agent stopped") { started.source.calls("stop") == ["stop(usage-drop)"] }

        // Viewing a done sub-agent shows Done + Server, hides Stop, and hides the steering bar.
        let doneAgent = try XCTUnwrap(started.model.subAgents.first { $0.state == .done })
        let doneButton = try XCTUnwrap(drive.tree().first { $0.isButton && $0.label.hasPrefix(doneAgent.title) })
        drive.press(doneButton)
        await drive.until("done sub-agent opens") { started.model.place == .chat(.subAgent(doneAgent.id)) }
        await drive.settle(300)
        XCTAssertTrue(drive.shows("Done"))
        XCTAssertTrue(drive.shows("Server"))
        XCTAssertFalse(drive.has(AssistantWords.stop.said, role: "AXButton"), "finished sub-agent shows no Stop button")
        XCTAssertFalse(drive.shows("Steer this sub-agent while it works…"), "finished sub-agent shows no mid-flight steering bar")
    }

    private struct PhoneHeaderHarness: View {
        let id: SubAgent.ID
        @Environment(AssistantModel.self) private var model

        var body: some View {
            VStack(spacing: 0) {
                if let live = model.subAgent(id) {
                    SubAgentHeaderBar(agent: live, showsBackToMain: true, showsStop: true, showsChrome: true)
                }
                SubAgentSteeringBar(chat: .subAgent(id))
            }
            .frame(width: 390)
            .background(Token.content)
        }
    }

    func testPhoneStyleSubAgentHeaderSupportsBackToMainAndOneTapStop() async throws {
        var content = Drawing.content()
        content.subAgents = [
            SubAgent(id: "mac-worker", title: "Refactor auth module", role: "Coder", state: .working,
                     runsOn: "Mac folder: ~/dev/tilo", started: Drawing.now),
        ]
        let source = MemorySource(content, now: { Drawing.now })
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        await model.prime()
        model.open(.subAgent("mac-worker"))

        let view = AssistantRoot(model: model) {
            PhoneHeaderHarness(id: "mac-worker")
        }
        let hosting = NSHostingView(rootView: view)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 390, height: 180), [.borderless])
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        let drive = Drive(window)
        await drive.settle(250)

        XCTAssertTrue(drive.shows("Refactor auth module"))
        XCTAssertTrue(drive.shows("Working"))
        XCTAssertTrue(drive.shows("Coder"))
        XCTAssertTrue(drive.shows("Mac folder"))
        XCTAssertTrue(drive.shows("Steer this sub-agent while it works…"))
        XCTAssertTrue(drive.has("Back to main", role: "AXButton"))
        XCTAssertTrue(drive.has(AssistantWords.stop.said, role: "AXButton"))

        // 1-tap Stop transitions the sub-agent to .stopped, updates the badge, and hides Stop + steering bar.
        drive.press(AssistantWords.stop.said, role: "AXButton")
        await drive.until("sub-agent stops") { model.subAgent("mac-worker")?.state == .stopped }
        await drive.settle(200)
        XCTAssertTrue(drive.shows("Stopped"))
        XCTAssertFalse(drive.has(AssistantWords.stop.said, role: "AXButton"))
        XCTAssertFalse(drive.shows("Steer this sub-agent while it works…"))

        // 1-tap Back to main jumps straight back to .main.
        drive.press("Back to main", role: "AXButton")
        await drive.until("returned to main") { model.place == .chat(.main) }
    }

    func testMacTopBarActiveSubAgentsPillPrioritizesNeedsYouAndCyclesParallelWorkers() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive

        // From main chat, pressing the active sub-agents pill opens board-deck (needsYou) first.
        let pillLabel = AssistantWords.subAgentsCount.said(["count": "3"])
        XCTAssertTrue(drive.has(pillLabel, role: "AXButton"), "active sub-agents pill is visible: \(drive.said())")
        drive.press(pillLabel, role: "AXButton")
        await drive.until("jumps to needsYou sub-agent first") { started.model.place == .chat(.subAgent("board-deck")) }
        await drive.settle(250)
        XCTAssertTrue(drive.shows("Needs you"))
        XCTAssertTrue(drive.shows("Needs your input — reply below to guide this sub-agent…"))

        // Pressing the pill again while on board-deck cycles straight to the next active sub-agent.
        drive.press(pillLabel, role: "AXButton")
        await drive.until("cycles to next parallel sub-agent") { started.model.place == .chat(.subAgent("usage-drop")) }
    }
}
#endif
