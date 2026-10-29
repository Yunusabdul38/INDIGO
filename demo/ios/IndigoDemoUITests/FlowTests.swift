import XCTest

/// Alice and Bob on two simulators, tapping through the app the way a
/// person would. tests/ios-ui.sh runs the steps in order, alternating
/// devices; they hand each other tokens and addresses through SHARE_DIR.
final class FlowTests: XCTestCase {
    let app = XCUIApplication()

    override func setUp() {
        continueAfterFailure = false
        app.launch()
    }

    /// Alice: a new wallet, backed up, funded, pays a bill, makes ecash for Bob.
    func testA1Alice() throws {
        create("Alice")
        let words = words()
        try share("alice.words", words)
        app.buttons["I've written them down"].tap()
        checkPaper(words.split(separator: " ").map(String.init))
        waitForHome("Alice")
        XCTAssertFalse(app.buttons["backup-banner"].exists, "backed up, yet still asked to")

        menu("Get 50,000 test sats")
        let funded = alert(timeout: 120)
        XCTAssertTrue(funded.contains("completed"), funded)
        XCTAssertGreaterThanOrEqual(balance(), 50_000)

        app.buttons["Send"].firstMatch.tap()
        app.buttons["Test bill (2,500 sats)"].tap()
        tapSubmit()
        XCTAssertTrue(result().label.hasPrefix("Paid"), "the bill: \(result().label)")

        app.buttons["Ecash note"].tap()
        field("Amount in sats").tap()
        field("Amount in sats").typeText("1500")
        tapSubmit()
        let code = app.staticTexts["code"]
        XCTAssertTrue(code.waitForExistence(timeout: 60), "no ecash note")
        try share("token", code.label)
        app.buttons["Done"].tap()

        try books()
    }

    /// Bob: a new wallet on his own phone, backed up later. Redeems Alice's
    /// note (and is asked to back up), shows his Ark address.
    func testB1Bob() throws {
        create("Bob")
        app.buttons["Back up later"].tap()
        waitForHome("Bob")
        XCTAssertTrue(app.buttons["backup-banner"].exists, "no reminder to back up")

        app.buttons["Receive"].firstMatch.tap()
        app.buttons["Ecash"].tap()
        let token = field("Paste a Cashu token or Fedimint notes")
        token.tap()
        token.typeText(try shared("token"))
        reveal(app.buttons["Redeem"])
        app.buttons["Redeem"].tap()
        XCTAssertTrue(text(beginningWith: "Received").waitForExistence(timeout: 60), "Bob could not redeem")

        app.buttons["Ark"].tap()
        let code = app.staticTexts["code"]
        XCTAssertTrue(code.waitForExistence(timeout: 60), "no Ark address")
        try share("bob.ark", code.label)
        app.buttons["Done"].tap()

        // Money without a backup: asked once, then warned until it's done.
        let ask = app.alerts["Back up your wallet now?"]
        XCTAssertTrue(ask.waitForExistence(timeout: 10), "not asked to back up")
        ask.buttons["Later"].tap()
        XCTAssertTrue(app.staticTexts["Back up now: your money is at risk"].waitForExistence(timeout: 10))
        XCTAssertTrue(waitForBalance { $0 >= 1_500 }, "Bob's balance: \(balance())")
    }

    /// Alice pays Bob's Ark address from an empty Ark pocket (the app tops it
    /// up first), then loses her phone and comes back from her 12 words.
    func testA2Alice() throws {
        waitForHome("Alice")
        app.buttons["Send"].firstMatch.tap()
        let to = field("Paste an invoice or Ark address")
        to.tap()
        to.typeText(try shared("bob.ark"))
        field("Amount in sats").tap()
        field("Amount in sats").typeText("2000")
        reveal(app.buttons["submit"])
        tapSubmit()
        XCTAssertTrue(result(timeout: 240).label.hasPrefix("Paid"), "the Ark payment: \(result().label)")
        XCTAssertTrue(app.descendants(matching: .any)["step"].firstMatch.label.hasPrefix("Moved"), "expected a top-up into Ark first")
        app.buttons["Done"].tap()

        try books()
        let before = app.staticTexts["balance"].label

        // Delete: refused until the name is typed.
        menu("Delete this wallet")
        let delete = app.buttons["Delete Wallet"]
        XCTAssertTrue(delete.waitForExistence(timeout: 10))
        XCTAssertFalse(delete.isEnabled)
        let name = app.textFields.firstMatch
        name.tap()
        name.typeText("Alice")
        XCTAssertTrue(delete.isEnabled)
        delete.tap()

        // A "new phone": back from the 12 words alone.
        let nameField = field("Your name")
        XCTAssertTrue(nameField.waitForExistence(timeout: 10))
        nameField.tap()
        nameField.typeText("Alice")
        app.buttons["I have a recovery phrase"].tap()
        let words = field("Your 12 recovery words")
        words.tap()
        words.typeText(try shared("alice.words"))
        app.buttons["Restore my wallet"].tap()
        waitForHome("Alice", timeout: 300)
        XCTAssertTrue(waitForBalance(timeout: 120) { _ in self.app.staticTexts["balance"].label == before },
                      "restored \(app.staticTexts["balance"].label), had \(before)")

        try books()
    }

