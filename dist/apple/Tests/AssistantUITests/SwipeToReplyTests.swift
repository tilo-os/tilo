#if os(macOS)
@testable import AssistantUI
import XCTest

/// The swipe that answers a piece on an iPhone, by its rule: how far a drag pulls the piece and
/// whether letting go there answers it. The rule is plain numbers of a drag, so it is asked
/// here, where no finger is; the gesture that hands it a finger's drag is the iPhone's own.
final class SwipeToReplyTests: XCTestCase {
    func testADragToTheRightLetGoAtTheMarkAnswersThePieceAndAShorterOneDoesNot() {
        XCTAssertTrue(SwipePull.answers(across: SwipePull.mark, down: 0), "let go at the mark")
        XCTAssertTrue(SwipePull.answers(across: 200, down: 12), "let go far past it, a little off the line")
        XCTAssertFalse(SwipePull.answers(across: SwipePull.mark - 1, down: 0), "let go short of the mark")
        XCTAssertFalse(SwipePull.answers(across: 0, down: 0), "a press that went nowhere")
    }

    func testAScrollOfTheConversationAndADragToTheLeftPullNothing() {
        // More down than across is a scroll, whichever way it goes down or up.
        XCTAssertEqual(SwipePull.pull(across: 80, down: 60), 0)
        XCTAssertEqual(SwipePull.pull(across: 80, down: -60), 0)
        XCTAssertFalse(SwipePull.answers(across: 80, down: 60))
        XCTAssertFalse(SwipePull.answers(across: 80, down: -60))
        // To the left nothing is pulled, however far.
        XCTAssertEqual(SwipePull.pull(across: -200, down: 0), 0)
        XCTAssertFalse(SwipePull.answers(across: -200, down: 0))
    }

    func testThePieceFollowsTheFingerAndNoFurtherThanItsMost() {
        XCTAssertEqual(SwipePull.pull(across: 30, down: 4), 30)
        XCTAssertEqual(SwipePull.pull(across: SwipePull.most, down: 0), SwipePull.most)
        XCTAssertEqual(SwipePull.pull(across: 400, down: 0), SwipePull.most)
        XCTAssertGreaterThanOrEqual(SwipePull.most, SwipePull.mark, "the piece can be pulled as far as its mark")
    }
}
#endif
