//! INDIGO for Swift and Kotlin, through UniFFI.
//!
//! For now this is the demo surface: one wallet on the local regtest stack
//! (infra/docker-compose.regtest.yml), with the calls the pitch demo needs.
//! Phase 8 turns it into the full SDK surface with the network chosen by the
//! app.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use indigo_core::{BarkChain, BarkConfig, Destination, SendReport, Wallet, WalletConfig};
use indigo_domain::{Bolt11, Msat, Network, OpId, OpKind, OpStatus};
use indigo_keys::Seed;
use indigo_testnets::regtest;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum IndigoError {
    #[error("{message}")]
    Failed { message: String },
}

impl From<indigo_domain::Error> for IndigoError {
    fn from(e: indigo_domain::Error) -> Self {
        IndigoError::Failed { message: e.to_string() }
    }
}

type Result<T> = std::result::Result<T, IndigoError>;

/// Every call runs on INDIGO's own multi-threaded runtime: Fedimint blocks in
/// place, which a current-thread runtime (UniFFI's tokio bridge) forbids.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
});

async fn on_runtime<T: Send + 'static>(work: impl Future<Output = T> + Send + 'static) -> T {
    RUNTIME.spawn(work).await.expect("INDIGO call panicked")
}

/// One pocket's balance, in sats (with msat decimals).
#[derive(uniffi::Record)]
pub struct PocketBalance {
    /// cashu, fedimint or ark-bark
    pub pocket: String,
    pub sats: f64,
}

/// A Lightning invoice to show the payer, the operation to wait on, and the
/// pocket it goes into.
#[derive(uniffi::Record)]
pub struct Invoice {
    pub op: String,
    pub invoice: String,
    pub pocket: String,
}

/// What a send did, step by step.
#[derive(uniffi::Record)]
pub struct SendResult {
    /// "Paid 2000 sats (fee 3 sats) from cashu"
    pub summary: String,
    /// Top-ups made first: "Moved 2240 sats cashu → ark-bark".
    pub steps: Vec<String>,
    /// For ecash: the token to hand over.
    pub token: Option<String>,
}

/// One line of the wallet's history.
#[derive(uniffi::Record)]
pub struct Activity {
    pub title: String,
    pub pocket: String,
    pub sats: f64,
    pub incoming: bool,
    /// COMPLETED, PENDING, FAILED, EXPIRED (or a move's state).
    pub status: String,
    /// Unix seconds.
    pub at: u64,
}

/// What a pasted text is, for the send screen.
#[derive(uniffi::Record)]
pub struct Detected {
    /// "Lightning invoice", "Ark address", "Bitcoin address", "Cashu token", "Fedimint notes"
    pub kind: String,
    /// The amount the text itself carries (an invoice's).
    pub sats: Option<f64>,
    /// True for ecash: it is redeemed, not paid.
    pub redeem: bool,
}

/// One pocket's balance against its journal.
#[derive(uniffi::Record)]
pub struct AuditLine {
    pub pocket: String,
    pub balance_sats: f64,
    pub journal_sats: f64,
    pub matches: bool,
}

#[derive(uniffi::Object)]
pub struct IndigoWallet {
    inner: Arc<Wallet>,
}

fn pocket(id: &str) -> String {
    id.split(':').next().unwrap_or(id).to_string()
}

fn sats(msat: u64) -> f64 {
    msat as f64 / 1000.0
}

#[uniffi::export]
impl IndigoWallet {
    /// Opens (or creates) the wallet in `data_dir` from its 12 words, on the
    /// local regtest stack, and recovers anything a crash interrupted.
    #[uniffi::constructor]
    pub async fn open(data_dir: String, words: String) -> Result<Arc<Self>> {
        on_runtime(async move {
            let seed = Seed::from_mnemonic(&words)?;
            let config = WalletConfig {
                network: Network::Regtest,
                data_dir: PathBuf::from(data_dir),
                cashu_mint: Some(regtest::CASHU_MINT.into()),
                bark: Some(BarkConfig {
                    server: regtest::BARK_SERVER.into(),
                    chain: BarkChain::Bitcoind {
                        url: regtest::BARK_BITCOIND_RPC.into(),
                        user: "indigo".into(),
                        pass: "indigo".into(),
                    },
                }),
                arkade: None,
            };
            let inner = Wallet::open(&config, &seed).await?;
            inner.recover().await?;
            Ok(Arc::new(Self { inner: Arc::new(inner) }))
        })
        .await
    }

