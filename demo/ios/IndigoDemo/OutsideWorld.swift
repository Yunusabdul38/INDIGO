import Foundation

/// Everyone who is not Alice's app: demo/outside-world.py on this Mac. It
/// pays Alice's invoices, issues bills, funds on-chain addresses and mines.
enum OutsideWorld {
    static let base = URL(string: "http://127.0.0.1:8733")!

    static func federationInvite() async throws -> String {
        try await get("/federation")["invite"] as? String ?? ""
    }

    static func bill(sats: UInt64) async throws -> String {
        try await get("/invoice?sats=\(sats)")["invoice"] as? String ?? ""
    }

    static func pay(_ invoice: String) async throws {
        _ = try await post("/pay", ["invoice": invoice])
    }

    static func fund(_ address: String, btc: Double) async throws {
        _ = try await post("/fund", ["address": address, "btc": btc])
    }

    static func mine(_ blocks: Int) async throws {
        _ = try await post("/mine", ["blocks": blocks])
    }

    private static func get(_ path: String) async throws -> [String: Any] {
        let (data, _) = try await URLSession.shared.data(from: URL(string: path, relativeTo: base)!)
        return try decode(data)
    }

    private static func post(_ path: String, _ body: [String: Any]) async throws -> [String: Any] {
        var request = URLRequest(url: URL(string: path, relativeTo: base)!)
        request.httpMethod = "POST"
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, _) = try await URLSession.shared.data(for: request)
        return try decode(data)
    }

    private static func decode(_ data: Data) throws -> [String: Any] {
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
        if let error = json["error"] as? String { throw DemoError(message: error) }
        return json
    }
}

struct DemoError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// An error as a person should read it: INDIGO's own message, without the
/// Swift type around it.
func plain(_ error: Error) -> String {
    if case let IndigoError.Failed(message) = error { return message }
    return error.localizedDescription
}
