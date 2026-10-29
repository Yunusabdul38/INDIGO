//! The wallet facade that apps (CLI, mobile, web) talk to.
//!
//! Rules every method follows:
//! - An operation is recorded in the store before the operator is asked to
//!   move money, so a crash at any point leaves a record to recover from.
//! - A lost connection never counts as failure: the operation stays pending
//!   and `refresh` / `recover` ask the operator for the truth.
//! - Fee caps are checked before paying, never after.
//! - Moves between pockets go through `indigo-engine`, which saves each step.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use indigo_domain::{
    BalanceSnapshot, Bolt11, Error, Msat, NativeToken, Network, OpId, OpKind, OpStatus, PayOp, PayQuote, RailId,
    ReceiveOp, RecoveryReport, Result, now,
};
use indigo_keys::Seed;
use indigo_rail::Rail;
use indigo_engine::{Engine, Pockets};
use indigo_rail::ArkOps;
pub use indigo_rail::{ExitStatus, Expiry, Vtxo};
pub use indigo_rail_ark_arkade::ArkadeConfig;
use indigo_rail_ark_arkade::ArkadeRail;
pub use indigo_rail_ark_bark::{BarkChain, BarkConfig};
use indigo_rail_ark_bark::BarkRail;
use indigo_rail_cashu::CashuRail;
use indigo_rail_fedimint::FedimintRail;
pub use indigo_store::{IntentRecord, JournalEntry, OpRecord};
use indigo_store::Store;

mod route;
pub use route::SendReport;
pub use indigo_router::Destination;

pub struct WalletConfig {
    pub network: Network,
    /// Where the wallet keeps its files. One directory per network.
    pub data_dir: PathBuf,
    /// The Cashu mint, if the wallet has a Cashu pocket.
    pub cashu_mint: Option<String>,
    /// The Ark server for a Bark pocket, if the wallet has one.
    pub bark: Option<BarkConfig>,
    /// The operator for an Arkade pocket, if the wallet has one.
    pub arkade: Option<ArkadeConfig>,
}

/// What a federation looks like before joining it.
pub use fedimint_sdk::FederationPreview;

pub struct Wallet {
    network: Network,
    store: Store,
    fedimint: fedimint_sdk::Sdk,
    rails: RwLock<Vec<Arc<dyn Rail>>>,
    /// The Ark pockets again, for what only they can do.
    ark: Vec<Arc<dyn ArkOps>>,
    /// Pockets that could not be opened; `recover` reports them.
    open_problems: Vec<String>,
}

/// What happened during start-up recovery.
#[derive(Debug, Default)]
pub struct StartupReport {
    pub rails: Vec<(RailId, RecoveryReport)>,
    /// Open operations whose status changed while the app was down.
    pub settled: Vec<(OpId, OpStatus)>,
    /// Moves between pockets that were still open, as they stand now.
    pub moves: Vec<IntentRecord>,
    /// Open operations or moves that could not be checked; they stay open.
    pub problems: Vec<String>,
}

impl StartupReport {
    pub fn is_quiet(&self) -> bool {
        self.settled.is_empty()
            && self.problems.is_empty()
            && self.moves.is_empty()
            && self.rails.iter().all(|(_, r)| {
                r.resolved == 0
                    && r.claimed == Msat::ZERO
                    && r.reclaimed.is_empty()
                    && r.arrivals.is_empty()
                    && r.problems.is_empty()
            })
    }
}

/// One rail's balance checked against its journal (test 1.10).
#[derive(Debug)]
pub struct Audit {
    pub rail: RailId,
    pub balance: Msat,
    /// Signed msat.
    pub journal: i64,
    pub in_flight: usize,
}

impl Audit {
    pub fn matches(&self) -> bool {
        self.balance.0 as i64 == self.journal
    }

    /// Balance and journal can only be compared once nothing is in flight:
    /// a payment whose outcome is still unknown is in one and not the other.
    pub fn settled(&self) -> bool {
        self.in_flight == 0
    }
}

