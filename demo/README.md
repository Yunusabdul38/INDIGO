# INDIGO demos

Both run on the local regtest stack: real Lightning nodes, a Cashu mint, a
Fedimint federation and an Ark (Bark) server.

    demo/start.sh          # everything below in one go, ending with "Ready."
    demo/start.sh --fresh  # the same, both phones starting empty

The pitch, step by step with what to say: [PITCH.md](PITCH.md). By hand:

    docker compose -f infra/docker-compose.regtest.yml up -d --wait
    infra/regtest-setup.sh

## Web

    open http://127.0.0.1:8090/wallet.html   # demo/start.sh serves it

The Ark pocket running in the browser (WebAssembly, its data in IndexedDB)
on the same regtest stack: create or restore a wallet, receive over
Lightning ("Pay it from outside" uses the test faucet), show its Ark address,
pay an invoice or an Ark address. It pays the phones and the phones pay it.
tests/web-regtest.sh checks it in headless Chrome. Only Ark runs in the
browser so far: Cashu and Fedimint come with browser storage for the journal.

## Terminal

    cargo build -p indigo
    demo/indigo-demo.sh              # Enter between steps
    DEMO_AUTO=1 demo/indigo-demo.sh  # straight through

## iOS: two people, two phones

    demo/ios/build-rust.sh           # Rust → IndigoFFI.xcframework + Swift bindings + Xcode project
    demo/outside-world.py            # keep running: the test network's faucet, shops and miners
    open demo/ios/IndigoDemo.xcodeproj   # run it on two Simulators, e.g. iPhone 16 and iPhone 16 Pro

Each phone is one person with one wallet: type a name (Alice on one, Bob on
the other). Then do anything:

- **Receive**: a Lightning invoice (into the pocket INDIGO picks, or one you
  pick), your Ark address, an on-chain address, or redeem ecash.
- **Send**: paste an invoice or Ark address, or create an ecash note.
  "Pay from: Automatic" lets INDIGO pick the pocket; if no pocket covers it
  alone, INDIGO first moves money from your other pockets into one, and the
  result shows each move. Or pick a pocket that has money.
- **⋯ menu**: get 50,000 test sats, check the books, show the recovery
  phrase (Face ID or passcode first), delete the wallet.

**Backup and restore.** A new wallet shows its 12 recovery words and asks
for three of them back, to be sure the paper is right. "Back up later" is
allowed: the home screen keeps reminding you, more strongly once money
arrives, and asks once when it does. The words live only in the phone's
Keychain (never in backups or iCloud), and INDIGO sends them nowhere. To try
a lost phone:
delete the wallet, then "I have a recovery phrase" and type the words: Cashu
asks the mint, Fedimint rescans the federation and Ark asks its server, and
every pocket comes back (tests/restore.sh checks this on regtest).

Copy on one phone, paste on the other: the Simulators share the Mac's
clipboard. Each code also shows as a QR code with a Share button.
Needs Xcode, xcodegen (`brew install xcodegen`) and the Rust target
`aarch64-apple-ios-sim`.

`tests/ios-ui.sh` taps through all of this by itself on two Simulators of its
own (Alice and Bob): create, back up or not, fund, pay, ecash, Ark with a
top-up, delete and restore.
