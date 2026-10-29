import SwiftUI

struct ReceiveView: View {
    @EnvironmentObject var model: WalletModel
    @Environment(\.dismiss) private var dismiss
    @State private var mode = Mode.lightning
    @State private var amount = ""
    @State private var into = "auto"
    @State private var invoice: Invoice?
    @State private var status: String?
    @State private var address: String?
    @State private var token = ""
    @State private var working = false
    @State private var error: String?

    enum Mode: String, CaseIterable {
        case lightning = "Lightning", ark = "Ark", onchain = "On-chain", ecash = "Ecash"
    }

    var body: some View {
        NavigationStack {
            Form {
                Picker("How", selection: $mode) {
                    ForEach(Mode.allCases, id: \.self) { Text($0.rawValue) }
                }
                .pickerStyle(.segmented)
                .onChange(of: mode) { _ in Task { await loadAddress() } }

                switch mode {
                case .lightning: lightning
                case .ark, .onchain: addressSection
                case .ecash: redeem
                }
                if let error { Text(error).foregroundStyle(.red) }
            }
            .scrollDismissesKeyboard(.immediately)
            .navigationTitle("Receive")
            .toolbar { Button("Done") { dismiss() } }
        }
    }

    @ViewBuilder private var lightning: some View {
        if let invoice {
            Section {
                ShareableCode(text: invoice.invoice)
                Label(status ?? "Waiting for payment…", systemImage: status == nil ? "hourglass" : "checkmark.circle.fill")
                    .foregroundStyle(status == nil ? Color.secondary : Color.green)
                Text("Goes into \(Pockets.name(invoice.pocket))").font(.caption).foregroundStyle(.secondary)
            }
            if status == nil {
                Section {
                    Button("Pay it from outside (test network)") { Task { try? await OutsideWorld.pay(invoice.invoice) } }
                }
            }
        } else {
            Section {
                TextField("Amount in sats", text: $amount).keyboardType(.numberPad)
                Picker("Into", selection: $into) {
                    Text("Automatic").tag("auto")
                    ForEach(model.pockets, id: \.pocket) { Text(Pockets.name($0.pocket)).tag($0.pocket) }
                }
            } footer: {
                Text("Automatic: small amounts go to ecash (instant), 100,000 sats and up to Ark (self-custody).")
            }
            PrimaryButton("Create invoice") { Task { await createInvoice() } }
                .disabled(UInt64(amount) == nil || working).formAction()
        }
    }

    @ViewBuilder private var addressSection: some View {
        Section {
            if let address { ShareableCode(text: address) } else { ProgressView() }
        } footer: {
            Text(mode == .ark
                ? "Another Ark wallet pays this address directly, no Lightning needed."
                : "Bitcoin sent here shows up as on-chain coins you can move into Ark.")
        }
        if mode == .onchain, let address {
            Button("Fund it from the test faucet (0.001 BTC)") {
                Task { try? await OutsideWorld.fund(address, btc: 0.001); await model.refresh() }
            }
        }
    }

    @ViewBuilder private var redeem: some View {
        Section {
            // One line: a token is one long word, and a growing field would
            // push Redeem pages down.
            TextField("Paste a Cashu token or Fedimint notes", text: $token)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
            PasteButton(payloadType: String.self) { strings in token = strings.first ?? "" }
        } footer: {
            if let d = detect(text: token), d.redeem {
                Text(d.kind)
            } else if !token.isEmpty {
                Text("Not a Cashu token or Fedimint notes").foregroundStyle(.red)
            }
        }
        if let status { Label(status, systemImage: "checkmark.circle.fill").foregroundStyle(.green) }
        PrimaryButton("Redeem") { Task { await redeemToken() } }.disabled(token.isEmpty || working).formAction()
    }

    private func createInvoice() async {
        guard let sats = UInt64(amount) else { return }
        working = true
        defer { working = false }
        do {
            let invoice = try await model.w().request(sats: sats, into: into == "auto" ? nil : into)
            self.invoice = invoice
            error = nil
            // Keep claiming while the sheet is open: some pockets (Ark) need the
            // receiver online while the payment arrives.
            while status == nil {
                let s = try await model.w().waitReceived(op: invoice.op, seconds: 5)
                if s != "PENDING" { status = s == "COMPLETED" ? "Received \(UInt64(amount) ?? 0) sats" : s.lowercased() }
            }
            await model.refresh()
        } catch {
            self.error = plain(error)
        }
    }

    private func loadAddress() async {
        address = nil
        do {
            switch mode {
            case .ark: address = try await model.w().arkAddress()
            case .onchain: address = try await model.w().arkOnchainAddress()
            default: break
            }
        } catch { self.error = plain(error) }
    }

    private func redeemToken() async {
        working = true
        defer { working = false }
        do {
            status = try await model.w().redeem(token: token)
            token = ""
            error = nil
            await model.refresh()
        } catch { self.error = plain(error) }
    }
}
