import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit

/// A QR code with the text under it, and Copy / Share buttons.
struct ShareableCode: View {
    let text: String
    @State private var copied = false

    var body: some View {
        VStack(spacing: 12) {
            if let image = Self.qr(text) {
                Image(uiImage: image).interpolation(.none).resizable().scaledToFit()
                    .frame(maxWidth: 240).padding(8).background(.white).clipShape(RoundedRectangle(cornerRadius: 8))
            }
            Text(text).font(.caption2.monospaced()).lineLimit(3).truncationMode(.middle).textSelection(.enabled)
                .accessibilityIdentifier("code")
            HStack {
                Button(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc") {
                    UIPasteboard.general.string = text
                    copied = true
                }
                .buttonStyle(.bordered)
                ShareLink(item: text).buttonStyle(.bordered)
            }
        }
        .frame(maxWidth: .infinity)
    }

    static func qr(_ text: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
              let cg = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}
