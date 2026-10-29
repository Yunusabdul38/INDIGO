//! INDIGO's Bark pocket in a browser, its data in IndexedDB. `run()` does one
//! action, like one CLI command. wallet.html is the web demo; index.html runs
//! one action from its URL, for `check.mjs` (tests/web-regtest.sh) to drive
//! from headless Chrome, whose profile keeps IndexedDB between loads.
//!
//! Build (Apple's `ar` can't package the WebAssembly C code; Homebrew LLVM can):
//!   RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
//!   CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang \
//!   AR_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/llvm-ar \
//!   cargo build --target wasm32-unknown-unknown --release
//!   wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/web_ark_check.wasm

use std::path::Path;

use indigo_domain::{Bolt11, Msat, Network, OpId, OpStatus};
use indigo_rail::{ArkOps, Rail};
use indigo_rail_ark_bark::{BarkChain, BarkConfig, BarkRail};
use indigo_testnets::{regtest, signet};
use wasm_bindgen::prelude::*;

fn js(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// One action on the wallet of `words`, on `network` (signet or regtest): balance | address | onchain |
/// board <sats> | board-all | receive <sats> | receive-wait <sats> |
/// status <op> | pay <invoice> | pay-address <address> <sats> | recover.
/// Like the CLI, every action recovers and syncs first.
///
/// `receive-wait` shows the invoice through `show` at once, then stays to
/// claim the payment (a Bark server cancels one its wallet is not there to
/// take) for up to 3 minutes.
#[wasm_bindgen]
pub async fn run(
    network: String,
    words: String,
    action: String,
    arg: String,
    arg2: String,
    show: js_sys::Function,
) -> Result<String, JsValue> {
    console_error_panic_hook::set_once();
    let seed = bip39::Mnemonic::parse_normalized(words.trim()).map_err(js)?.to_seed("");
    let (config, network) = match network.as_str() {
        "regtest" => (
            BarkConfig { server: regtest::BARK_SERVER.into(), chain: BarkChain::Esplora(regtest::ESPLORA.into()) },
            Network::Regtest,
        ),
        _ => (
            BarkConfig { server: signet::BARK_SERVER.into(), chain: BarkChain::Esplora(signet::ESPLORA.into()) },
            Network::Signet,
        ),
    };
    // In a browser the data lives in IndexedDB; the folder is not used.
    let rail = BarkRail::open(&config, network, seed, Path::new("")).await.map_err(js)?;
    let recovered = rail.recover().await.map_err(js)?;
    let sats = |s: &str| s.parse::<u64>().map(Msat::from_sats).map_err(js);
    let none = |_: &OpId, _: Msat| Ok(());
    let out = match action.as_str() {
        "balance" => {
            let b = rail.balance().await.map_err(js)?;
            format!("{} spendable, {} pending", b.spendable, b.pending)
        }
        "address" => rail.native_address().await.map_err(js)?,
        "onchain" => format!("{} ({})", rail.onchain_address().await.map_err(js)?, rail.onchain_balance().await.map_err(js)?),
        "board" => rail.board(Some(sats(&arg)?)).await.map_err(js)?,
        "board-all" => rail.board(None).await.map_err(js)?,
        "receive" => {
            let r = rail.ln_receive(sats(&arg)?, "indigo browser").await.map_err(js)?;
            format!("{} {}", r.op, r.invoice.unwrap_or_default())
        }
        "receive-wait" => {
            let r = rail.ln_receive(sats(&arg)?, "indigo browser").await.map_err(js)?;
            let invoice = r.invoice.unwrap_or_default();
            show.call1(&JsValue::NULL, &JsValue::from_str(&format!("INVOICE {invoice}")))?;
            let mut status = OpStatus::Pending;
            for _ in 0..90 {
                status = rail.op_status(&r.op).await.map_err(js)?;
                if status != OpStatus::Pending {
                    break;
                }
                sleep_ms(2000).await;
            }
            format!("{} {status:?}", r.op)
        }
        "status" => format!("{:?}", rail.op_status(&OpId(arg)).await.map_err(js)?),
        "pay" => {
            let quote = rail.ln_quote(&Bolt11::parse(&arg).map_err(js)?).await.map_err(js)?;
            let paid = rail.ln_pay(quote).await.map_err(js)?;
            format!("{} {:?}", paid.op, paid.status)
        }
        "pay-address" => {
            let paid = rail.native_pay(&arg, sats(&arg2)?, &none).await.map_err(js)?;
            format!("{} {:?}", paid.op, paid.status)
        }
        "recover" => format!("{} arrivals, problems: {:?}", recovered.arrivals.len(), recovered.problems),
        other => return Err(js(format!("unknown action {other}"))),
    };
    Ok(out)
}

/// A new 12-word recovery phrase.
#[wasm_bindgen]
pub fn new_words() -> Result<String, JsValue> {
    let mut entropy = [0u8; 16];
    getrandom03::fill(&mut entropy).map_err(js)?;
    Ok(bip39::Mnemonic::from_entropy(&entropy).map_err(js)?.to_string())
}

/// Waits in the browser (no runtime timer here: a JavaScript `setTimeout`).
async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = web_sys_window_set_timeout(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn web_sys_window_set_timeout(handler: &js_sys::Function, ms: i32) -> JsValue;
}
