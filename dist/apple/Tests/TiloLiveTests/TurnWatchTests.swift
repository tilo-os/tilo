#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import TiloLive
import XCTest

/// What a device reads of the turn that writes a reply, from what Chat keeps with the reply:
/// what it does now, the steps it took, and how long each took by the device's own clock.
final class TurnWatchTests: XCTestCase {
    private let began = Date(timeIntervalSince1970: 1_794_900_000)
    private let asked = #"{"$variant":"asked"}"#
    private let applied = #"{"$variant":"applied"}"#
    private let refused = #"{"$variant":"refused","why":"not_found"}"#
    private let taken = #"{"$variant":"taken"}"#

    /// A reply as Chat keeps it: its words, what Chat keeps of its work, and whether that work goes on.
    private struct Reply {
        var words: String
        var work: JSON
        var going: Bool
    }

    private func quoted(_ words: String) -> String {
        "\"" + words.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
            .replacingOccurrences(of: "\n", with: "\\n") + "\""
    }

    private func row(_ id: String, _ sequence: Int, _ label: String, about resource: String, _ outcome: String) -> String {
        "{\"id\":\(quoted(id)),\"sequence\":\(sequence),\"resource\":\(quoted(resource)),\"label\":\(quoted(label)),\"outcome\":\(outcome)}"
    }

    private func doing(_ did: String, about: String = "") -> String {
        "{\"$variant\":\"doing\",\"did\":\(quoted(did)),\"about\":\(quoted(about))}"
    }

    /// The assistant's reply: its words, the state of its work, a row for each step, its thinking.
    private func reply(_ words: String = "", state: String? = nil, since: Int = 1_794_900_000, trace: String = "[]",
                       thought: String = "", going: Bool = true,
                       file: StaticString = #filePath, line: UInt = #line) throws -> Reply {
        let stream = "{\"sequence\":1,\"reasoning\":\(quoted(thought)),\"progress\":\"\",\"finish\":null,\"detail\":\"\"}"
        let work = "{\"stream\":\(stream),\"state\":{\"state\":\(state ?? taken),\"since\":\(since)},\"trace\":\(trace),\"failure_detail\":null}"
        return Reply(words: words, work: try XCTUnwrap(JSON(text: work), file: file, line: line), going: going)
    }

    private func read(_ watch: inout TurnWatch, _ reply: Reply, now: Date) -> Message.Turn? {
        watch.read(work: reply.work, words: reply.words, of: "m1", began: began, going: reply.going, now: now)
    }

    func testATurnAsChatKeepsItWithTheReplyIsReadStepByStepInOrderWithHowEachEnded() throws {
        var watch = TurnWatch()
        let work = try XCTUnwrap(JSON(text: try KeptTurn.work()))
        let read = watch.read(work: work, words: "", of: "m1", began: began, going: true, now: began)
        let turn = try XCTUnwrap(read, "a reply whose turn took three steps says nothing of its turn")
        XCTAssertEqual(turn.now, .step("Append paragraph: word-processor"))
        XCTAssertEqual(turn.steps.map(\.words),
                       ["List: word-processor", "Append paragraph: word-processor", "Append paragraph: word-processor"])
        XCTAssertEqual(turn.steps.map(\.outcome), [.done, .done, .failed])
        XCTAssertEqual(turn.steps.map(\.took), [nil, nil, nil], "a step Chat keeps no time of, and this device never saw going, was given a time")
    }

    func testAToolsBareNameIsNoWordsAndAStepsWordsAreSaidWithWhatItIsAbout() throws {
        var watch = TurnWatch()
        let bare = read(&watch, try reply(state: doing("tilo_read")), now: began)
        XCTAssertEqual(bare?.now, .step(nil), "a tool's bare name was said as words")
        XCTAssertEqual(bare?.steps, [])
        let words = read(&watch, try reply(state: doing("Read", about: "Mail settings"), since: 1_794_900_004), now: began + 4)
        XCTAssertEqual(words?.now, .step("Read: Mail settings"))
        let alone = read(&watch, try reply(state: doing("Add an account"), since: 1_794_900_009), now: began + 9)
        XCTAssertEqual(alone?.now, .step("Add an account"))
        XCTAssertEqual(alone?.began, began, "the turn's beginning moved")
    }

