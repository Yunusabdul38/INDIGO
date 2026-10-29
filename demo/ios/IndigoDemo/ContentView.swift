import SwiftUI

struct ContentView: View {
    @StateObject private var model = WalletModel()

    var body: some View {
        Group {
            if let words = model.newPhrase {
                BackupView(words: words, onDone: { model.setBackedUp(true); model.newPhrase = nil },
                           onLater: { model.newPhrase = nil })
            } else if model.hasWallet {
                HomeView()
            } else {
                OnboardingView()
            }
        }
        .environmentObject(model)
        .task { await model.open() }
    }
}

struct OnboardingView: View {
    @EnvironmentObject var model: WalletModel
    @State private var name = ""
    @State private var restoring = false
    @State private var words = ""

    private var nameOK: Bool { !name.trimmingCharacters(in: .whitespaces).isEmpty }

    var body: some View {
        ScrollView {
            VStack(spacing: 20) {
                Text("INDIGO").font(.largeTitle.bold()).foregroundStyle(Color.brand).padding(.top, 80)
                Text("One wallet for every Bitcoin layer: Cashu, Fedimint and Ark, joined by Lightning.")
                    .multilineTextAlignment(.center).foregroundStyle(.secondary)
                VStack(spacing: 12) {
                    TextField("Your name", text: $name).textFieldStyle(.roundedBorder)
                    if restoring {
                        TextField("Your 12 recovery words", text: $words, axis: .vertical)
                            .lineLimit(3...5).textFieldStyle(.roundedBorder)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                        PrimaryButton("Restore my wallet") { Task { await model.restore(name: name, words: words) } }
                            .disabled(!nameOK || words.isEmpty || model.opening)
                        if model.opening { ProgressView("Finding your money in every pocket…").font(.footnote) }
                        Button("Create a new wallet instead") { restoring = false }
                    } else {
                        PrimaryButton("Create a new wallet") { Task { await model.create(name: name) } }
                            .disabled(!nameOK)
                        Button("I have a recovery phrase") { restoring = true }
                    }
                }
                .frame(maxWidth: 320)
                Text("Your wallet lives only on this phone. INDIGO has no account, no server copy and no way to reset it: your 12 recovery words are the only backup.")
                    .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center).padding(.top)
            }
            .padding(24)
        }
        .alert(model.error ?? "", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {}
    }
}

/// Backing up: the 12 words, then three of them typed back from the paper.
/// `onLater` lets the user skip it for now.
struct BackupView: View {
    let words: String
    let onDone: () -> Void
    var onLater: (() -> Void)?
    @State private var checking = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                if checking {
                    CheckPhraseView(words: words.split(separator: " ").map(String.init), onDone: onDone) {
                        checking = false
                    }
                } else {
                    PhraseView(words: words)
                    VStack(spacing: 12) {
                        PrimaryButton("I've written them down") { checking = true }
                        if let onLater { Button("Back up later", action: onLater) }
                    }
                }
            }
            .padding(24)
        }
    }
}

/// The 12 words, with what they mean.
struct PhraseView: View {
    let words: String

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Your recovery phrase").font(.title2.weight(.semibold))
            Text("These 12 words are your wallet. If you lose this phone, they bring back everything in every pocket.")
                .foregroundStyle(.secondary)
            LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 10) {
                ForEach(Array(words.split(separator: " ").enumerated()), id: \.offset) { i, word in
                    HStack {
                        Text("\(i + 1).").foregroundStyle(.secondary).frame(width: 28, alignment: .trailing)
                        Text(String(word)).font(.body.monospaced()).accessibilityIdentifier("word-\(i + 1)")
                        Spacer()
                    }
                    .padding(8).background(.quaternary).clipShape(RoundedRectangle(cornerRadius: 8))
                }
            }
            VStack(alignment: .leading, spacing: 8) {
                Label("Write them on paper, in order. Keep it somewhere safe.", systemImage: "pencil")
                Label("Anyone with these words can take your money. Never type them into a website or share them.", systemImage: "exclamationmark.shield")
                Label("INDIGO keeps no copy. They stay in this phone's Keychain and never leave it.", systemImage: "iphone")
            }
            .font(.callout)
        }
    }
}

