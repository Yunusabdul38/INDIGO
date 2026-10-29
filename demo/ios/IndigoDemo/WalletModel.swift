import Foundation

/// This device's INDIGO wallet: one person, one wallet.
@MainActor
final class WalletModel: ObservableObject {
    @Published var name = UserDefaults.standard.string(forKey: "name") ?? ""
    @Published var pockets: [PocketBalance] = []
    @Published var activity: [Activity] = []
    @Published var onchainSats: Double = 0
    @Published var opening = false
    @Published var error: String?
    /// Set once the user confirmed writing the 12 words down.
    @Published var backedUp = UserDefaults.standard.bool(forKey: "backedUp")
    /// The words to show right after creating a wallet.
    @Published var newPhrase: String?

    private(set) var wallet: IndigoWallet?

    init() { Self.removeOldFolders() }

    var total: Double { pockets.reduce(0) { $0 + $1.sats } }
    var hasWallet: Bool { !name.isEmpty && Keychain.words != nil }

    /// Pockets with money in them, for "pay from" pickers.
    var funded: [PocketBalance] { pockets.filter { $0.sats >= 1 } }

    /// Creates the wallet on first launch; opens it after.
    func open() async {
        guard hasWallet, wallet == nil else { return }
        opening = true
        defer { opening = false }
        do {
            guard let words = Keychain.words else { return }
            let wallet = try await IndigoWallet.open(dataDir: Self.dir.path, words: words)
            try await wallet.joinFederation(invite: try await OutsideWorld.federationInvite())
            self.wallet = wallet
            await refresh()
        } catch {
            self.error = plain(error)
        }
    }

    func create(name: String) async {
        Self.newDir()
        let words = newWords()
        Keychain.save(words)
        setName(name)
        setBackedUp(false)
        newPhrase = words
        await open()
    }

    /// A new phone: the wallet comes back from its 12 words. Every pocket
    /// asks its operator what the words own; the federation is rescanned.
    func restore(name: String, words: String) async {
        let words = words.lowercased().split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard wordsValid(words: words) else {
            error = "Those are not 12 valid recovery words. Check the spelling and order."
            return
        }
        opening = true
        defer { opening = false }
        do {
            Self.newDir()
            let invite = try await OutsideWorld.federationInvite()
            wallet = try await IndigoWallet.restore(dataDir: Self.dir.path, words: words, invite: invite)
            Keychain.save(words)
            setName(name)
            setBackedUp(true)
            await refresh()
        } catch {
            wallet = nil
            self.error = plain(error)
        }
    }

    /// The words, after Face ID or the passcode.
    func revealPhrase() async -> String? {
        await Keychain.unlock() ? Keychain.words : nil
    }

    func setBackedUp(_ value: Bool) {
        backedUp = value
        UserDefaults.standard.set(value, forKey: "backedUp")
    }

    private func setName(_ name: String) {
        self.name = name
        UserDefaults.standard.set(name, forKey: "name")
    }

    func refresh() async {
        guard let wallet else { return }
        pockets = await wallet.balances().sorted { Pockets.order($0.pocket) < Pockets.order($1.pocket) }
        activity = (try? await wallet.activity()) ?? []
        onchainSats = (try? await wallet.arkOnchainSats()) ?? 0
    }

    /// Deletes this device's wallet. Only the 12 words bring it back.
    func reset() {
        try? FileManager.default.removeItem(at: Self.dir)
        UserDefaults.standard.removeObject(forKey: "walletDir")
        Keychain.delete()
        UserDefaults.standard.removeObject(forKey: "name")
        UserDefaults.standard.removeObject(forKey: "backedUp")
        wallet = nil
        name = ""
        pockets = []
        activity = []
        onchainSats = 0
    }

    func w() throws -> IndigoWallet {
        guard let wallet else { throw DemoError(message: "The wallet is still opening") }
        return wallet
    }

    /// Each wallet gets a folder of its own. A deleted wallet's Fedimint
    /// database stays locked until the app quits, so a wallet restored right
    /// after must not reuse its folder.
    private static var dir: URL {
        root.appendingPathComponent(UserDefaults.standard.string(forKey: "walletDir") ?? "wallet")
    }

    private static let root = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]

    private static func newDir() {
        UserDefaults.standard.set("wallet-\(UUID().uuidString)", forKey: "walletDir")
    }

    /// Folders of deleted wallets. Called at launch, when nothing holds them.
    static func removeOldFolders() {
        let current = dir.lastPathComponent
        let names = (try? FileManager.default.contentsOfDirectory(atPath: root.path)) ?? []
        for name in names where name.hasPrefix("wallet") && name != current {
            try? FileManager.default.removeItem(at: root.appendingPathComponent(name))
        }
    }
}

enum Pockets {
    static func order(_ pocket: String) -> Int {
        ["cashu", "fedimint", "ark-bark"].firstIndex(of: pocket) ?? 9
    }

    static func name(_ pocket: String) -> String {
        switch pocket {
        case "cashu": return "Cashu"
        case "fedimint": return "Fedimint"
        case "ark-bark": return "Ark"
        default: return pocket
        }
    }

    static func detail(_ pocket: String) -> String {
        switch pocket {
        case "cashu": return "Ecash from a mint · instant, private"
        case "fedimint": return "Ecash from a federation · shared custody"
        case "ark-bark": return "Ark · self-custody, exit to Bitcoin any time"
        default: return ""
        }
    }

    static func icon(_ pocket: String) -> String {
        switch pocket {
        case "cashu": return "banknote"
        case "fedimint": return "building.columns"
        case "ark-bark": return "lock.shield"
        default: return "circle"
        }
    }
}

/// "12,345 sats", with msat decimals only when there are some.
func sats(_ value: Double) -> String {
    let f = NumberFormatter()
    f.numberStyle = .decimal
    f.maximumFractionDigits = value.rounded() == value ? 0 : 3
    return (f.string(from: NSNumber(value: value)) ?? "\(value)") + " sats"
}