    func testEveryStepIsCountedInTheOrderItWasTakenWhateverOrderItsRowsComeIn() throws {
        var watch = TurnWatch()
        let rows = (1...30).reversed().map { row("s\($0)", $0, "Read", about: "Part \($0)", applied) }.joined(separator: ",")
        let read30 = read(&watch, try reply(state: doing("Read", about: "Part 30"), trace: "[\(rows)]"), now: began)
        let turn = try XCTUnwrap(read30)
        XCTAssertEqual(turn.steps.count, 30)
        XCTAssertEqual(turn.steps.first?.words, "Read: Part 1")
        XCTAssertEqual(turn.steps.last?.words, "Read: Part 30")
        XCTAssertEqual(turn.now, .step("Read: Part 30"))
        // Chat keys a turn's rows by their ids: what stands under each id reads the same.
        var other = TurnWatch()
        let keyed = (1...3).map { "\"s\($0)\":" + row("s\($0)", $0, "Read", about: "Part \($0)", applied) }.joined(separator: ",")
        let same = read(&other, try reply(state: doing("Read"), trace: "{\(keyed)}"), now: began)
        XCTAssertEqual(same?.steps.map(\.words), ["Read: Part 1", "Read: Part 2", "Read: Part 3"])
    }

    func testAStepsTimeIsTheDevicesOwnAndAStepItNeverSawGoingHasNone() throws {
        var watch = TurnWatch()
        let first = "[\(row("s1", 1, "Look up", about: "Mail servers", asked))]"
        let going = read(&watch, try reply(state: doing("Look up"), trace: first), now: began)
        XCTAssertEqual(going?.steps.map(\.outcome), [.going])
        XCTAssertEqual(going?.steps.map(\.took), [nil])
        let both = "[\(row("s1", 1, "Look up", about: "Mail servers", applied)),\(row("s2", 2, "Add an account", about: "Mail", refused))]"
        let read2 = read(&watch, try reply(state: doing("Add an account"), since: 1_794_900_003, trace: both), now: began + 3.5)
        let ended = try XCTUnwrap(read2)
        XCTAssertEqual(ended.steps.map(\.outcome), [.done, .failed])
        XCTAssertEqual(ended.steps[0].took ?? -1, 3.5, accuracy: 0.001)
        XCTAssertNil(ended.steps[1].took, "a step first seen ended was given a time")
        // A later look changes no time that was taken.
        let read3 = read(&watch, try reply(state: doing("Add an account"), since: 1_794_900_003, trace: both), now: began + 60)
        let later = try XCTUnwrap(read3)
        XCTAssertEqual(later.steps[0].took ?? -1, 3.5, accuracy: 0.001)
    }

    func testThinkingShowsItsNewestWordsTheReplysWordsTakeTheirPlaceAndAStepComesBack() throws {
        var watch = TurnWatch()
        let thought = "First the address.\nNow **looking** at the mail servers"
        let nothing = read(&watch, try reply(), now: began)?.now
        XCTAssertEqual(nothing, .thinking(""))
        let thinks = read(&watch, try reply(thought: thought), now: began + 1)?.now
        XCTAssertEqual(thinks, .thinking("Now looking at the mail servers"))
        let writes = read(&watch, try reply("Your mail", thought: thought), now: began + 2)?.now
        XCTAssertEqual(writes, .writing)
        let still = read(&watch, try reply("Your mail", thought: thought), now: began + 3)?.now
        XCTAssertEqual(still, .writing, "a look that found nothing new changed what the turn does")
        let step = try reply("Your mail", state: doing("Add an account", about: "Mail"), since: 1_794_900_004, thought: thought)
        let steps = read(&watch, step, now: began + 4)?.now
        XCTAssertEqual(steps, .step("Add an account: Mail"))
        let more = try reply("Your mail account is", state: doing("Add an account", about: "Mail"), since: 1_794_900_004, thought: thought)
        let again = read(&watch, more, now: began + 5)?.now
        XCTAssertEqual(again, .writing)
    }

    func testTheBeginningIsHeldByADeviceWhoseClockIsBehindAndAnEndedTurnIsForgotten() throws {
        var watch = TurnWatch()
        // The device's clock is twenty seconds behind its person's Tilo.
        let first = read(&watch, try reply(), now: began - 20)
        XCTAssertEqual(first?.began, began - 20)
        let second = read(&watch, try reply(thought: "more"), now: began - 15)
        XCTAssertEqual(second?.began, began - 20, "the count of the turn's time began again")
        let ended = read(&watch, try reply("Done.", going: false), now: began)
        XCTAssertNil(ended)
        // A turn that takes the message up again is a new one.
        let again = read(&watch, try reply(), now: began + 100)
        XCTAssertEqual(again?.began, began)
        XCTAssertEqual(again?.now, .thinking(""))
    }

    func testTheNewestWordsOfALongThoughtBeginAtAWord() {
        let long = (0..<400).map { "word\($0)" }.joined(separator: " ")
        let newest = TurnWatch.newest(of: long)
        XCTAssertTrue(newest.hasSuffix("word399"), newest)
        XCTAssertTrue(newest.hasPrefix("word"), newest)
        XCTAssertLessThan(newest.count, long.count)
        XCTAssertEqual(TurnWatch.newest(of: "One line.\n\n"), "One line.")
        XCTAssertEqual(TurnWatch.newest(of: ""), "")
    }
}
#endif