/// Three words, picked at random, typed back from the paper.
struct CheckPhraseView: View {
    let words: [String]
    let onDone: () -> Void
    let onBack: () -> Void
    @State private var picks = [Int]()
    @State private var typed = [Int: String]()
    @State private var wrong = false

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            VStack(alignment: .leading, spacing: 8) {
                Text("Check your paper").font(.title2.weight(.semibold))
                Text("Type these words from what you wrote down, to be sure it can bring your wallet back.")
                    .foregroundStyle(.secondary)
            }
            ForEach(picks, id: \.self) { i in
                VStack(alignment: .leading, spacing: 8) {
                    Text("Word \(i + 1)").font(.subheadline)
                    TextField("", text: Binding(get: { typed[i] ?? "" }, set: { typed[i] = $0; wrong = false }))
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityIdentifier("check-\(i + 1)")
                }
            }
            if wrong {
                Text("That doesn't match. Check the spelling and the order on your paper.")
                    .font(.callout).foregroundStyle(.red)
            }
            VStack(spacing: 12) {
                PrimaryButton("Confirm") {
                    let right = picks.allSatisfy { typed[$0]?.trimmingCharacters(in: .whitespaces).lowercased() == words[$0] }
                    if right { onDone() } else { wrong = true }
                }
                .disabled(picks.contains { (typed[$0] ?? "").isEmpty })
                Button("Show the words again", action: onBack)
            }
        }
        .onAppear { if picks.isEmpty { picks = Array(words.indices.shuffled().prefix(3)).sorted() } }
    }
}

struct HomeView: View {
    @EnvironmentObject var model: WalletModel
    @State private var sheet: Sheet?
    @State private var message: String?
    @State private var working = false
    @State private var phrase: String?
    @State private var confirmDelete = false
    @State private var askBackup = false
    @State private var askedBackup = false

    enum Sheet: String, Identifiable {
        case receive, send
        var id: String { rawValue }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Balance").font(.subheadline).foregroundStyle(.secondary)
                        Text(sats(model.total)).font(.system(size: 38, weight: .bold)).monospacedDigit()
                            .accessibilityIdentifier("balance")
                    }
                    .padding(.vertical, 4)
                    HStack(spacing: 12) {
                        PrimaryButton("Receive") { sheet = .receive }
                        PrimaryButton("Send") { sheet = .send }
                    }
                    .disabled(model.wallet == nil)
                }
                if !model.backedUp { backupBanner }
                Section("Pockets") {
                    ForEach(model.pockets, id: \.pocket) { p in
                        HStack {
                            Image(systemName: Pockets.icon(p.pocket)).frame(width: 28).foregroundStyle(.tint)
                            VStack(alignment: .leading) {
                                Text(Pockets.name(p.pocket))
                                Text(Pockets.detail(p.pocket)).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(sats(p.sats)).monospacedDigit()
                        }
                    }
                    if model.onchainSats > 0 {
                        HStack {
                            Image(systemName: "bitcoinsign.circle").frame(width: 28).foregroundStyle(.tint)
                            VStack(alignment: .leading) {
                                Text("On-chain")
                                Text("Waiting to be moved into Ark").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("Move into Ark") { Task { await boardAll() } }.buttonStyle(.bordered).disabled(working)
                        }
                    }
                }
                Section("Activity") {
                    if model.activity.isEmpty { Text("Nothing yet").foregroundStyle(.secondary) }
                    ForEach(Array(model.activity.prefix(30).enumerated()), id: \.offset) { _, a in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(a.title)
                                Text("\(Pockets.name(a.pocket)) · \(a.status.lowercased())").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text((a.incoming ? "+" : "") + sats(a.sats)).monospacedDigit()
                                .foregroundStyle(a.incoming ? .green : .primary)
                        }
                    }
                }
            }
            .navigationTitle(model.name.isEmpty ? "INDIGO" : "Hi, \(model.name)")
            .toolbar {
                if model.opening || working { ProgressView() }
                Menu {
                    Button("Get 50,000 test sats", systemImage: "drop") { Task { await faucet() } }
                    Button("Check the books", systemImage: "checkmark.seal") { Task { await audit() } }
                    Button("Recovery phrase", systemImage: "key") { Task { await showPhrase() } }
                    Button("Delete this wallet", systemImage: "trash", role: .destructive) { confirmDelete = true }
                } label: { Image(systemName: "ellipsis.circle") }
            }
            .refreshable { await model.refresh() }
            .sheet(item: $sheet, onDismiss: { Task { await model.refresh(); askBackupIfFunded() } }) { s in
                switch s {
                case .receive: ReceiveView()
                case .send: SendView()
                }
            }
            .sheet(item: Binding(get: { phrase.map(Phrase.init) }, set: { phrase = $0?.words })) { p in
                if model.backedUp {
                    ScrollView {
                        VStack(spacing: 24) {
                            PhraseView(words: p.words)
                            PrimaryButton("Done") { phrase = nil }
                        }
                        .padding(24)
                    }
                } else {
                    BackupView(words: p.words, onDone: { model.setBackedUp(true); phrase = nil }, onLater: { phrase = nil })
                }
            }
            .sheet(isPresented: $confirmDelete) { DeleteWalletView() }
            .alert("Back up your wallet now?", isPresented: $askBackup) {
                Button("Back up now") { Task { await showPhrase() } }
                Button("Later", role: .cancel) {}
            } message: {
                Text("You have money in this wallet. Your 12 recovery words are the only way to get it back if you lose this phone.")
            }
            .alert(message ?? "", isPresented: Binding(get: { message != nil }, set: { if !$0 { message = nil } })) {}
            .alert(model.error ?? "", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {}
        }
    }

    /// Stronger once there is money that only this phone can reach.
    private var backupBanner: some View {
        Section {
            Button { Task { await showPhrase() } } label: {
                VStack(alignment: .leading, spacing: 4) {
                    Label(model.total > 0 ? "Back up now: your money is at risk" : "Back up your wallet",
                          systemImage: "exclamationmark.triangle.fill")
                        .font(.headline).foregroundStyle(.orange)
                    Text(model.total > 0
                         ? "\(sats(model.total)) can only be reached from this phone until you write down your 12 words."
                         : "Write down your 12 recovery words before adding money.")
                        .font(.footnote).foregroundStyle(Color.secondary)
                }
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("backup-banner")
        }
    }

    /// The first time money is here without a backup, ask once.
    private func askBackupIfFunded() {
        guard !model.backedUp, !askedBackup, model.total > 0 else { return }
        askedBackup = true
        askBackup = true
    }

    private func showPhrase() async {
        if let words = await model.revealPhrase() { phrase = words }
    }

    /// The test network's faucet: the outside world pays an invoice.
    private func faucet() async {
        await act {
            let invoice = try await model.w().request(sats: 50_000, into: nil)
            try await OutsideWorld.pay(invoice.invoice)
            let status = try await model.w().waitReceived(op: invoice.op, seconds: 60)
            return "50,000 test sats → \(Pockets.name(invoice.pocket)): \(status.lowercased())"
        }
    }

    private func boardAll() async {
        await act {
            _ = try await model.w().arkBoardAll()
            for _ in 0..<20 {
                try await OutsideWorld.mine(1)
                try await Task.sleep(nanoseconds: 2_000_000_000)
                if (try await model.w().arkOnchainSats()) < 1 { break }
            }
            return "Moved into Ark"
        }
    }

    private func audit() async {
        await act {
            let lines = try await model.w().audit()
            let ok = lines.allSatisfy(\.matches)
            return ok ? "Every pocket's journal matches its balance exactly." :
                "Mismatch: " + lines.filter { !$0.matches }.map(\.pocket).joined(separator: ", ")
        }
    }

    private func act(_ body: () async throws -> String) async {
        working = true
        do { message = try await body() } catch { message = plain(error) }
        await model.refresh()
        working = false
    }
}

