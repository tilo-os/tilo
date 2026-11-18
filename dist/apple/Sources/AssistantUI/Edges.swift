import SwiftUI

/// The soft end of a stretch that scrolls: the ground's own colour over what scrolls out there,
/// clear toward the middle. It takes no touch and is not read out.
struct EdgeFade: View {
    let edge: VerticalEdge
    /// The ground the stretch stands on.
    var ground = Token.content
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let colour = ground.color(scheme)
        LinearGradient(colors: [colour, colour.opacity(0)], startPoint: edge == .top ? .top : .bottom,
                       endPoint: edge == .top ? .bottom : .top)
            .frame(height: Measure.edgeFade)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}
