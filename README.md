# INDIGO

One Bitcoin wallet SDK for iOS, Android and the web that holds every layer
in one wallet: **Cashu** ecash, **Fedimint** federations, **Ark** (Bark and
Arkade), on-chain bitcoin and **Lightning** between them.

- **One wallet, one backup.** 12 recovery words cover every pocket. Restoring
  on a new device asks the mint, rescans the federation and asks the Ark
  server; every pocket comes back.
- **Users don't pick pockets.** Payments go out from the cheapest pocket that
  covers them. If none does alone, INDIGO first moves money into one, then pays.
  Incoming money lands where it fits: small amounts in ecash, large ones in Ark.
- **Every sat accounted for.** Every operation and every move between pockets
  is journaled and recovers from a crash at any step. An audit compares each
  pocket's balance with its journal, to the sat.
- **No central switch.** No INDIGO server, no account. The developer picks
  the mints, federations and Ark servers; the words never leave the device.
- Bitcoin networks only. Fiat is optional and comes in over Lightning
  through a provider the developer chooses (planned).

## Get started

You need macOS with:

- [Rust](https://rustup.rs) 1.85 or newer
- [Docker Desktop](https://www.docker.com/products/docker-desktop/)
- Python 3

For the iOS demo, also Xcode, [xcodegen](https://github.com/yonaskolb/XcodeGen)
(`brew install xcodegen`) and `rustup target add aarch64-apple-ios-sim`.
For the web demo, see [Web](#web-the-ark-pocket-in-a-browser).

Then, from the repository root:

    demo/start.sh

On the first run this takes several minutes: it builds the Rust workspace and
starts the regtest stack. It ends with **Ready.**, after it has:

1. started Docker and the local **regtest** stack: bitcoind, Lightning nodes,
   a Cashu mint, a Fedimint federation with its Lightning gateway, and an Ark
   (Bark) server;
2. funded it and opened the Lightning channels (`infra/regtest-setup.sh`);
3. started the **outside world** on 127.0.0.1:8733 (`demo/outside-world.py`):
   the faucet, shops that send bills, and a miner;
4. served the web wallet, if built, on http://127.0.0.1:8090/wallet.html;
5. built the CLI (`target/debug/indigo`);
6. built the iOS app and opened it on two Simulators: Alice on "iPhone 16",
   Bob on "iPhone 16 Pro".

`demo/start.sh --fresh` does the same with both phones starting empty. It is
safe to run again: whatever is already up is left as it is. Logs go to
`demo/.run/`.

The five-minute pitch, with what to tap and what to say:
**[demo/PITCH.md](demo/PITCH.md)**.

Everything here is test money on a private regtest chain.

## The demos

All three run on the same local regtest stack, so the phones, the terminal
and the browser can pay each other.

### iOS: two people, two phones

Each phone is one person with one wallet. Type a name, then:

- **Receive**: a Lightning invoice (into the pocket INDIGO picks, or one you
  pick), your Ark address, an on-chain address, or redeem ecash.
- **Send**: paste an invoice or Ark address, or create an ecash note.
  "Pay from: Automatic" lets INDIGO pick; the result lists any money it moved
  first. Or pick a pocket that has money.
- **⋯ menu**: get 50,000 test sats, check the books, show the recovery phrase
  (Face ID or passcode first), delete the wallet.

**Backup.** A new wallet shows its 12 words and asks for three of them back.
"Back up later" is allowed: the home screen keeps reminding you, more strongly
once money arrives, and asks once when it does. The words live only in the
phone's Keychain (not in backups or iCloud).

**Lost phone.** ⋯ → Delete this wallet (type the name to confirm), then
"I have a recovery phrase" and the 12 words. Same balance, books still match.

Copy on one Simulator, paste on the other: they share the Mac's clipboard.
Every code also shows as a QR code with a Share button.

To build by hand instead of `demo/start.sh`:

    demo/ios/build-rust.sh               # Rust → IndigoFFI.xcframework, Swift bindings, Xcode project
    python3 demo/outside-world.py        # keep it running
    open demo/ios/IndigoDemo.xcodeproj

### Terminal

    demo/indigo-demo.sh              # press Enter between steps
    DEMO_AUTO=1 demo/indigo-demo.sh  # straight through, about two minutes

Alice gets paid over Lightning into Cashu, moves money to Fedimint and into
Ark, brings on-chain coins into Ark, pays a bill from Ark, hands Bob a Cashu
token and Fedimint notes, pays Bob over Ark, and ends with the audit.

The CLI behind it does everything one command at a time: `target/debug/indigo
--network regtest --help`. It needs `INDIGO_TEST_MNEMONIC` (12 words; get some
with `indigo new-test-seed`) and `--data-dir` for the wallet folder.

### Web: the Ark pocket in a browser

    open http://127.0.0.1:8090/wallet.html

The Ark pocket compiled to WebAssembly, with its data in IndexedDB. Create or
restore a wallet, receive over Lightning ("Pay it from outside" uses the
faucet), show its Ark address, pay an invoice or an Ark address. It pays the
phones and the phones pay it. Only Ark runs in the browser so far; Cashu and
Fedimint need browser storage for the journal first.

Building it needs Homebrew LLVM (`brew install llvm`), `wasm-bindgen-cli` and
`rustup target add wasm32-unknown-unknown`; the commands are at the top of
[apps/web-ark-check/src/lib.rs](apps/web-ark-check/src/lib.rs).

## Tests

With the regtest stack up (`demo/start.sh`, or the two infra commands below):

| Script | What it proves |
|---|---|
| `tests/phase1.sh regtest` | Cashu pocket: receive, pay, ecash, kill tests |
| `tests/phase2.sh regtest` | Fedimint pocket and moves between pockets, killed at every step |
| `tests/phase3.sh regtest` | Ark pocket: boarding, Ark payments, Lightning in and out |
| `tests/phase4-route.sh regtest` | Automatic pocket choice, top-ups, refusals |
| `tests/restore.sh regtest` | Wipe a funded wallet, restore it from 12 words |
| `tests/web-regtest.sh` | The browser wallet in headless Chrome (needs Node and Chrome) |
| `tests/ios-ui.sh` | Taps through the iOS app on two Simulators of its own |

`cargo test --workspace` runs the unit tests. CI (`.github/workflows/ci.yml`)
runs the unit tests, the regtest scripts above, and the iOS and Android builds.

Stack by hand:

    docker compose -f infra/docker-compose.regtest.yml up -d --wait
    infra/regtest-setup.sh

## Other networks

The CLI also runs on **Mutinynet** (`--network mutinynet`, the default), a
public signet with 30-second blocks, against our own mint and federation
(`infra/mutinynet-setup.sh` and `infra/docker-compose.mutinynet.yml`) and the
public Arkade server. It also runs on **Second's public signet**
(`--network signet`), where only the Ark (Bark) server runs.
`tests/phase1.sh mutinynet` and `tests/phase2.sh mutinynet` cover Mutinynet.

## Layout

| Path | What it is |
|---|---|
| `crates/domain` | Amounts, operations, moves, errors: the shared vocabulary |
| `crates/rail` | The `Rail` trait every pocket implements |
| `crates/rail-cashu` | Cashu pocket (CDK) |
| `crates/rail-fedimint` | Fedimint pocket (fedimint-sdk) |
| `crates/rail-ark-bark` | Ark pocket on Bark; also builds for the browser |
| `crates/rail-ark-arkade` | Ark pocket on Arkade |
| `crates/keys` | One 12-word seed, a separate key per pocket |
| `crates/store` | SQLite journal of every operation and move |
| `crates/engine` | Crash-safe moves between pockets |
| `crates/router` | Picks the paying pocket, plans top-ups |
| `crates/core` | The `Wallet`: send, receive, move, restore, audit |
| `crates/ffi` | Swift and Kotlin bindings (UniFFI) |
| `crates/testnets` | Endpoints of every test network |
| `apps/cli` | The `indigo` CLI |
| `apps/web-ark-check` | The browser build and the web wallet page |
| `demo/` | `start.sh`, the pitch, the terminal demo, the iOS app, the outside world |
| `infra/` | Docker stacks for regtest and Mutinynet, and their setup |
| `tests/` | End-to-end scripts |