impl Wallet {
    /// Opens the wallet. Apps call `recover()` next, before moving money.
    pub async fn open(config: &WalletConfig, seed: &Seed) -> Result<Self> {
        // The pockets' libraries pull in both of rustls's crypto backends, so
        // it can't choose one by itself. `ring` also builds for browsers.
        // Already installed (a second open) is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        std::fs::create_dir_all(&config.data_dir).map_err(|e| Error::Storage(e.to_string()))?;
        let store = Store::open(&config.data_dir.join("indigo.sqlite"))?;
        let mut rails: Vec<Arc<dyn Rail>> = Vec::new();
        if let Some(mint) = &config.cashu_mint {
            let cashu_db = cdk_sqlite::WalletSqliteDatabase::new(config.data_dir.join("cashu.sqlite"))
                .await
                .map_err(|e| Error::Storage(format!("cashu store: {e}")))?;
            rails.push(Arc::new(CashuRail::new(mint, config.network, seed.cashu(), Arc::new(cashu_db))?));
        }
        // One SDK instance holds every federation joined; each is one pocket.
        let fedimint_dir = config.data_dir.join("fedimint");
        let fedimint = fedimint_sdk::Sdk::builder()
            .storage(fedimint_sdk::Storage::at(fedimint_dir.to_str().ok_or_else(|| Error::Storage("data dir is not utf-8".into()))?).map_err(sdk_err)?)
            .mnemonic(seed.fedimint_words().parse().map_err(sdk_err)?)
            .build()
            .await
            .map_err(sdk_err)?;
        for federation in fedimint.federations() {
            rails.push(Arc::new(FedimintRail::new(federation, config.network)));
        }
        let mut ark: Vec<Arc<dyn ArkOps>> = Vec::new();
        let mut open_problems = Vec::new();
        if let Some(bark) = &config.bark {
            // An unreachable Ark server leaves out its pocket, not the wallet.
            match BarkRail::open(bark, config.network, seed.ark(), &config.data_dir.join("bark")).await {
                Ok(rail) => {
                    let rail = Arc::new(rail);
                    rails.push(rail.clone());
                    ark.push(rail);
                }
                Err(e) => open_problems.push(format!("Ark pocket at {} not opened: {e}", bark.server)),
            }
        }
        if let Some(arkade) = &config.arkade {
            match ArkadeRail::open(arkade, config.network, seed.ark(), &config.data_dir.join("arkade")).await {
                Ok(rail) => {
                    let rail = Arc::new(rail);
                    rails.push(rail.clone());
                    ark.push(rail);
                }
                Err(e) => open_problems.push(format!("Ark pocket at {} not opened: {e}", arkade.server)),
            }
        }
        Ok(Self { network: config.network, store, fedimint, rails: RwLock::new(rails), ark, open_problems })
    }

    /// Stops background work cleanly. Apps call it before exiting.
    pub async fn close(&self) -> Result<()> {
        self.fedimint.shutdown().await.map_err(sdk_err)
    }

    /// What joining the federation behind `invite` would mean, before joining.
    pub async fn preview_federation(&self, invite: &str) -> Result<FederationPreview> {
        let invite: fedimint_sdk::InviteCode = invite.trim().parse().map_err(sdk_err)?;
        self.fedimint.preview(&invite).await.map_err(sdk_err)
    }

