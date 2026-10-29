//! The Ark pocket on Bark (Second's `bark-wallet`), one Ark server each.
//!
//! Bark logs every change to the balance as a movement, with the real change
//! and fee. Operations INDIGO starts are matched to them after a crash:
//!
//! - `<rail>:receive:<payment hash>` and `<rail>:pay:<payment hash>.<quote
//!   expiry>` for Lightning, which Bark also keys by payment hash
//! - `<rail>:send:<destination hash>.<amount msat>.<unix seconds>` for Ark
//!   payments, recorded before Bark is asked to pay; Bark's movement for it
//!   is the one to that destination, for that amount, from that time on
//! - `<rail>:movement:<id>` for everything else Bark logs (payments to our
//!   address, boards, refreshes, exits), reported by `recover` as arrivals

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use bark::ark::bitcoin::hashes::sha256;
use bark::ark::bitcoin::{Address as BtcAddress, Amount, FeeRate, Network as BtcNetwork};
use bark::ark::lightning::PaymentHash;
use bark::LightningSendOptions;
use bark::movement::{Movement, MovementStatus, PaymentMethod};
use bark::onchain::{OnchainWallet, OnchainWalletTrait};
use bark::persist::BarkPersister;
use bark::{Config, OpenWalletArgs, Wallet, WalletSeed};
use indigo_domain::{
    Arrival, BalanceSnapshot, Bolt11, Capabilities, Error, Msat, NativeToken, Network, OpId, OpKind, OpStatus,
    PayOp, PayQuote, RailId, ReceiveOp, RecoveryReport, Result, now,
};
use indigo_rail::{ArkOps, BeforeMove, ExitStatus, Expiry, Rail, Vtxo};
use sha2::Digest;
use tokio::sync::RwLock;

/// Where a Bark pocket finds its Ark server and the chain.
#[derive(Clone, Debug)]
pub struct BarkConfig {
    pub server: String,
    pub chain: BarkChain,
}

/// Bark reads the chain from Esplora or from a bitcoind with `-txindex`.
/// Its Esplora must offer `/txs/package` (package relay) for boarding.
#[derive(Clone, Debug)]
pub enum BarkChain {
    Esplora(String),
    Bitcoind { url: String, user: String, pass: String },
}

pub struct BarkRail {
    id: RailId,
    network: Network,
    wallet: Wallet,
    onchain: Arc<RwLock<OnchainWallet>>,
}

impl BarkRail {
    /// Opens (or creates) the Bark wallet. `seed` is this pocket's 64-byte
    /// secret. Natively its data lives in `dir` (SQLite); in a browser, in
    /// IndexedDB, named after the wallet (`dir` is ignored there).
    pub async fn open(config: &BarkConfig, network: Network, seed: [u8; 64], dir: &Path) -> Result<Self> {
        let id = RailId(format!("ark-bark:{}", config.server.trim_end_matches('/')));
        let btc = btc_network(network);
        let wallet_seed = WalletSeed::new_from_seed(btc, &seed);
        #[cfg(not(target_arch = "wasm32"))]
        std::fs::create_dir_all(dir).map_err(|e| Error::Storage(format!("{id}: {e}")))?;
        let db: Arc<dyn BarkPersister> = bark::persist::platform_default(Some(dir), Some(wallet_seed.fingerprint()))
            .await
            .map_err(|e| Error::Storage(format!("{id}: {e:#}")))?;
        let onchain = OnchainWallet::load_or_create(btc, seed, db.clone())
            .await
            .map_err(|e| Error::Storage(format!("{id}: on-chain wallet: {e:#}")))?;
        let onchain = Arc::new(RwLock::new(onchain));
        let mut bark_config = Config::network_default(btc);
        bark_config.server_address = config.server.clone();
        match &config.chain {
            BarkChain::Esplora(url) => bark_config.esplora_address = Some(url.clone()),
            BarkChain::Bitcoind { url, user, pass } => {
                bark_config.bitcoind_address = Some(url.clone());
                bark_config.bitcoind_user = Some(user.clone());
                bark_config.bitcoind_pass = Some(bark::secret::Secret::new(pass.clone()));
            }
        }
        bark_config.user_agent = Some(format!("indigo/{}", env!("CARGO_PKG_VERSION")));
        let args = OpenWalletArgs {
            // Each step is driven by INDIGO's own calls and recover().
            run_daemon: false,
            datadir: Some(dir.to_path_buf()),
            persister: Some(db),
            onchain: Some(onchain.clone() as Arc<RwLock<dyn OnchainWalletTrait>>),
            create_if_not_exists: true,
            create_without_server: true,
            ..Default::default()
        };
        let wallet = Wallet::open(btc, wallet_seed, bark_config, args)
            .await
            .map_err(|e| Error::OperatorUnavailable { rail: id.clone(), reason: format!("{e:#}") })?;
        Ok(Self { id, network, wallet, onchain })
    }

