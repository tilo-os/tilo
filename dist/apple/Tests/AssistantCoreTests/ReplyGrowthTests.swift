import AssistantCore
import XCTest

/// What a program with no window counts of a reply that grows: each time it drew other words.
final class ReplyGrowthTests: XCTestCase {
    private let asked = Item(id: "asked", at: Date(timeIntervalSince1970: 90), .message(Message(.person, "When is the launch?")))

    private func reply(_ words: String, id: Item.ID = "reply") -> Item {
        Item(id: id, at: Date(timeIntervalSince1970: 100), .message(Message(.assistant, words)))
    }

    private func at(_ milliseconds: Int) -> Date {
        Date(timeIntervalSince1970: 1000 + Double(milliseconds) / 1000)
    }

    func testAReplyThatGrowsIsCountedByEachTimeItsWordsWereDrawnAnew() {
        var conversation = Conversation()
        var growth = ReplyGrowth()
        growth.drew(conversation, at: at(0))
        conversation.apply(.all([asked]))
        growth.drew(conversation, at: at(10))
        // The turn took the message up: its reply stands there and holds no word yet.
        conversation.apply(.added([reply("")]))
        growth.drew(conversation, at: at(20))
        XCTAssertEqual(growth.seen, [:], "a reply with no word yet was counted as drawn")
        // Its model writes. One change is drawn twice: the second drawing shows nothing new.
        let written = [(145, "The launch"), (350, "The launch is planned"), (360, "The launch is planned"),
                       (560, "The launch is planned for May.")]
        for (milliseconds, words) in written {
            conversation.apply(.changed(reply(words)))
            growth.drew(conversation, at: at(milliseconds))
        }
        XCTAssertEqual(growth.seen["reply"], ReplyGrowth.Seen(first: at(145), last: at(560), drew: 3, letters: 30))
        XCTAssertEqual(growth.seen.count, 1, "the person's own message was counted as a reply")
    }

    func testWhatStoodThereAtFirstIsNoNewsAndAReplyThatComesWholeIsDrawnOnce() {
        var conversation = Conversation()
        var growth = ReplyGrowth()
        conversation.apply(.all([asked, reply("An old answer.", id: "old")]))
        growth.drew(conversation, at: at(0))
        XCTAssertEqual(growth.seen, [:], "what the conversation held when it was first drawn was counted")
        conversation.apply(.added([reply("Whole, at once.", id: "whole")]))
        growth.drew(conversation, at: at(900))
        XCTAssertEqual(growth.seen, ["whole": ReplyGrowth.Seen(first: at(900), last: at(900), drew: 1, letters: 15)])
        // The person's own words are no reply.
        conversation.apply(.added([Item(id: "thanks", at: Date(timeIntervalSince1970: 110), .message(Message(.person, "Thanks.")))]))
        growth.drew(conversation, at: at(1000))
        XCTAssertEqual(growth.seen.count, 1)
        // The old answer grows after all: it is counted from then on.
        conversation.apply(.changed(reply("An old answer. And more.", id: "old")))
        growth.drew(conversation, at: at(1200))
        XCTAssertEqual(growth.seen["old"], ReplyGrowth.Seen(first: at(1200), last: at(1200), drew: 1, letters: 24))
    }
}