    /// Joins a federation; it becomes a new pocket.
    pub async fn join_federation(&self, invite: &str) -> Result<RailId> {
        let invite: fedimint_sdk::InviteCode = invite.trim().parse().map_err(sdk_err)?;
        let preview = self.fedimint.preview(&invite).await.map_err(sdk_err)?;
        let network: Network = match preview.network {
            fedimint_sdk::Network::Bitcoin => Network::Mainnet,
            fedimint_sdk::Network::Testnet | fedimint_sdk::Network::Testnet4 => Network::Testnet,
            fedimint_sdk::Network::Signet => Network::Signet,
            fedimint_sdk::Network::Regtest => Network::Regtest,
            _ => return Err(Error::InvalidInput("federation on an unknown network".into())),
        };
        if network != self.network {
            return Err(Error::WrongNetwork { what: "federation", expected: self.network, found: network });
        }
        let federation = self.fedimint.join(&invite).await.map_err(sdk_err)?;
        let rail = FedimintRail::new(federation, self.network);
        let id = rail.id();
        self.rails.write().unwrap_or_else(|p| p.into_inner()).push(Arc::new(rail));
        Ok(id)
    }

    /// On a new device: joins the federation behind `invite` by rescanning
    /// it for what this wallet's seed owns, waiting up to `wait`. The pocket
    /// cannot spend until the rescan is complete (Fedimint's own lock).
    pub async fn restore_federation(&self, invite: &str, wait: Duration) -> Result<RailId> {
        let invite: fedimint_sdk::InviteCode = invite.trim().parse().map_err(sdk_err)?;
        let recovery = self.fedimint.recover(&invite).await.map_err(sdk_err)?;
        let done = tokio::time::timeout(wait, recovery.progress.await_final())
            .await
            .map_err(|_| Error::Other("the federation rescan is still running; it carries on in the background".into()))?
            .map_err(sdk_err)?;
        if !done.is_complete() {
            return Err(Error::Other(format!("the federation rescan stopped: {done:?}")));
        }
        let rail = FedimintRail::new(recovery.federation, self.network);
        let id = rail.id();
        self.rails.write().unwrap_or_else(|p| p.into_inner()).push(Arc::new(rail));
        Ok(id)
    }

    /// Restores from the 12 words on a new device: every pocket finds what
    /// its seed owns, recovery runs, and whatever each pocket holds that the
    /// (new, empty) journal does not explain is recorded as restored.
    pub async fn restore(&self) -> Result<StartupReport> {
        let mut problems = Vec::new();
        for rail in self.rails() {
            if let Err(e) = rail.restore().await {
                problems.push(format!("{}: {e}", rail.id()));
            }
        }
        let mut report = self.recover().await?;
        report.problems.extend(problems);
        for rail in self.rails() {
            let Ok(b) = rail.balance().await else { continue };
            let held = b.spendable.checked_add(b.pending)?;
            let journal = self.store.journal_total(&rail.id())?;
            if held.0 as i64 > journal {
                let amount = Msat(held.0 - journal.max(0) as u64);
                let op = OpId(format!("restore:{}:{}", rail.id(), now()));
                let status = OpStatus::Completed { amount, fee: Msat::ZERO };
                self.record(&op, &*rail, OpKind::NativeReceive, amount, &status, Some("restored from the 12 words".into()))?;
            }
        }
        Ok(report)
    }

