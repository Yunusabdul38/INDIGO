import Foundation
import LocalAuthentication
import Security

/// The 12 words live only in this phone's Keychain: encrypted by iOS, never
/// in backups or iCloud (ThisDeviceOnly), and never sent anywhere by INDIGO.
enum Keychain {
    private static let account = "indigo-recovery-phrase"

    static var words: String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func save(_ words: String) {
        delete()
        let item: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: account,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            kSecValueData as String: Data(words.utf8),
        ]
        SecItemAdd(item as CFDictionary, nil)
    }

    static func delete() {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrAccount as String: account]
        SecItemDelete(query as CFDictionary)
    }

    /// Face ID, Touch ID or the passcode before showing the words. A
    /// Simulator with no passcode set has nothing to check: it lets through.
    static func unlock() async -> Bool {
        let context = LAContext()
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else { return true }
        return (try? await context.evaluatePolicy(.deviceOwnerAuthentication,
                                                  localizedReason: "Show your recovery phrase")) ?? false
    }
}
