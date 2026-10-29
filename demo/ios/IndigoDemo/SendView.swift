import SwiftUI

struct SendView: View {
    @EnvironmentObject var model: WalletModel
    @Environment(\.dismiss) private var dismiss
    @State private var mode = Mode.pay
    @State private var to = ""
    @State private var amount = ""
    @State private var from = "auto"
    @State private var result: SendResult?
    @State private var working = false
    @State private var error: String?

    enum Mode: String, CaseIterable {
        case pay = "Pay", ecash = "Ecash note"
    }

    private var detected: Detected? { detect(text: to) }
    private var needsAmount: Bool { mode == .ecash || (detected != nil && detected?.sats == nil) }

    var body: some View {
        NavigationStack {
            Form {
                Picker("What", selection: $mode) {
                    ForEach(Mode.allCases, id: \.self) { Text($0.rawValue) }
                }
                .pickerStyle(.segmented)

                if let result {
                    done(result)
                } else {
                    if mode == .pay { destination }
                    Section {
                        if needsAmount {
                            TextField("Amount in sats", text: $amount).keyboardType(.numberPad)
                        }
                        Picker("Pay from", selection: $from) {
                            Text("Automatic").tag("auto")
                            ForEach(model.funded.filter(canPay), id: \.pocket) {
                                Text("\(Pockets.name($0.pocket)) · \(sats($0.sats))").tag($0.pocket)
                            }
                        }
                    } footer: {
                        Text("Automatic picks the cheapest pocket that covers it. If none does alone, INDIGO first moves money from your other pockets into one, then pays.")
                    }
                    if let error { Text(error).foregroundStyle(.red) }
                    PrimaryButton(mode == .pay ? "Send" : "Create ecash note") { Task { await send() } }
                        .disabled(!ready || working).formAction()
                        .accessibilityIdentifier("submit")
                    if working { ProgressView() }
                }
            }
            .scrollDismissesKeyboard(.immediately)
            .navigationTitle("Send")
            .toolbar { Button("Done") { dismiss() } }
            .onChange(of: mode) { _ in result = nil; error = nil; from = "auto" }
        }
    }

    @ViewBuilder private var destination: some View {
        Section {
            TextField("Paste an invoice or Ark address", text: $to, axis: .vertical).lineLimit(2...5)
            HStack {
                PasteButton(payloadType: String.self) { strings in to = strings.first?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "" }
                Spacer()
                Button("Test bill (2,500 sats)") { Task { to = (try? await OutsideWorld.bill(sats: 2_500)) ?? "" } }
                    .buttonStyle(.bordered)
            }
        } footer: {
            if let d = detected {
                Text(d.redeem ? "\(d.kind): redeem it under Receive → Ecash."
                     : d.kind + (d.sats.map { " for " + sats($0) } ?? ""))
            } else if !to.isEmpty {
                Text("Not something INDIGO can pay").foregroundStyle(.red)
            }
        }
    }

    @ViewBuilder private func done(_ r: SendResult) -> some View {
        Section("Done") {
            ForEach(r.steps, id: \.self) { Label($0, systemImage: "arrow.triangle.swap").accessibilityIdentifier("step") }
            Label(r.summary, systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                .accessibilityIdentifier("result")
        }
        if let token = r.token {
            Section("Hand this to the other person") { ShareableCode(text: token) }
        }
    }

    private var ready: Bool {
        if needsAmount && UInt64(amount) == nil { return false }
        if mode == .ecash { return true }
        guard let d = detected else { return false }
        return !d.redeem && d.kind != "Bitcoin address"
    }

    /// Which pockets can pay what is being sent.
    private func canPay(_ p: PocketBalance) -> Bool {
        switch mode {
        case .ecash: return p.pocket == "cashu" || p.pocket == "fedimint"
        case .pay: return detected?.kind == "Ark address" ? p.pocket.hasPrefix("ark") : true
        }
    }

    private func send() async {
        working = true
        defer { working = false }
        let pocket = from == "auto" ? nil : from
        do {
            if mode == .ecash {
                result = try await model.w().sendEcash(sats: UInt64(amount) ?? 0, from: pocket)
            } else {
                result = try await model.w().send(to: to, sats: needsAmount ? UInt64(amount) : nil, from: pocket)
            }
            error = nil
            await model.refresh()
        } catch {
            self.error = plain(error)
        }
    }
}
