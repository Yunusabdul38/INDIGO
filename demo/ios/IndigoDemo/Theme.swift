import SwiftUI

// One look across the app: indigo for actions, red only for destroying
// something, orange only for warnings. Screen headlines are .title2
// semibold, explanations .secondary, small print .footnote.

extension Color {
    static let brand = Color(red: 0.31, green: 0.27, blue: 0.90) // #4F46E5
}

extension View {
    /// A PrimaryButton as a Form row of its own, without the row's card.
    func formAction() -> some View {
        listRowInsets(EdgeInsets()).listRowBackground(Color.clear)
    }
}

/// A screen's main action: full width, large, filled.
struct PrimaryButton: View {
    let title: String
    let role: ButtonRole?
    let tint: Color
    let action: () -> Void

    init(_ title: String, role: ButtonRole? = nil, tint: Color = .brand, action: @escaping () -> Void) {
        self.title = title
        self.role = role
        self.tint = tint
        self.action = action
    }

    var body: some View {
        Button(role: role, action: action) {
            Text(title).fontWeight(.semibold).frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent).controlSize(.large).tint(tint)
    }
}