    fn op(&self, kind: &str, key: &str) -> OpId {
        OpId(format!("{}:{kind}:{key}", self.id))
    }

    fn parse_op<'a>(&self, op: &'a OpId) -> Result<(&'a str, &'a str)> {
        let rest = op
            .0
            .strip_prefix(&self.id.0)
            .and_then(|r| r.strip_prefix(':'))
            .ok_or_else(|| Error::UnknownOp(op.clone()))?;
        rest.split_once(':').ok_or_else(|| Error::UnknownOp(op.clone()))
    }

    /// Bark reports errors as `anyhow`: one that mentions the server or the
    /// connection is the server being unreachable, anything else its answer.
    fn err(&self, e: anyhow::Error) -> Error {
        let reason = format!("{e:#}");
        let lower = reason.to_lowercase();
        if ["connect", "unreachable", "timed out", "timeout", "transport", "unavailable"].iter().any(|w| lower.contains(w)) {
            Error::OperatorUnavailable { rail: self.id.clone(), reason }
        } else if lower.contains("insufficient") || lower.contains("not enough") {
            Error::Other(format!("{}: {reason}", self.id))
        } else {
            Error::OperatorError { rail: self.id.clone(), reason }
        }
    }

    async fn movements(&self) -> Result<Vec<Movement>> {
        self.wallet.history().await.map_err(|e| self.err(e))
    }

    /// The movement for a Lightning payment, by its payment hash.
    async fn lightning_movement(&self, hash: &str) -> Result<Option<Movement>> {
        Ok(self
            .movements()
            .await?
            .into_iter()
            .find(|m| m.lightning_payment_hash().is_some_and(|h| h.to_string() == hash)))
    }

    /// Movements INDIGO started itself: their ops already account for them.
    fn started_by_indigo(m: &Movement) -> bool {
        m.lightning_payment_hash().is_some() || (m.subsystem.name == "bark.arkoor" && m.effective_balance.is_negative())
    }

    async fn ln_pay_status(&self, key: &str) -> Result<OpStatus> {
        let (hash, expiry) = key.split_once('.').ok_or_else(|| Error::InvalidInput(format!("bad pay key {key}")))?;
        let payment_hash = payment_hash(hash)?;
        let state = self.wallet.check_lightning_payment(payment_hash, false).await;
        match (state, self.lightning_movement(hash).await?) {
            (Ok(bark::actions::lightning::pay::LightningSendState::Paid(_)), Some(m)) => Ok(completed(&m)),
            // Paid, but Bark has not logged the movement yet: ask again.
            (Ok(bark::actions::lightning::pay::LightningSendState::Paid(_)), None) => Ok(OpStatus::Pending),
            (Ok(bark::actions::lightning::pay::LightningSendState::InProgress(_)), _) => Ok(OpStatus::Pending),
            // Bark records a payment before it moves money: unknown and past
            // the quote's expiry means it was never started.
            (Ok(bark::actions::lightning::pay::LightningSendState::Unknown), Some(m)) => Ok(match m.status {
                MovementStatus::Successful => completed(&m),
                MovementStatus::Pending => OpStatus::Pending,
                _ => OpStatus::Failed { reason: "the payment failed".into(), cost: fee_of(&m) },
            }),
            (Ok(bark::actions::lightning::pay::LightningSendState::Unknown), None) => {
                let expiry: u64 = expiry.parse().map_err(|_| Error::InvalidInput(format!("bad pay key {key}")))?;
                Ok(if now() > expiry { OpStatus::failed("never paid; nothing moved") } else { OpStatus::Pending })
            }
            (Err(e), _) => Err(self.err(e)),
        }
    }

    async fn send_status(&self, key: &str) -> Result<OpStatus> {
        let mut parts = key.split('.');
        let (Some(dest), Some(amount), Some(at)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Error::InvalidInput(format!("bad send key {key}")));
        };
        let amount: u64 = amount.parse().map_err(|_| Error::InvalidInput(format!("bad send key {key}")))?;
        let at: i64 = at.parse().map_err(|_| Error::InvalidInput(format!("bad send key {key}")))?;
        let found = self.movements().await?.into_iter().find(|m| {
            m.subsystem.name == "bark.arkoor"
                && m.time.created_at.timestamp() >= at
                && m.sent_to.iter().any(|d| {
                    matches!(&d.destination, PaymentMethod::Ark(a) if hash_hex(&a.to_string()) == dest)
                        && d.amount.to_sat() * 1000 == amount
                })
        });
        Ok(match found {
            Some(m) if m.status == MovementStatus::Successful => completed(&m),
            Some(m) if m.status == MovementStatus::Pending => OpStatus::Pending,
            Some(m) => OpStatus::Failed { reason: "the Ark payment failed".into(), cost: fee_of(&m) },
            // Bark checkpoints a send before contacting the server, so no
            // movement a minute later means it never started.
            None if now() as i64 > at + 60 => OpStatus::failed("never sent; nothing moved"),
            None => OpStatus::Pending,
        })
    }
}