    fn rails(&self) -> Vec<Arc<dyn Rail>> {
        self.rails.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The rail whose id starts with `name` (e.g. `cashu`, `fedimint`, or a
    /// full rail id).
    pub fn rail(&self, name: &str) -> Result<Arc<dyn Rail>> {
        self.rails()
            .into_iter()
            .find(|r| r.id().0 == name || r.id().0.starts_with(&format!("{name}:")))
            .ok_or_else(|| Error::InvalidInput(format!("no {name} pocket in this wallet")))
    }

    pub async fn balances(&self) -> Vec<Result<BalanceSnapshot>> {
        let mut out = Vec::new();
        for rail in self.rails() {
            out.push(rail.balance().await);
        }
        out
    }

    /// Asks for a Lightning invoice; the operation is recorded as pending.
    pub async fn receive(&self, rail: &str, amount: Msat, memo: &str) -> Result<ReceiveOp> {
        let rail = self.rail(rail)?;
        let op = rail.ln_receive(amount, memo).await?;
        self.record(&op.op, &*rail, OpKind::LnReceive, amount, &op.status, op.invoice.clone())?;
        Ok(op)
    }

    /// Asks the operator where an operation stands and records the answer.
    pub async fn refresh(&self, op: &OpId) -> Result<OpStatus> {
        let record = self.store.get(op)?.ok_or_else(|| Error::UnknownOp(op.clone()))?;
        if record.status.is_final() {
            return Ok(record.status);
        }
        let rail = self.rail_for(&record.rail)?;
        let status = rail.op_status(op).await?;
        self.store.set_status(op, &status)?;
        Ok(status)
    }

    /// What paying `invoice` would cost. Nothing moves.
    pub async fn quote(&self, rail: &str, invoice: &str) -> Result<PayQuote> {
        let invoice = Bolt11::parse(invoice)?;
        self.rail(rail)?.ln_quote(&invoice).await
    }

    /// Pays a quote, refusing up front if its fee reserve is above `max_fee`.
    pub async fn pay(&self, quote: PayQuote, max_fee: Option<Msat>) -> Result<PayOp> {
        quote.check_fee_cap(max_fee)?;
        let rail = self.rail_for(&quote.rail)?;
        let op = quote.op_id();
        self.record(&op, &*rail, OpKind::LnPay, quote.amount, &OpStatus::Pending, Some(quote.invoice.to_string()))?;
        match rail.ln_pay(quote).await {
            Ok(paid) => {
                self.store.set_status(&paid.op, &paid.status)?;
                Ok(paid)
            }
            // Contact lost: stays pending; refresh/recover settle it.
            Err(e @ Error::OperatorUnavailable { .. }) => Err(e),
            // Refused before any money moved.
            Err(e) => {
                self.store.set_status(&op, &OpStatus::failed(e.to_string()))?;
                Err(e)
            }
        }
    }

    /// Creates a token. It is recorded as sent before it is returned: if the
    /// app dies first, `recover()` reclaims it.
    pub async fn send_token(&self, rail: &str, amount: Msat) -> Result<NativeToken> {
        let rail = self.rail(rail)?;
        let recorded = OnceLock::new();
        let before = |op: &OpId, amount: Msat| {
            self.record(op, &*rail, OpKind::NativeSend, amount, &OpStatus::Pending, None)?;
            let _ = recorded.set(op.clone());
            Ok(())
        };
        match rail.native_send(amount, &before).await {
            Ok(token) => {
                // The rail says what was handed over: notes can round up.
                self.store.set_status(&token.op, &OpStatus::Completed { amount: token.amount, fee: token.fee })?;
                Ok(token)
            }
            Err(e) => Err(self.settle_after(recorded.get(), e).await),
        }
    }

    /// An address another wallet of the same kind pays into (an Ark address).
    pub async fn native_address(&self, rail: &str) -> Result<String> {
        self.rail(rail)?.native_address().await
    }

    /// Pays a native address (Ark to Ark); recorded before money moves.
    pub async fn pay_address(&self, rail: &str, address: &str, amount: Msat) -> Result<PayOp> {
        let rail = self.rail(rail)?;
        let recorded = OnceLock::new();
        let before = |op: &OpId, amount: Msat| {
            self.record(op, &*rail, OpKind::NativeSend, amount, &OpStatus::Pending, Some(address.to_string()))?;
            let _ = recorded.set(op.clone());
            Ok(())
        };
        match rail.native_pay(address, amount, &before).await {
            Ok(paid) => {
                self.store.set_status(&paid.op, &paid.status)?;
                Ok(paid)
            }
            Err(e) => Err(self.settle_after(recorded.get(), e).await),
        }
    }

    /// The Ark pocket whose id starts with `name` (e.g. `ark-bark`); plain
    /// `ark` is the wallet's first Ark pocket.
    pub fn ark(&self, name: &str) -> Result<Arc<dyn ArkOps>> {
        self.ark
            .iter()
            .find(|r| name == "ark" || r.id().0 == name || r.id().0.starts_with(&format!("{name}:")))
            .cloned()
            .ok_or_else(|| Error::InvalidInput(format!("no {name} Ark pocket in this wallet")))
    }

    pub async fn receive_token(&self, rail: &str, token: &str) -> Result<ReceiveOp> {
        let rail = self.rail(rail)?;
        let recorded = OnceLock::new();
        let before = |op: &OpId, amount: Msat| {
            self.record(op, &*rail, OpKind::NativeReceive, amount, &OpStatus::Pending, None)?;
            let _ = recorded.set(op.clone());
            Ok(())
        };
        match rail.native_receive(token, &before).await {
            Ok(op) => {
                self.store.set_status(&op.op, &op.status)?;
                Ok(op)
            }
            Err(e) => Err(self.settle_after(recorded.get(), e).await),
        }
    }

    /// After a recorded operation errored, asks the operator what actually
    /// happened, so the record never claims more or less than moved. If it
    /// can't tell yet, the op stays pending for `recover()`.
    async fn settle_after(&self, op: Option<&OpId>, e: Error) -> Error {
        if let Some(op) = op {
            let _ = self.refresh(op).await;
        }
        e
    }

    pub fn ops(&self) -> Result<Vec<OpRecord>> {
        self.store.ops()
    }

    pub fn journal(&self) -> Result<Vec<JournalEntry>> {
        self.store.journal()
    }

    /// Every rail's balance against the sum of its journal entries.
    pub async fn audit(&self) -> Result<Vec<Audit>> {
        let open = self.store.open_ops()?;
        let mut out = Vec::new();
        for rail in self.rails() {
            let b = rail.balance().await?;
            out.push(Audit {
                rail: rail.id(),
                // Pending proofs are still the wallet's until their payment settles.
                balance: b.spendable.checked_add(b.pending)?,
                journal: self.store.journal_total(&rail.id())?,
                in_flight: open.iter().filter(|o| o.rail == rail.id()).count(),
            });
        }
        Ok(out)
    }

    /// Starts a move: the destination issues an invoice and the source
    /// quotes paying it. Nothing is paid until `confirm_move`. With a `key`
    /// already used, returns that move instead of starting another.
    pub async fn start_move(&self, from: &str, to: &str, amount: Msat, key: Option<&str>) -> Result<IntentRecord> {
        let (from, to) = (self.rail(from)?.id(), self.rail(to)?.id());
        Engine::new(&self.store, self).start(&from, &to, amount, key).await
    }

    /// Pays a started move, waiting up to `wait` for the destination to claim it.
    pub async fn confirm_move(&self, id: &str, wait: Duration) -> Result<IntentRecord> {
        Engine::new(&self.store, self).confirm(id, wait).await
    }

    pub fn moves(&self) -> Result<Vec<IntentRecord>> {
        self.store.intents()
    }

    /// Finishes or rolls back whatever a crash interrupted: each rail's own
    /// recovery first, then every open operation is checked with its
    /// operator, then every open move is taken as far as it can go.
    pub async fn recover(&self) -> Result<StartupReport> {
        let mut report = StartupReport { problems: self.open_problems.clone(), ..Default::default() };
        for rail in self.rails() {
            let r = rail.recover().await.unwrap_or_else(|e| RecoveryReport {
                problems: vec![e.to_string()],
                ..Default::default()
            });
            for (send, amount) in &r.reclaimed {
                self.record_reclaim(send, &*rail, *amount)?;
            }
            // A rail reports every arrival it knows of; only new ones are
            // recorded, and only those stay in the report.
            let mut r = r;
            let mut new = Vec::new();
            for arrival in std::mem::take(&mut r.arrivals) {
                if self.store.get(&arrival.op)?.is_none() {
                    let status = OpStatus::Completed { amount: arrival.amount, fee: arrival.fee };
                    self.record(&arrival.op, &*rail, arrival.kind, arrival.amount, &status, Some(arrival.detail.clone()))?;
                    new.push(arrival);
                }
            }
            r.arrivals = new;
            report.rails.push((rail.id(), r));
        }
        for open in self.store.open_ops()? {
            match self.refresh(&open.op).await {
                Ok(status) if status.is_final() => report.settled.push((open.op, status)),
                Ok(_) => {}
                Err(e) => report.problems.push(format!("{}: {e}", open.op)),
            }
        }
        for (intent, error) in Engine::new(&self.store, self).recover().await? {
            if let Some(e) = error {
                report.problems.push(format!("move {}: {e}", intent.id));
            }
            report.moves.push(intent);
        }
        Ok(report)
    }

    /// Notes or tokens sent earlier that nobody redeemed came back. Only a
    /// send the journal debited is credited back: one that never completed
    /// was never debited.
    fn record_reclaim(&self, send: &OpId, rail: &dyn Rail, amount: Msat) -> Result<()> {
        let debited = matches!(self.store.get(send)?, Some(OpRecord { status: OpStatus::Completed { .. }, .. }));
        if !debited {
            return Ok(());
        }
        let op = OpId(format!("{send}:reclaimed"));
        let detail = Some(format!("unredeemed {send}"));
        self.record(&op, rail, OpKind::NativeReceive, amount, &OpStatus::Completed { amount, fee: Msat::ZERO }, detail)
    }

    fn record(
        &self,
        op: &OpId,
        rail: &dyn Rail,
        kind: OpKind,
        amount: Msat,
        status: &OpStatus,
        detail: Option<String>,
    ) -> Result<()> {
        self.store.record(&OpRecord {
            op: op.clone(),
            rail: rail.id(),
            kind,
            amount,
            status: status.clone(),
            detail,
            created_at: now(),
            updated_at: now(),
        })
    }

    fn rail_for(&self, id: &RailId) -> Result<Arc<dyn Rail>> {
        self.rails()
            .into_iter()
            .find(|r| r.id() == *id)
            .ok_or_else(|| Error::InvalidInput(format!("no pocket {id} in this wallet")))
    }
}

fn sdk_err(e: fedimint_sdk::Error) -> Error {
    Error::Other(format!("fedimint: {}", e.message))
}

/// What a move needs from the wallet: each call records its operation as the
/// wallet's own calls do.
#[async_trait::async_trait]
impl Pockets for Wallet {
    async fn receive(&self, rail: &RailId, amount: Msat) -> Result<ReceiveOp> {
        let rail = self.rail_for(rail)?;
        let op = rail.ln_receive(amount, "INDIGO move").await?;
        self.record(&op.op, &*rail, OpKind::LnReceive, amount, &op.status, op.invoice.clone())?;
        Ok(op)
    }

    async fn quote(&self, rail: &RailId, invoice: &str) -> Result<PayQuote> {
        self.rail_for(rail)?.ln_quote(&Bolt11::parse(invoice)?).await
    }

    async fn pay(&self, quote: PayQuote) -> Result<PayOp> {
        // The user accepted this quote's fee reserve when confirming the move.
        Wallet::pay(self, quote, None).await
    }

    async fn refresh(&self, op: &OpId) -> Result<OpStatus> {
        Wallet::refresh(self, op).await
    }

    async fn recheck(&self, op: &OpId) -> Result<OpStatus> {
        let record = self.store.get(op)?.ok_or_else(|| Error::UnknownOp(op.clone()))?;
        if !record.status.is_final() {
            return Wallet::refresh(self, op).await;
        }
        let status = self.rail_for(&record.rail)?.op_status(op).await?;
        if let OpStatus::Completed { amount, fee } = &status {
            self.store.correct_to_completed(op, *amount, *fee)?;
        }
        Ok(status)
    }

    async fn sleep(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }
}
