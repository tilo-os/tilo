#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

@MainActor
final class UpgradeTests: XCTestCase {
    // MARK: Persistent live-artifact preview thumbnails

    func testSnapshotCacheSavesBoundedThumbnailsAndWipesThemOnSignOut() async throws {
        let folder = URL(filePath: NSTemporaryDirectory(), directoryHint: .isDirectory)
            .appending(path: "tilo-thumb-check-\(UUID().uuidString)", directoryHint: .isDirectory)
        defer { try? FileManager.default.removeItem(at: folder) }
        let cache = SnapshotCache(file: folder.appending(path: "snapshot.json"))
        XCTAssertEqual(cache.thumbnails.lastPathComponent, "thumbnails")

        // Safe filenames reject path traversal and slashes while staying deterministic.
        let unsafeID = "../sheet:2026/Q3?draft"
        let safeName = SnapshotCache.safeThumbnailID(unsafeID)
        XCTAssertFalse(safeName.contains("/"))
        XCTAssertFalse(safeName.contains(".."))
        XCTAssertFalse(safeName.contains(":"))

        // Write more than the 24-thumbnail cap and verify oldest files are pruned.
        let samplePNG = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x01, 0x02])
        for index in 0..<28 {
            cache.saveThumbnail(samplePNG + Data([UInt8(index)]), for: "artifact-\(index)")
            try await Task.sleep(for: .milliseconds(15))
        }
        await cache.flushThumbnails()
        let stored = (try? FileManager.default.contentsOfDirectory(at: cache.thumbnails, includingPropertiesForKeys: nil)) ?? []
        XCTAssertEqual(stored.count, SnapshotCache.thumbnailLimit, "thumbnail cache stays bounded at 24 files")
        XCTAssertNotNil(cache.readThumbnail(for: "artifact-27"), "newest thumbnail is kept")
        XCTAssertNil(cache.readThumbnail(for: "artifact-0"), "oldest thumbnails are evicted")

        // Signing out wipes the thumbnail directory completely.
        let model = AssistantModel(source: Drawing.source(), snapshots: cache)
        await model.prime()
        model.signOut()
        await cache.flushThumbnails()
        XCTAssertNil(cache.readThumbnail(for: "artifact-27"), "signOut wipes cached thumbnails")
        XCTAssertFalse(FileManager.default.fileExists(atPath: cache.thumbnails.path))
    }

    // MARK: Direct deep-link from waiting-count control

    func testOpenFirstWaitingJumpsDirectlyToFirstWaitingDecisionAndFallsBackCleanly() async {
        // Prefers needsInput task item over readyForReview task item.
        var content = Drawing.content()
        content.tasks = [
            AssistantTask(id: "review-1", title: "Review draft", state: .readyForReview, item: "maya"),
            AssistantTask(id: "ask-1", title: "Pick version", state: .needsInput, item: "rundown-1"),
        ]
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await model.prime()
        model.showsPanel = true
        let jumped = model.openFirstWaiting()
        XCTAssertTrue(jumped)
        XCTAssertEqual(model.place, .chat(.main))
        XCTAssertEqual(model.shownItem, "rundown-1")
        XCTAssertFalse(model.showsPanel, "direct jump focuses the conversation instead of leaving Tasks covering it")

        // When no task item exists, jumps to a waiting sub-agent.
        var subOnly = Drawing.content()
        subOnly.main = []
        subOnly.tasks = [AssistantTask(id: "deck", title: "Find deck", state: .needsInput)]
        subOnly.subAgents = [SubAgent(id: "board-deck", title: "Find deck", role: "Researcher",
                                      state: .needsYou, runsOn: "Cloud", started: Drawing.now)]
        let subModel = AssistantModel(source: MemorySource(subOnly, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await subModel.prime()
        XCTAssertTrue(subModel.openFirstWaiting())
        XCTAssertEqual(subModel.place, .chat(.subAgent("board-deck")))

        // When nothing can be addressed directly, falls back to opening the Active Tasks tab.
        var fallback = Drawing.content()
        fallback.main = []
        fallback.subAgents = []
        fallback.tasks = [AssistantTask(id: "orphan", title: "Needs check", state: .readyForReview)]
        let fallbackModel = AssistantModel(source: MemorySource(fallback, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await fallbackModel.prime()
        fallbackModel.showsPanel = false
        XCTAssertFalse(fallbackModel.openFirstWaiting())
        XCTAssertTrue(fallbackModel.showsPanel)
        XCTAssertEqual(fallbackModel.panelTab, .active)
    }

    func testSinceLeftCardWaitingCountButtonJumpsDirectlyToWaitingDecision() async throws {
        var content = Drawing.content()
        content.sinceLeft = Drawing.sinceLeft
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await model.prime()
        let view = AssistantRoot(model: model) {
            SinceLeftCard(since: Drawing.sinceLeft)
                .frame(width: 420)
                .padding(16)
                .background(Token.content)
        }
        let hosting = NSHostingView(rootView: view)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 460, height: 360), [.borderless])
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        let drive = Drive(window)
        await drive.settle(250)
        drive.press(AssistantWords.waitingCount.said(["count": "1"]), role: "AXButton")
        await drive.until("waiting-count button jumps straight to maya") { model.shownItem == "maya" }
    }

    // MARK: Automation removal vs single-action Undo clarity

    func testAutomationRemovalIsClearlyDistinguishedFromSingleActionUndo() async throws {
        let removeChoice = Choice(id: "undo", title: AssistantWords.undo.said)
        let lastActionChoice = Choice(id: "undo-last", title: "Undo")
        XCTAssertEqual(CardActionCopy.eventChoiceTitle(for: removeChoice), "Remove automation")
        XCTAssertEqual(CardActionCopy.eventChoiceTitle(for: lastActionChoice), "Undo last action")
        XCTAssertEqual(CardActionCopy.eventSettledText(for: .undone),
                       "Automation removed. It will not act again; past actions stay as they were.")

        let questionChoices = CardActionCopy.questionChoices([
            Choice(id: "keep", title: "Keep it", isMain: true),
            Choice(id: "undo", title: AssistantWords.undo.said),
        ])
        XCTAssertEqual(questionChoices.map(\.title), ["Keep it", "Undo this action"])

        // Render an event card inside ConversationView and verify the visible button says "Remove automation".
        var content = Drawing.content()
        content.main = Drawing.eventPiece
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await model.prime()
        let view = AssistantRoot(model: model) {
            ConversationView(chat: .main)
                .frame(width: 460, height: 420)
                .background(Token.content)
        }
        let hosting = NSHostingView(rootView: view)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 460, height: 420), [.borderless])
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        let drive = Drive(window)
        await drive.settle(300)
        XCTAssertTrue(drive.has("Remove automation", role: "AXButton"),
                      "automation card offers explicit Remove automation button: \(drive.said())")
        XCTAssertFalse(drive.has("Undo", role: "AXButton"),
                       "ambiguous bare Undo button is not shown for automation removal")
    }
}
#endif
