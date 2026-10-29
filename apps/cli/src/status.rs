//! `indigo status`: is every service the network needs reachable?

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::network::{Chain, Endpoints};

/// Each check, including every request inside it, finishes within this.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Check {
    pub service: &'static str,
    pub result: anyhow::Result<String>,
    pub elapsed: Duration,
}

pub async fn run(endpoints: &Endpoints) -> anyhow::Result<Vec<Check>> {
    let http = reqwest::Client::builder().timeout(CHECK_TIMEOUT).build()?;
    // Regtest LND serves a self-signed certificate. Only its check skips
    // certificate verification; every other service keeps it.
    let lnd_http = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .build()?;

    // Independent checks run concurrently, so a run with several services down
    // takes one timeout, not one per service.
    let (chain, index, mint, guardian, gateway, bark, arkade, lightning, faucet) = tokio::join!(
        timed("chain", chain_height(&http, &endpoints.chain)),
        async {
            match endpoints.esplora_index {
                Some(url) => Some(timed("esplora index", esplora_index(&http, url, &endpoints.chain)).await),
                None => None,
            }
        },
        optional("cashu mint", endpoints.cashu_mint, |url| cashu_mint(&http, url)),
        optional("fedimint guardian", endpoints.fedimint_guardian, fedimint_guardian),
        optional("fedimint gateway", endpoints.fedimint_gateway, |url| fedimint_gateway(&http, url)),
        optional("bark server", endpoints.bark.map(|(server, _)| server), |url| bark_server(&http, url)),
        async {
            match endpoints.arkade {
                Some(url) => Some(timed("arkade", arkade(&http, url)).await),
                None => None,
            }
        },
        async {
            match endpoints.lightning {
                Some(url) => Some(timed("lightning (lnd)", lnd(&lnd_http, url)).await),
                None => None,
            }
        },
        async {
            match endpoints.faucet {
                Some(url) => Some(timed("faucet website", faucet(&http, url)).await),
                None => None,
            }
        },
    );
    let mut checks = vec![chain];
    checks.extend(index);
    checks.extend([mint, guardian, gateway, bark].into_iter().flatten());
    checks.extend([arkade, lightning, faucet].into_iter().flatten());
    Ok(checks)
}

/// Runs a check only on networks that have the service.
async fn optional<F: Future<Output = anyhow::Result<String>>>(
    service: &'static str,
    url: Option<&'static str>,
    check: impl FnOnce(&'static str) -> F,
) -> Option<Check> {
    match url {
        Some(url) => Some(timed(service, check(url)).await),
        None => None,
    }
}

/// The Bark server speaks gRPC over HTTP/2: any HTTP answer means it is up.
async fn bark_server(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let status = http.get(url).send().await?.status();
    Ok(format!("{url} answering (HTTP {})", status.as_u16()))
}

/// Any HTTP answer means the gateway is up; its API needs a federation id or
/// a password for anything more.
async fn fedimint_gateway(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let status = http.get(url).send().await?.status();
    Ok(format!("answering (HTTP {})", status.as_u16()))
}

async fn timed(
    service: &'static str,
    fut: impl Future<Output = anyhow::Result<String>>,
) -> Check {
    let start = Instant::now();
    let result = fut.await;
    Check { service, result, elapsed: start.elapsed() }
}

async fn chain_height(http: &reqwest::Client, chain: &Chain) -> anyhow::Result<String> {
    match chain {
        Chain::Esplora(base) => {
            let height = http
                .get(format!("{base}/blocks/tip/height"))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            Ok(format!("block height {}", height.trim()))
        }
        Chain::BitcoindRpc { .. } => {
            let info = bitcoind_info(http, chain).await?;
            Ok(format!("block height {} ({})", info["blocks"], info["chain"].as_str().unwrap_or("?")))
        }
    }
}

async fn bitcoind_info(http: &reqwest::Client, chain: &Chain) -> anyhow::Result<Value> {
    let Chain::BitcoindRpc { url, user, pass } = chain else {
        bail!("not a bitcoind chain");
    };
    let body: Value = http
        .post(*url)
        .basic_auth(user, Some(pass))
        .json(&json!({"jsonrpc": "1.0", "id": "indigo", "method": "getblockchaininfo", "params": []}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(body["result"].clone())
}

/// Esplora must be up and caught up with bitcoind: wallets that read a stale
/// index show wrong balances.
async fn esplora_index(http: &reqwest::Client, url: &str, chain: &Chain) -> anyhow::Result<String> {
    let indexed: u64 = http
        .get(format!("{url}/blocks/tip/height"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?
        .trim()
        .parse()?;
    let tip = bitcoind_info(http, chain).await?["blocks"].as_u64().context("bitcoind block count")?;
    // One block of lag is normal right after a block is mined.
    if indexed + 1 < tip {
        bail!("{url} is at block {indexed}, bitcoind at {tip}: still indexing");
    }
    Ok(format!("{url} (block {indexed})"))
}

async fn cashu_mint(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let info: Value = http.get(format!("{url}/v1/info")).send().await?.error_for_status()?.json().await?;
    let version = info["version"].as_str().unwrap_or("unknown version");
    Ok(format!("{url} ({version})"))
}

/// Guardians speak JSON-RPC over WebSocket. An unconfigured guardian (before
/// DKG) also accepts the connection, so ask for the consensus session count,
/// which only a running federation answers.
async fn fedimint_guardian(url: &str) -> anyhow::Result<String> {
    tokio::time::timeout(CHECK_TIMEOUT, session_count(url))
        .await
        .with_context(|| format!("{url}: no answer within {} s", CHECK_TIMEOUT.as_secs()))?
}

async fn session_count(url: &str) -> anyhow::Result<String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("cannot reach {url}"))?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "session_count",
        "params": [{"auth": null, "params": null}],
    });
    ws.send(Message::text(request.to_string())).await?;
    let mut reply = None;
    while let Some(msg) = ws.next().await {
        if let Message::Text(text) = msg? {
            reply = Some(serde_json::from_str::<Value>(&text)?);
            break;
        }
    }
    let reply = reply.context("connection closed without a reply")?;
    match reply.get("result") {
        Some(sessions) => Ok(format!("{url} (consensus session {sessions})")),
        None => bail!("{url} is up but not serving a federation: {}", reply["error"]),
    }
}

async fn arkade(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let info: Value = http.get(format!("{url}/v1/info")).send().await?.error_for_status()?.json().await?;
    Ok(format!("{url} (network {})", info["network"].as_str().unwrap_or("?")))
}

async fn lnd(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let state: Value = http.get(format!("{url}/v1/state")).send().await?.error_for_status()?.json().await?;
    let state = state["state"].as_str().unwrap_or("?").to_string();
    if state != "SERVER_ACTIVE" {
        bail!("lnd state {state}");
    }
    Ok(state)
}

/// The faucet website only. Its federation is checked separately above: the
/// site can be up while the federation it advertises is down.
async fn faucet(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    http.get(url).send().await?.error_for_status()?;
    Ok(url.to_string())
}
