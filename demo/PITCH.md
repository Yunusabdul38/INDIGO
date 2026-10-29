# INDIGO pitch: five minutes, two phones

Before you go on: `demo/start.sh --fresh` (about two minutes from a warm Mac,
longer if Docker has to start). It ends with "Ready." and both phones on the
welcome screen. Left phone is Alice (iPhone 16), right phone is Bob (iPhone 16 Pro).

## 1. The problem (30 s, no phones)

> Bitcoin isn't one network any more. Ecash mints, Fedimint federations, Ark,
> Lightning, on-chain: each has its own wallet, its own backup, its own way to
> pay. Users shouldn't have to know which one they're in. Developers shouldn't
> have to integrate five SDKs.
>
> INDIGO is one SDK, for iOS, Android and the web, that puts all of them in one
> wallet, with one backup. And no central switch: the developer runs it.

## 2. One wallet, one backup (60 s, Alice)

- Type **Alice**, tap **Create a new wallet**.
  > One set of 12 words for every pocket. They stay in this phone's
  > Keychain. INDIGO has no server copy, no account.
- Tap **I've written them down**, type back the three words it asks for, **Confirm**.
  > We check the paper's right before any money goes in.
- On Bob's phone: type **Bob**, **Create a new wallet**, then **Back up later**.
  > Bob skips it. Watch what happens when he gets money.

## 3. Get paid, spend, without picking a pocket (60 s, Alice)

- **⋯ → Get 50,000 test sats.** Shows "→ Cashu".
  > Small amounts land in ecash: instant and private. Large ones go to Ark,
  > which is self-custody.
- **Send → Test bill (2,500 sats) → Send.** "Paid … from Cashu".
  > The wallet picked the cheapest pocket that covers it.

## 4. Person to person (60 s, both phones)

- Alice: **Send → Ecash note**, 1,500, **Create ecash note**, then **Copy**.
- Bob: **Receive → Ecash**, tap the field, paste (the Simulators share the
  clipboard), **Redeem**, **Done**.
  > Bob now has money and no backup, so INDIGO asks him once to back up, and
  > the home screen keeps saying his money is at risk until he does.
  Tap **Later**.
- Bob: **Receive → Ark**, **Copy** his Ark address.
- Alice: **Send**, paste, 2,000, **Send**.
  > Alice's Ark pocket was empty. INDIGO moved money from Cashu into Ark first,
  > then paid. The result lists the move. Nobody chose a pocket.

## 5. Trust (60 s, Alice)

- **⋯ → Check the books.** "Every pocket's journal matches its balance exactly."
  > Every move between pockets is journaled and crash-safe, and we can prove it to the sat.
- **⋯ → Delete this wallet.** Show the explanation; type **Alice**; **Delete Wallet**.
  > Lost phone.
- Type **Alice**, **I have a recovery phrase**, type the 12 words, **Restore my wallet**.
  > The mint, the federation and the Ark server each give back what these
  > words own. Same balance, and the books still match.

## Optional: Bob on the web (60 s)

- Open http://127.0.0.1:8090/wallet.html, type **Bob (web)**, **Create a new wallet**.
- **Receive → Pay it from outside** with 10,000. "Received 10,000 sats".
  > Same Rust core, compiled to WebAssembly, running in a browser tab.
- **Receive → Ark → Copy**. On Alice's phone: **Send**, paste, 1,000, **Send**.
  > The phone pays the browser over Ark. Same protocols, any platform.

## 6. Close (30 s)

> One SDK, every Bitcoin layer, one backup, no custody by us. iOS and the
> terminal today, Ark already in the browser on the same Rust core, the rest
> of the web next. Then fiat on-ramps, over Lightning, through whichever
> provider the developer picks.

## If something goes wrong

- A spinner that won't stop, or "could not reach": pull down to refresh. If
  it's still stuck, run `demo/start.sh` again: it only restarts what's down.
- Phones won't cooperate: switch to the terminal, run
  `DEMO_AUTO=1 demo/indigo-demo.sh`. It shows the same story (pockets, moves,
  boarding on-chain, ecash, Ark, books) in about two minutes.
- Before the pitch, `tests/ios-ui.sh` rehearses the whole phone flow on two
  spare Simulators (about four minutes).