    /// On a new device: brings a wallet back from its 12 words into an
    /// empty `data_dir`, rejoining the federation behind `invite` by
    /// rescanning it. Every pocket comes back with what its seed owns.
    #[uniffi::constructor]
    pub async fn restore(data_dir: String, words: String, invite: String) -> Result<Arc<Self>> {
        let wallet = Self::open(data_dir, words).await?;
        let w = wallet.inner.clone();
        on_runtime(async move {
            w.restore_federation(&invite, Duration::from_secs(300)).await?;
            w.restore().await?;
            Ok::<_, IndigoError>(())
        })
        .await?;
        Ok(wallet)
    }

    /// Joins the federation behind `invite` (no-op if already joined).
    pub async fn join_federation(&self, invite: String) -> Result<()> {
        let w = self.inner.clone();
        on_runtime(async move {
            if w.rail("fedimint").is_err() {
                w.join_federation(&invite).await?;
            }
            Ok(())
        })
        .await
    }

    pub async fn balances(&self) -> Vec<PocketBalance> {
        let w = self.inner.clone();
        on_runtime(async move {
            let _ = w.recover().await;
            w.balances()
                .await
                .into_iter()
                .flatten()
                .map(|b| PocketBalance { pocket: pocket(&b.rail.0), sats: sats(b.spendable.0 + b.pending.0) })
                .collect()
        })
        .await
    }

    /// A Lightning invoice for `sats`, into `into` or the pocket INDIGO picks.
    pub async fn request(&self, sats: u64, into: Option<String>) -> Result<Invoice> {
        let w = self.inner.clone();
        on_runtime(async move {
            let op = w.receive_auto(Msat::from_sats(sats), into.as_deref(), "INDIGO").await?;
            Ok(Invoice { pocket: pocket(&op.op.0), op: op.op.0, invoice: op.invoice.unwrap_or_default() })
        })
        .await
    }