fn btc_network(network: Network) -> BtcNetwork {
    match network {
        Network::Mainnet => BtcNetwork::Bitcoin,
        Network::Testnet => BtcNetwork::Testnet,
        Network::Signet => BtcNetwork::Signet,
        Network::Regtest => BtcNetwork::Regtest,
    }
}

fn msat(amount: Amount) -> Msat {
    Msat::from_sats(amount.to_sat())
}

/// Bark counts whole sats: a fraction can't be asked for or sent.
fn whole_sats(amount: Msat) -> Result<Amount> {
    match amount.0 % 1000 {
        0 => Ok(Amount::from_sat(amount.sats_floor())),
        _ => Err(Error::InvalidInput(format!("{amount}: Ark counts whole sats"))),
    }
}

fn payment_hash(hex: &str) -> Result<PaymentHash> {
    let hash = sha256::Hash::from_str(hex).map_err(|_| Error::InvalidInput(format!("bad payment hash {hex}")))?;
    Ok(PaymentHash::from(hash))
}

fn hash_hex(s: &str) -> String {
    sha2::Sha256::digest(s.as_bytes())[..12].iter().map(|b| format!("{b:02x}")).collect()
}

fn fee_of(m: &Movement) -> Msat {
    msat(m.offchain_fee)
}

