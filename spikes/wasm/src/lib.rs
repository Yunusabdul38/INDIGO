//! Browser spike: one wasm module with CDK, Fedimint (fedimint-sdk), Bark and
//! BDK. `run()` is callable from JavaScript. Throwaway, like `spike`.
//!
//! Arkade is behind the non-default `arkade` feature because its Rust client
//! does not build for browsers (test 0.9): building with `--features arkade`
//! reproduces that failure.
//!
//! Build (Apple's `ar` cannot package ring's WebAssembly C code; Homebrew LLVM can):
//!   RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
//!   CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang \
//!   AR_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/llvm-ar \
//!   cargo build --target wasm32-unknown-unknown --release
//!   wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/spike_wasm.wasm

#[allow(unused_imports)]
use std::str::FromStr;

#[allow(unused_imports)]
use indigo_testnets::mutinynet::{ARKADE as ARKADE_SERVER, CASHU_MINT, FAUCET_FEDERATION_INVITE as FAUCET_FEDERATION};
#[allow(unused_imports)]
use indigo_testnets::test_descriptors::{BIP84_CHANGE as CHANGE_DESC, BIP84_RECEIVE as DESC};
use wasm_bindgen::prelude::*;

#[allow(dead_code)]
fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Returns one line per SDK, or throws with the first failure.
#[wasm_bindgen]
pub async fn run(online: bool, mint: Option<String>) -> Result<String, JsValue> {
    #[allow(unused_mut)]
    let mut out: Vec<String> = Vec::new();
    let _ = online;

    #[cfg(feature = "cdk")]
    {
        // Cashu (CDK): HTTP client against the Mutinynet mint.
        let mint_url = cdk::mint_url::MintUrl::from_str(mint.as_deref().unwrap_or(CASHU_MINT)).map_err(err)?;
        if online {
            let client = cdk::wallet::HttpClient::new(mint_url.clone(), None);
            let info = cdk::wallet::MintConnector::get_mint_info(&client)
                .await
                .map_err(err)?;
            out.push(format!("cdk: mint {:?}", info.version));
        } else {
            out.push(format!("cdk: mint url {mint_url}"));
        }
    }
    #[cfg(feature = "fedimint")]
    {
        // Fedimint (fedimint-sdk): parse the faucet invite; the Sdk type itself is linked.
        let invite: fedimint_sdk::InviteCode = FAUCET_FEDERATION.parse().map_err(err)?;
        let _ = std::any::type_name::<fedimint_sdk::Sdk>();
        out.push(format!("fedimint-sdk: federation {}", invite.federation_id()));
    }
    #[cfg(feature = "bark")]
    {
        // Ark (Bark): seed, plus the IndexedDB store type it would use.
        let seed = bark::WalletSeed::new_from_seed(bark::ark::bitcoin::Network::Signet, &[7u8; 64]);
        let _ = std::any::type_name::<bark::persist::adaptor::indexed_db::IndexedDbClient>();
        out.push(format!("bark: seed {}", seed.fingerprint()));
    }
    #[cfg(feature = "arkade")]
    {
        // Ark (Arkade): gRPC-Web client to the Mutinynet operator.
        let mut arkade = ark_grpc::Client::new(ARKADE_SERVER.to_string());
        if online {
            arkade.connect().await.map_err(err)?;
            let info = arkade.get_info().await.map_err(err)?;
            out.push(format!("arkade: network {:?}", info.network));
        } else {
            out.push(format!("arkade: client for {ARKADE_SERVER}"));
        }
        let _ = std::any::type_name::<ark_client::OfflineClientConfig>();
        let _ = ark_core::ArkAddress::decode("x").is_err();
    }
    #[cfg(feature = "bdk")]
    {
        // On-chain (BDK).
        let mut onchain = bdk_wallet::Wallet::create(DESC, CHANGE_DESC)
            .network(bdk_wallet::bitcoin::Network::Signet)
            .create_wallet_no_persist()
            .map_err(err)?;
        let addr = onchain.reveal_next_address(bdk_wallet::KeychainKind::External);
        out.push(format!("bdk: address {}", addr.address));
    }

    Ok(out.join("\n"))
}