    /// Waits up to `seconds` for a receive to be paid and claimed; returns
    /// COMPLETED, PENDING, FAILED or EXPIRED.
    pub async fn wait_received(&self, op: String, seconds: u64) -> Result<String> {
        let w = self.inner.clone();
        on_runtime(async move {
            let op = OpId(op);
            let mut status = OpStatus::Pending;
            for _ in 0..seconds.max(1) {
                status = w.refresh(&op).await?;
                if status.is_final() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Ok(status.label().to_string())
        })
        .await
    }

    /// Pays a Lightning invoice or an Ark address (`sats` for an address),
    /// from `from` or from whatever pockets INDIGO picks.
    pub async fn send(&self, to: String, sats: Option<u64>, from: Option<String>) -> Result<SendResult> {
        let w = self.inner.clone();
        on_runtime(async move { Ok(report(w.send(&to, sats.map(Msat::from_sats), from.as_deref()).await?)) }).await
    }

    /// Creates ecash (a Cashu token or Fedimint notes) to hand to someone.
    pub async fn send_ecash(&self, sats: u64, from: Option<String>) -> Result<SendResult> {
        let w = self.inner.clone();
        on_runtime(async move { Ok(report(w.send_ecash(Msat::from_sats(sats), from.as_deref()).await?)) }).await
    }

    /// Redeems a Cashu token or Fedimint notes; returns what arrived where.
    pub async fn redeem(&self, token: String) -> Result<String> {
        let w = self.inner.clone();
        on_runtime(async move {
            let op = w.redeem(&token).await?;
            Ok(format!("Received {} into {}", op.amount, pocket(&op.op.0)))
        })
        .await
    }

    /// Moves `sats` from one of the user's pockets to another; returns the
    /// move's final state.
    pub async fn move_funds(&self, from: String, to: String, sats: u64) -> Result<String> {
        let w = self.inner.clone();
        on_runtime(async move {
            let started = w.start_move(&from, &to, Msat::from_sats(sats), None).await?;
            let done = w.confirm_move(&started.id, Duration::from_secs(60)).await?;
            Ok(done.state.as_str().to_string())
        })
        .await
    }

    /// The Ark address another Ark wallet pays.
    pub async fn ark_address(&self) -> Result<String> {
        let w = self.inner.clone();
        on_runtime(async move { Ok(w.native_address("ark-bark").await?) }).await
    }

    /// The Ark pocket's own on-chain address, to fund boarding.
    pub async fn ark_onchain_address(&self) -> Result<String> {
        let w = self.inner.clone();
        on_runtime(async move { Ok(w.ark("ark-bark")?.onchain_address().await?) }).await
    }

    /// On-chain coins waiting to be boarded into Ark, in sats.
    pub async fn ark_onchain_sats(&self) -> Result<f64> {
        let w = self.inner.clone();
        on_runtime(async move { Ok(sats(w.ark("ark-bark")?.onchain_balance().await?.0)) }).await
    }

    /// Boards every on-chain coin into Ark; returns the funding txid.
    pub async fn ark_board_all(&self) -> Result<String> {
        let w = self.inner.clone();
        on_runtime(async move { Ok(w.ark("ark-bark")?.board(None).await?) }).await
    }

    /// What the wallet did, newest first: moves between pockets as one line.
    pub async fn activity(&self) -> Result<Vec<Activity>> {
        let w = self.inner.clone();
        on_runtime(async move {
            let moves = w.moves()?;
            let in_moves: std::collections::HashSet<String> =
                moves.iter().flat_map(|m| [m.source_op.clone(), m.dest_op.clone()]).flatten().map(|o| o.0).collect();
            let mut out: Vec<Activity> = w
                .ops()?
                .into_iter()
                .filter(|o| !in_moves.contains(&o.op.0))
                .map(|o| {
                    let pocket = pocket(&o.rail.0);
                    let (title, incoming) = match (o.kind, pocket.as_str()) {
                        (OpKind::LnReceive, _) => ("Received over Lightning".to_string(), true),
                        (OpKind::LnPay, _) => ("Paid a Lightning invoice".to_string(), false),
                        (OpKind::NativeSend, p) if p.starts_with("ark") => ("Paid an Ark address".to_string(), false),
                        (OpKind::NativeSend, _) => ("Sent ecash".to_string(), false),
                        (OpKind::NativeReceive, p) if p.starts_with("ark") => ("Received over Ark".to_string(), true),
                        (OpKind::NativeReceive, _) => ("Redeemed ecash".to_string(), true),
                    };
                    Activity { title, pocket, sats: sats(o.amount.0), incoming, status: o.status.label().into(), at: o.created_at }
                })
                .collect();
            out.extend(moves.into_iter().map(|m| Activity {
                title: format!("Moved {} → {}", pocket(&m.from.0), pocket(&m.to.0)),
                pocket: pocket(&m.to.0),
                sats: sats(m.amount.0),
                incoming: false,
                status: m.state.as_str().into(),
                at: m.created_at,
            }));
            out.sort_by_key(|a| std::cmp::Reverse(a.at));
            Ok(out)
        })
        .await
    }

    /// Every pocket's balance against its journal.
    pub async fn audit(&self) -> Result<Vec<AuditLine>> {
        let w = self.inner.clone();
        on_runtime(async move {
            Ok(w.audit()
                .await?
                .into_iter()
                .map(|a| AuditLine {
                    pocket: pocket(&a.rail.0),
                    balance_sats: sats(a.balance.0),
                    journal_sats: a.journal as f64 / 1000.0,
                    matches: a.matches(),
                })
                .collect())
        })
        .await
    }
}

fn report(r: SendReport) -> SendResult {
    let summary = match &r.status {
        OpStatus::Completed { amount, fee } => format!("Paid {amount} (fee {fee}) from {}", pocket(&r.from.0)),
        other => format!("{} from {}", other.label(), pocket(&r.from.0)),
    };
    let steps = r
        .top_ups
        .iter()
        .map(|m| format!("Moved {} {} → {}", m.amount, pocket(&m.from.0), pocket(&m.to.0)))
        .collect();
    SendResult { summary, steps, token: r.token.map(|t| t.token) }
}

/// Recognises a pasted text; `None` if INDIGO can't use it.
#[uniffi::export]
pub fn detect(text: String) -> Option<Detected> {
    let (kind, sats_, redeem) = match Destination::parse(&text)? {
        Destination::Lightning(i) => ("Lightning invoice", Bolt11::parse(&i).ok()?.amount().map(|a| sats(a.0)), false),
        Destination::Ark(_) => ("Ark address", None, false),
        Destination::Bitcoin(_) => ("Bitcoin address", None, false),
        Destination::CashuToken(_) => ("Cashu token", None, true),
        Destination::FedimintNotes(_) => ("Fedimint notes", None, true),
    };
    Some(Detected { kind: kind.into(), sats: sats_, redeem })
}

/// True if `words` is a valid 12-word recovery phrase.
#[uniffi::export]
pub fn words_valid(words: String) -> bool {
    Seed::from_mnemonic(&words).is_ok()
}

/// A new 12-word recovery phrase.
#[uniffi::export]
pub fn new_words() -> String {
    Seed::generate_words()
}