/// A finished movement as a completed operation. Bark's `effective_balance`
/// is the real change, fees included: in = amount - fee, out = amount + fee.
fn completed(m: &Movement) -> OpStatus {
    let fee = fee_of(m);
    let change = m.effective_balance.to_sat();
    let amount = if change >= 0 { change as u64 * 1000 + fee.0 } else { (change.unsigned_abs() * 1000).saturating_sub(fee.0) };
    OpStatus::Completed { amount: Msat(amount), fee }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Rail for BarkRail {
    fn id(&self) -> RailId {
        self.id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            ln_send: true,
            ln_receive: true,
            native_send: true,
            native_receive: true,
            onchain_in: true,
            onchain_out: true,
        }
    }

    async fn balance(&self) -> Result<BalanceSnapshot> {
        let b = self.wallet.balance().await.map_err(|e| self.err(e))?;
        Ok(BalanceSnapshot {
            rail: self.id.clone(),
            spendable: msat(b.spendable),
            // Still the wallet's: locked in a round, a payment or an exit.
            pending: msat(b.pending_in_round + b.pending_lightning_send + b.pending_exit),
            as_of: now(),
            stale: false,
        })
    }

    async fn ln_receive(&self, amount: Msat, memo: &str) -> Result<ReceiveOp> {
        let memo = (!memo.is_empty()).then(|| memo.to_string());
        let invoice =
            self.wallet.bolt11_invoice(whole_sats(amount)?, memo, None).await.map_err(|e| self.err(e))?;
        let hash = PaymentHash::from(&invoice).to_string();
        Ok(ReceiveOp {
            op: self.op("receive", &hash),
            amount,
            invoice: Some(invoice.to_string()),
            status: OpStatus::Pending,
        })
    }

    async fn ln_quote(&self, invoice: &Bolt11) -> Result<PayQuote> {
        if invoice.network() != self.network {
            return Err(Error::WrongNetwork { what: "invoice", expected: self.network, found: invoice.network() });
        }
        if invoice.is_expired() {
            return Err(Error::Expired);
        }
        let amount = invoice
            .amount()
            .ok_or_else(|| Error::InvalidInput("invoices without an amount are not supported".into()))?;
        let estimate = self
            .wallet
            .estimate_lightning_send_fee(Amount::from_sat(amount.0.div_ceil(1000)))
            .await
            .map_err(|e| self.err(e))?;
        let expires_at = invoice.expires_at();
        Ok(PayQuote {
            rail: self.id.clone(),
            quote_id: format!("{}.{expires_at}", invoice.payment_hash()),
            invoice: invoice.clone(),
            amount,
            fee_reserve: msat(estimate.fee),
            expires_at,
        })
    }

    async fn ln_pay(&self, quote: PayQuote) -> Result<PayOp> {
        let op = quote.op_id();
        let hash = quote.invoice.payment_hash();
        let paid = self
            .wallet
            .pay_lightning_invoice_with(quote.invoice.to_string(), None, LightningSendOptions::default().wait(true))
            .await;
        let status = match paid {
            Ok(_) => self.ln_pay_status(&quote.quote_id).await.unwrap_or(OpStatus::Pending),
            // Started but not finished, or never started: Bark's own record says which.
            Err(e) => match self.wallet.lightning_send_checkpoint(payment_hash(&hash)?).await {
                Ok(Some(_)) => OpStatus::Pending,
                _ => return Err(self.err(e)),
            },
        };
        Ok(PayOp { op, status })
    }

    async fn native_send(&self, _amount: Msat, _before: BeforeMove<'_>) -> Result<NativeToken> {
        Err(Error::InvalidInput(format!("{} pays addresses, not tokens: use native_pay", self.id)))
    }

    async fn native_receive(&self, _token: &str, _before: BeforeMove<'_>) -> Result<ReceiveOp> {
        Err(Error::InvalidInput(format!("{} is paid at an address: use native_address", self.id)))
    }

    async fn native_address(&self) -> Result<String> {
        Ok(self.wallet.new_address().await.map_err(|e| self.err(e))?.to_string())
    }

    async fn native_pay(&self, address: &str, amount: Msat, before: BeforeMove<'_>) -> Result<PayOp> {
        let dest = bark::ark::Address::from_str(address.trim())
            .map_err(|e| Error::InvalidInput(format!("not an Ark address: {e}")))?;
        self.wallet.validate_arkoor_address(&dest).await.map_err(|e| Error::InvalidInput(format!("{e}")))?;
        let sats = whole_sats(amount)?;
        let op = self.op("send", &format!("{}.{}.{}", hash_hex(&dest.to_string()), amount.0, now()));
        before(&op, amount)?;
        let status = match self.wallet.send_arkoor_payment(&dest, sats).await {
            Ok(_) => {
                let (_, key) = self.parse_op(&op)?;
                self.send_status(key).await.unwrap_or(OpStatus::Pending)
            }
            Err(e) => match self.err(e) {
                // Lost contact mid-way: the movement log settles it later.
                Error::OperatorUnavailable { .. } => OpStatus::Pending,
                e => return Err(e),
            },
        };
        Ok(PayOp { op, status })
    }

    async fn op_status(&self, op: &OpId) -> Result<OpStatus> {
        let (kind, key) = self.parse_op(op)?;
        match kind {
            "receive" => {
                // Claims a paid invoice; Bark's daemon would otherwise do it.
                self.wallet.sync_pending_lightning_receives().await.map_err(|e| self.err(e))?;
                match self.lightning_movement(key).await? {
                    Some(m) if m.status == MovementStatus::Successful => Ok(completed(&m)),
                    Some(m) if matches!(m.status, MovementStatus::Failed | MovementStatus::Canceled) => {
                        Ok(OpStatus::Expired)
                    }
                    _ => Ok(OpStatus::Pending),
                }
            }
            "pay" => self.ln_pay_status(key).await,
            "send" => self.send_status(key).await,
            "movement" => Err(Error::InvalidInput(format!("{op} is recorded complete when found"))),
            _ => Err(Error::UnknownOp(op.clone())),
        }
    }

    /// Syncs with the server and the chain, then reports what Bark did that
    /// INDIGO did not ask for, as arrivals.
    async fn recover(&self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        // A refresh the app was killed in leaves its VTXOs locked in a round
        // no process is driving (Bark's daemon would, but INDIGO runs without
        // it). Cancel those Bark says are safe to (no forfeits signed yet), and
        // push on any further along.
        if let Err(e) = self.wallet.cancel_all_pending_rounds().await {
            report.problems.push(format!("{}: interrupted rounds not cancelled: {e:#}", self.id));
        }
        if let Err(e) = self.wallet.progress_pending_rounds(None).await {
            report.problems.push(format!("{}: interrupted rounds not resumed: {e:#}", self.id));
        }
        self.wallet.sync().await;
        if let Err(e) = self.wallet.sync_onchain().await {
            report.problems.push(format!("{}: on-chain wallet not synced: {e:#}", self.id));
        }
        for m in self.movements().await? {
            if Self::started_by_indigo(&m) || m.status != MovementStatus::Successful {
                continue;
            }
            let OpStatus::Completed { amount, fee } = completed(&m) else { continue };
            let incoming = m.effective_balance.is_positive();
            report.arrivals.push(Arrival {
                op: self.op("movement", &m.id.to_string()),
                kind: if incoming { OpKind::NativeReceive } else { OpKind::NativeSend },
                amount,
                fee,
                detail: m.subsystem.name.trim_start_matches("bark.").replace('_', " "),
            });
        }
        Ok(report)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ArkOps for BarkRail {
    async fn onchain_address(&self) -> Result<String> {
        let address = self.onchain.write().await.address().await.map_err(|e| self.err(e))?;
        Ok(address.to_string())
    }

    async fn onchain_balance(&self) -> Result<Msat> {
        self.wallet.sync_onchain().await.map_err(|e| self.err(e))?;
        Ok(msat(self.onchain.read().await.balance().total()))
    }

    async fn board(&self, amount: Option<Msat>) -> Result<String> {
        self.wallet.sync_onchain().await.map_err(|e| self.err(e))?;
        let pending = match amount {
            Some(amount) => self.wallet.board_amount(whole_sats(amount)?).await,
            None => self.wallet.board_all().await,
        }
        .map_err(|e| self.err(e))?;
        Ok(pending.funding_tx.compute_txid().to_string())
    }

    async fn send_onchain(&self, address: &str, amount: Msat) -> Result<String> {
        let address = BtcAddress::from_str(address.trim())
            .map_err(|e| Error::InvalidInput(format!("not a Bitcoin address: {e}")))?
            .require_network(btc_network(self.network))
            .map_err(|e| Error::InvalidInput(format!("{e}")))?;
        let sent = self.wallet.send_onchain(address, whole_sats(amount)?).await.map_err(|e| self.err(e))?;
        Ok(sent.txid.to_string())
    }

    async fn vtxos(&self) -> Result<Vec<Vtxo>> {
        Ok(self
            .wallet
            .vtxos()
            .await
            .map_err(|e| self.err(e))?
            .into_iter()
            .map(|v| Vtxo {
                id: v.vtxo.id().to_string(),
                amount: msat(v.vtxo.amount()),
                expiry: Expiry::Height(v.vtxo.expiry_height().into()),
                state: format!("{:?}", v.state.kind()),
            })
            .collect())
    }

    async fn refresh(&self, all: bool) -> Result<Option<String>> {
        let due = if all {
            self.wallet.spendable_vtxos().await.map_err(|e| self.err(e))?
        } else {
            self.wallet.get_vtxos_to_refresh().await.map_err(|e| self.err(e))?
        };
        if due.is_empty() {
            return Ok(None);
        }
        let status = self.wallet.refresh_vtxos(due).await.map_err(|e| self.err(e))?;
        Ok(status.map(|s| format!("{s:?}")))
    }

    async fn exit_start(&self) -> Result<usize> {
        let count = self.wallet.vtxos().await.map_err(|e| self.err(e))?.len();
        self.wallet.exit_mgr().start_exit_for_entire_wallet().await.map_err(|e| self.err(e))?;
        Ok(count)
    }

    async fn exit_progress(&self) -> Result<ExitStatus> {
        self.wallet.sync_exits().await.map_err(|e| self.err(e))?;
        self.wallet.progress_exits().await.map_err(|e| self.err(e))?;
        let exits = self.wallet.exit_mgr();
        let claimable: Amount = exits.list_claimable().await.iter().map(|v| v.amount()).sum();
        let live = exits.get_exit_vtxos().await.len();
        let detail = match exits.all_claimable_at_height().await {
            Some(height) => format!("all claimable at block {height}"),
            None => "waiting for exit transactions to confirm".into(),
        };
        Ok(ExitStatus { in_progress: live, claimable: msat(claimable), detail })
    }

    async fn exit_claim(&self, address: &str) -> Result<String> {
        let address = BtcAddress::from_str(address.trim())
            .map_err(|e| Error::InvalidInput(format!("not a Bitcoin address: {e}")))?
            .require_network(btc_network(self.network))
            .map_err(|e| Error::InvalidInput(format!("{e}")))?;
        let exits = self.wallet.exit_mgr();
        let claimable = exits.list_claimable().await;
        let psbt = exits
            .drain_exits(&claimable, &self.wallet, address, None::<FeeRate>)
            .await
            .map_err(|e| self.err(anyhow::anyhow!("{e}")))?;
        let tx = psbt.extract_tx().map_err(|e| Error::Other(format!("exit claim: {e}")))?;
        self.wallet.chain().broadcast_tx(&tx).await.map_err(|e| self.err(e))?;
        Ok(tx.compute_txid().to_string())
    }
}