private struct Phrase: Identifiable {
    let words: String
    var id: String { words }
}

/// Deleting asks the user to type their wallet's name, after saying exactly
/// what deleting does and what it costs if the words were not written down.
struct DeleteWalletView: View {
    @EnvironmentObject var model: WalletModel
    @Environment(\.dismiss) private var dismiss
    @State private var typed = ""

    private var matches: Bool {
        typed.trimmingCharacters(in: .whitespaces) == model.name.trimmingCharacters(in: .whitespaces)
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 28) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Delete \(model.name)'s wallet?").font(.title2.weight(.semibold))
                        Text("This removes the wallet and its recovery words from this iPhone.")
                            .foregroundStyle(.secondary)
                    }

                    VStack(alignment: .leading, spacing: 14) {
                        point("Your \(sats(model.total)) is not moved. It stays with the mint, the federation and the Ark server.")
                        point("Only your 12 recovery words can bring it back.")
                        point("INDIGO keeps no copy, so this cannot be undone.")
                    }

                    if !model.backedUp {
                        Text("You haven't backed up your recovery words. Deleting now loses this money for good.")
                            .font(.callout.weight(.medium)).foregroundStyle(.red)
                    }

                    VStack(alignment: .leading, spacing: 8) {
                        Text("Type **\(model.name)** to confirm").font(.subheadline)
                        TextField(model.name, text: $typed)
                            .textFieldStyle(.roundedBorder)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                    }

                    PrimaryButton("Delete Wallet", role: .destructive, tint: .red) {
                        model.reset()
                        dismiss()
                    }
                    .disabled(!matches)
                }
                .padding(24)
            }
            .navigationTitle("Delete wallet")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { Button("Cancel") { dismiss() } }
        }
    }

    private func point(_ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Circle().fill(.secondary).frame(width: 5, height: 5).alignmentGuide(.firstTextBaseline) { $0[.bottom] + 4 }
            Text(text).fixedSize(horizontal: false, vertical: true)
        }
    }
}
