import AssistantCore
import XCTest

/// Transparency controls on `Supervision`: per-cause daily spend with honest unknown pricing,
/// dated learned facts with confirmation authority, muted offers and workspace data-destination
/// policy notes.
final class SupervisionTransparencyTests: XCTestCase {
    func testDefaultsStayBackwardCompatible() {
        let usage = Supervision.Usage()
        XCTAssertTrue(usage.causes.isEmpty)
        XCTAssertFalse(usage.isUnknownPrice)
        XCTAssertFalse(usage.hasUnknownPrice)

        let fact = Supervision.Learned(id: "f1", text: "Prefers short replies")
        XCTAssertNil(fact.window)
        XCTAssertFalse(fact.confirmed)

        let facts = Supervision()
        XCTAssertTrue(facts.mutedOffers.isEmpty)
        XCTAssertNil(facts.destinationNote)
        XCTAssertNil(facts.trimmedDestinationNote)
    }

    func testCauseSpendMapsServerKindsAndFormatsCostsHonestly() {
        let mainChat = Supervision.Usage.CauseSpend(kind: "turn", tokens: 120_000, costCents: 45, isUnknownPrice: false)
        let events = Supervision.Usage.CauseSpend(kind: "event", tokens: 30_000, costCents: 100, isUnknownPrice: false)
        let lookRound = Supervision.Usage.CauseSpend(kind: "look", tokens: 12_500, costCents: 0, isUnknownPrice: true)
        let reflection = Supervision.Usage.CauseSpend(kind: "reflection", tokens: 8_000, costCents: 12, isUnknownPrice: true)
        let shadow = Supervision.Usage.CauseSpend(kind: "shadow", tokens: 2_000, costCents: 0, isUnknownPrice: false)
        let custom = Supervision.Usage.CauseSpend(kind: "Custom check", tokens: 500, costCents: 5, isUnknownPrice: false)
        let blank = Supervision.Usage.CauseSpend(kind: "  ", tokens: 0, costCents: 0, isUnknownPrice: false)

        XCTAssertEqual(mainChat.label, "Main chat")
        XCTAssertEqual(events.label, "Events")
        XCTAssertEqual(lookRound.label, "Look-round")
        XCTAssertEqual(reflection.label, "Reflection")
        XCTAssertEqual(shadow.label, "Shadow")
        XCTAssertEqual(custom.label, "Custom check")
        XCTAssertEqual(blank.label, "Other")

        XCTAssertEqual(mainChat.costText(currency: "$"), "0.45 $")
        XCTAssertEqual(events.costText(currency: "$"), "1 $")
        XCTAssertEqual(lookRound.costText(currency: "$"), "Price unknown")
        XCTAssertEqual(reflection.costText(currency: "$"), "0.12 $ · Price unknown")
        XCTAssertEqual(shadow.costText(currency: "$"), "0 $")
    }

    func testSpentDisplayNeverHidesUnknownPricingAsZero() {
        let unpricedOnly = Supervision.Usage(
            messagesToday: 4,
            tokensToday: 18_000,
            spentTodayMicros: 0,
            causes: [Supervision.Usage.CauseSpend(kind: "turn", tokens: 18_000, costCents: 0, isUnknownPrice: true)]
        )
        XCTAssertTrue(unpricedOnly.hasUnknownPrice)
        let display = unpricedOnly.spentDisplay(formattedKnown: "0 $")
        XCTAssertEqual(display.text, "Price unknown")
        XCTAssertFalse(display.isKnown)

        let topLevelFlag = Supervision.Usage(spentTodayMicros: 0, isUnknownPrice: true)
        XCTAssertTrue(topLevelFlag.hasUnknownPrice)
        XCTAssertEqual(topLevelFlag.spentDisplay(formattedKnown: "0 $").text, "Price unknown")
        XCTAssertFalse(topLevelFlag.spentDisplay(formattedKnown: "0 $").isKnown)

        let mixed = Supervision.Usage(
            spentTodayMicros: 450_000,
            causes: [
                Supervision.Usage.CauseSpend(kind: "turn", tokens: 10_000, costCents: 45, isUnknownPrice: false),
                Supervision.Usage.CauseSpend(kind: "look", tokens: 4_000, costCents: 0, isUnknownPrice: true),
            ]
        )
        XCTAssertTrue(mixed.hasUnknownPrice)
        let mixedDisplay = mixed.spentDisplay(formattedKnown: "0.45 $")
        XCTAssertEqual(mixedDisplay.text, "0.45 $")
        XCTAssertTrue(mixedDisplay.isKnown)

        let knownZero = Supervision.Usage(spentTodayMicros: 0)
        XCTAssertFalse(knownZero.hasUnknownPrice)
        let knownZeroDisplay = knownZero.spentDisplay(formattedKnown: "0 $")
        XCTAssertEqual(knownZeroDisplay.text, "0 $")
        XCTAssertTrue(knownZeroDisplay.isKnown)
    }

    func testDatedLearnedFactAndConfirmationAuthority() {
        let confirmed = Supervision.Learned(
            id: "plan",
            text: "Moving offices",
            window: "in 2 to 5 months · 17 Feb – 17 May",
            confirmed: true
        )
        let inferred = Supervision.Learned(
            id: "style",
            text: "Prefers bullet points",
            window: nil,
            confirmed: false
        )

        XCTAssertEqual(confirmed.window, "in 2 to 5 months · 17 Feb – 17 May")
        XCTAssertTrue(confirmed.confirmed)
        XCTAssertNil(inferred.window)
        XCTAssertFalse(inferred.confirmed)
    }

    func testMutedOffersAndWorkspaceDestinationPolicyVisibility() {
        let restricted = Supervision(
            mutedOffers: ["Archive messages in Mail", "Label calendar invites"],
            destinationNote: "  This server only  "
        )
        XCTAssertEqual(restricted.mutedOffers.count, 2)
        XCTAssertEqual(restricted.trimmedDestinationNote, "This server only")

        let blankPolicy = Supervision(destinationNote: "   \n ")
        XCTAssertNil(blankPolicy.trimmedDestinationNote)
    }
}