    /// Bob sees Alice's Ark payment.
    func testB2Bob() throws {
        waitForHome("Bob")
        XCTAssertTrue(waitForBalance(timeout: 120) { $0 >= 3_500 }, "Bob's balance: \(balance())")
    }

    // MARK: - Helpers

    private func create(_ name: String) {
        let field = field("Your name")
        XCTAssertTrue(field.waitForExistence(timeout: 20), "not on the first screen")
        field.tap()
        field.typeText(name)
        app.buttons["Create a new wallet"].tap()
        XCTAssertTrue(app.staticTexts["word-12"].waitForExistence(timeout: 30), "no recovery phrase shown")
    }

    /// Types back the three words the app asks for.
    private func checkPaper(_ words: [String]) {
        XCTAssertTrue(app.staticTexts["Check your paper"].waitForExistence(timeout: 10))
        for i in 1...12 where app.textFields["check-\(i)"].exists {
            app.textFields["check-\(i)"].tap()
            app.textFields["check-\(i)"].typeText(words[i - 1])
        }
        app.buttons["Confirm"].tap()
    }

    private func words() -> String {
        (1...12).map { app.staticTexts["word-\($0)"].label }.joined(separator: " ")
    }

    private func waitForHome(_ name: String, timeout: TimeInterval = 60) {
        XCTAssertTrue(app.navigationBars["Hi, \(name)"].waitForExistence(timeout: timeout), "home did not open")
        // Opened once the pockets are listed.
        XCTAssertTrue(app.staticTexts["Cashu"].waitForExistence(timeout: timeout), "the wallet did not open")
    }

    private func books() throws {
        menu("Check the books")
        let result = alert(timeout: 30)
        XCTAssertTrue(result.contains("matches its balance exactly"), result)
    }

    private func menu(_ item: String) {
        app.navigationBars.buttons["More"].tap()
        app.buttons[item].tap()
    }

    private func alert(timeout: TimeInterval) -> String {
        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: timeout), "no result shown")
        let text = alert.label
        alert.buttons.firstMatch.tap()
        return text
    }

    /// What a person does when a button is under the keyboard: close it, scroll.
    private func reveal(_ element: XCUIElement) {
        for _ in 0..<4 where !(element.exists && element.isHittable) { app.collectionViews.firstMatch.swipeUp() }
    }

    private func tapSubmit() {
        let submit = app.buttons["submit"]
        XCTAssertTrue(submit.waitForEnabled(timeout: 20), "the send button stayed disabled")
        submit.tap()
    }

    private func field(_ placeholder: String) -> XCUIElement {
        let f = app.textFields[placeholder]
        return f.exists ? f : app.textViews[placeholder]
    }

    /// The Send screen's outcome, or its error.
    private func result(timeout: TimeInterval = 90) -> XCUIElement {
        let r = app.descendants(matching: .any)["result"].firstMatch
        if !r.waitForExistence(timeout: timeout) { XCTFail("no result: \(app.staticTexts.allElementsBoundByIndex.map(\.label).suffix(6))") }
        return r
    }

    private func text(beginningWith prefix: String) -> XCUIElement {
        app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", prefix)).firstMatch
    }

    private func balance() -> Double {
        Double(app.staticTexts["balance"].label.filter { $0.isNumber || $0 == "." }) ?? 0
    }

    /// Pulls to refresh until `ok` holds.
    private func waitForBalance(timeout: TimeInterval = 60, _ ok: (Double) -> Bool) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            if ok(balance()) { return true }
            app.staticTexts["balance"].swipeDown()
            sleep(3)
        }
        return ok(balance())
    }

    private var dir: URL { URL(fileURLWithPath: ProcessInfo.processInfo.environment["SHARE_DIR"] ?? NSTemporaryDirectory()) }
    private func share(_ name: String, _ value: String) throws { try value.write(to: dir.appendingPathComponent(name), atomically: true, encoding: .utf8) }
    private func shared(_ name: String) throws -> String { try String(contentsOf: dir.appendingPathComponent(name), encoding: .utf8) }
}

extension XCUIElement {
    func waitForEnabled(timeout: TimeInterval) -> Bool {
        let done = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true AND enabled == true"), object: self)
        return XCTWaiter().wait(for: [done], timeout: timeout) == .completed
    }
}
