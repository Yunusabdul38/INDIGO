//! The Cashu pocket: a `Rail` over CDK, one wallet per mint.
//!
//! Operation ids are built from the mint's own ids, so the same operation
//! always maps to the same id and can be found again after a crash:
//!
//! - `<rail>:receive:<mint quote id>` for Lightning receives
//! - `<rail>:pay:<melt quote id>` for Lightning payments
//! - `<rail>:send:<CDK operation id>` for tokens sent
//! - `<rail>:token:<Y of the token's first proof>` for tokens received; CDK's
//!   transaction history lists those Ys, also after crash recovery

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use cdk::amount::SplitTarget;
use cdk::cdk_database::{self, WalletDatabase};
use cdk::mint_url::MintUrl;
use cdk::nuts::nut00::KnownMethod;
use cdk::dhke::hash_to_curve;
use cdk::nuts::{
    CurrencyUnit, MeltQuoteState, MintQuoteState, PaymentMethod, PublicKey, State as ProofState, Token,
};
use cdk::wallet::types::{TransactionDirection, TransactionStatus};
use cdk::wallet::{ReceiveOptions, SendOptions, Wallet};
use indigo_domain::{
    BalanceSnapshot, Bolt11, Capabilities, Error, NativeToken, Network, OpId, OpStatus, PayOp, PayQuote,
    RailId, ReceiveOp, RecoveryReport, Result, Msat, now,
};
use indigo_rail::{BeforeMove, Rail};
use uuid::Uuid;

pub type CashuStore = Arc<dyn WalletDatabase<cdk_database::Error> + Send + Sync>;

pub struct CashuRail {
    id: RailId,
    network: Network,
    wallet: Wallet,
}

impl CashuRail {
    /// A pocket for one mint. `seed` is this pocket's 64-byte secret; `store`
    /// is any CDK wallet database (SQLite natively, browser storage later).
    pub fn new(mint_url: &str, network: Network, seed: [u8; 64], store: CashuStore) -> Result<Self> {
        let url = MintUrl::from_str(mint_url).map_err(|e| Error::InvalidInput(format!("mint url: {e}")))?;
        let id = RailId(format!("cashu:{url}"));
        let wallet = Wallet::new(&url.to_string(), CurrencyUnit::Sat, store, seed, None)
            .map_err(|e| Error::Other(format!("cannot open Cashu wallet: {e}")))?;
        Ok(Self { id, network, wallet })
    }

    fn op(&self, kind: &str, key: &str) -> OpId {
        OpId(format!("{}:{kind}:{key}", self.id))
    }

    /// Splits an op id from this rail into its kind and the mint's key.
    fn parse_op<'a>(&self, op: &'a OpId) -> Result<(&'a str, &'a str)> {
        let rest = op
            .0
            .strip_prefix(&self.id.0)
            .and_then(|r| r.strip_prefix(':'))
            .ok_or_else(|| Error::UnknownOp(op.clone()))?;
        rest.split_once(':').ok_or_else(|| Error::UnknownOp(op.clone()))
    }

    fn err(&self, e: cdk::Error) -> Error {
        match e {
            cdk::Error::TokenAlreadySpent => Error::AlreadySpent,
            cdk::Error::ExpiredQuote(..) => Error::Expired,
            // The mint answered with an error it doesn't classify: it was
            // reached, so say so rather than "could not be reached".
            cdk::Error::UnknownErrorResponse(reason) => Error::OperatorError { rail: self.id.clone(), reason },
            ref other if !other.is_definitive_failure() => {
                Error::OperatorUnavailable { rail: self.id.clone(), reason: other.to_string() }
            }
            other => Error::Other(other.to_string()),
        }
    }

    async fn spendable(&self) -> Result<Msat> {
        Ok(sat(self.wallet.total_balance().await.map_err(|e| self.err(e))?))
    }

    /// Asks the mint (NUT-07) for the real state of every proof the wallet
    /// holds and records the answer, so the balance matches the mint. Covers
    /// two CDK 0.18 gaps:
    /// - crash recovery hands a melt's proofs back when the mint says "unpaid",
    ///   but a melt request sent just before the crash can still be executed
    /// - a failed receive of an already-spent token leaves its proofs pending
    ///   and tied to the failed operation, which CDK's own
    ///   `check_all_pending_proofs` skips
    ///
    /// Only proofs the mint reports SPENT are dropped; proofs of a payment
    /// still in flight are pending at the mint too and stay untouched.
    async fn sync_proof_states(&self) -> Result<()> {
        let unspent = self.wallet.get_unspent_proofs().await.map_err(|e| self.err(e))?;
        if !unspent.is_empty() {
            self.wallet.sync_proofs_state(unspent).await.map_err(|e| self.err(e))?;
        }
        // Proofs an unfinished CDK operation still uses are left to that
        // operation's own recovery.
        let active: Vec<Uuid> = self.store().get_incomplete_sagas().await.map_err(|e| self.db(e))?.iter().map(|s| s.id).collect();
        let held: Vec<_> = self
            .store()
            .get_proofs(
                Some(self.wallet.mint_url.clone()),
                Some(self.wallet.unit.clone()),
                Some(vec![ProofState::Pending, ProofState::Reserved, ProofState::PendingSpent]),
                None,
            )
            .await
            .map_err(|e| self.db(e))?
            .into_iter()
            .filter(|p| p.used_by_operation.is_none_or(|op| !active.contains(&op)))
            .map(|p| p.proof)
            .collect();
        if !held.is_empty() {
            self.wallet.check_proofs_spent(held).await.map_err(|e| self.err(e))?;
        }
        Ok(())
    }

    /// The fee the mint charged for a melt, from CDK's transaction history.
    /// Only a completed transaction has the real fee: while CDK is still
    /// finishing a recovered melt, its transaction carries the fee reserve.
    async fn melt_fee(&self, quote_id: &str) -> Result<Option<Msat>> {
        let txs = self.wallet.list_transactions(Some(TransactionDirection::Outgoing)).await.map_err(|e| self.err(e))?;
        Ok(txs
            .iter()
            .find(|tx| tx.quote_id.as_deref() == Some(quote_id) && tx.status == TransactionStatus::Completed)
            .map(|tx| sat(tx.fee)))
    }

    fn store(&self) -> &CashuStore {
        &self.wallet.localstore
    }

    fn db(&self, e: cdk_database::Error) -> Error {
        Error::Storage(format!("{}: {e}", self.id))
    }

    /// Whether any of the token's proofs is already spent at the mint.
    async fn token_spent(&self, token: &Token) -> Result<bool> {
        // Loads the mint's keysets into the store if this wallet has none yet.
        self.wallet.active_keyset().await.map_err(|e| self.err(e))?;
        let keysets = self.store().get_mint_keysets(self.wallet.mint_url.clone()).await.map_err(|e| self.db(e))?;
        let proofs = token
            .proofs(&keysets.unwrap_or_default())
            .map_err(|e| Error::InvalidInput(format!("token not from this mint: {e}")))?;
        let states = self.wallet.check_proofs_spent(proofs).await.map_err(|e| self.err(e))?;
        Ok(states.iter().any(|s| s.state == ProofState::Spent))
    }

    /// Whether CDK still has an unfinished operation `id`.
    async fn saga_open(&self, id: Uuid) -> Result<bool> {
        Ok(self.store().get_incomplete_sagas().await.map_err(|e| self.db(e))?.iter().any(|s| s.id == id))
    }

    /// Whether an unfinished CDK receive is redeeming the proof with this Y.
    async fn receive_open(&self, y: &PublicKey) -> Result<bool> {
        for saga in self.store().get_incomplete_sagas().await.map_err(|e| self.db(e))? {
            let held = self.store().get_reserved_proofs(&saga.id).await.map_err(|e| self.db(e))?;
            if held.iter().any(|p| p.y == *y) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn sat(amount: cdk::Amount) -> Msat {
    Msat::from_sats(amount.into())
}

/// Cashu counts whole sats: a fraction can't be asked for or sent.
fn whole_sats(amount: Msat) -> Result<cdk::Amount> {
    match amount.0 % 1000 {
        0 => Ok(amount.sats_floor().into()),
        _ => Err(Error::InvalidInput(format!("{amount}: a Cashu mint only takes whole sats"))),
    }
}

/// CDK reports a quote without an expiry as 0: that means "no expiry".
fn expired(expiry: u64) -> bool {
    expiry != 0 && expiry <= now()
}

/// A received token's id: the Y of its first proof, which CDK records on the
/// receive transaction.
fn token_y(token: &str) -> Result<PublicKey> {
    let token = Token::from_str(token.trim()).map_err(|e| Error::InvalidInput(format!("not a Cashu token: {e}")))?;
    let secret = token.token_secrets().into_iter().next().ok_or_else(|| Error::InvalidInput("empty token".into()))?;
    hash_to_curve(secret.as_bytes()).map_err(|e| Error::InvalidInput(format!("bad token secret: {e}")))
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Rail for CashuRail {
    fn id(&self) -> RailId {
        self.id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { ln_send: true, ln_receive: true, native_send: true, native_receive: true, ..Default::default() }
    }

    async fn balance(&self) -> Result<BalanceSnapshot> {
        // Proofs are local, so the balance can always be read; whether the mint
        // answers decides if it is current or stale.
        let spendable = self.spendable().await?;
        let pending = sat(self.wallet.total_pending_balance().await.map_err(|e| self.err(e))?);
        let stale = self.wallet.fetch_mint_info().await.is_err();
        Ok(BalanceSnapshot { rail: self.id.clone(), spendable, pending, as_of: now(), stale })
    }

    async fn ln_receive(&self, amount: Msat, memo: &str) -> Result<ReceiveOp> {
        let memo = (!memo.is_empty()).then(|| memo.to_string());
        let quote = self
            .wallet
            .mint_quote(PaymentMethod::Known(KnownMethod::Bolt11), Some(whole_sats(amount)?), memo, None)
            .await
            .map_err(|e| self.err(e))?;
        Ok(ReceiveOp {
            op: self.op("receive", &quote.id),
            amount,
            invoice: Some(quote.request),
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
        if invoice.amount().is_none() {
            return Err(Error::InvalidInput("invoices without an amount are not supported".into()));
        }
        let quote = self
            .wallet
            .melt_quote(PaymentMethod::Known(KnownMethod::Bolt11), invoice.to_string(), None, None)
            .await
            .map_err(|e| self.err(e))?;
        Ok(PayQuote {
            rail: self.id.clone(),
            quote_id: quote.id,
            invoice: invoice.clone(),
            amount: sat(quote.amount),
            fee_reserve: sat(quote.fee_reserve),
            // The quote and the invoice each expire; the earlier one wins.
            expires_at: match quote.expiry {
                0 => invoice.expires_at(),
                expiry => expiry.min(invoice.expires_at()),
            },
        })
    }

    async fn ln_pay(&self, quote: PayQuote) -> Result<PayOp> {
        let op = quote.op_id();
        // expires_at is already the earlier of the quote's and the invoice's expiry.
        if quote.expires_at <= now() {
            return Err(Error::Expired);
        }
        let needed = quote.max_total()?;
        let available = self.spendable().await?;
        if available < needed {
            return Err(Error::InsufficientFunds { available, needed });
        }
        let prepared = self.wallet.prepare_melt(&quote.quote_id, HashMap::new()).await.map_err(|e| self.err(e))?;
        let status = match prepared.confirm().await {
            Ok(melt) => match melt.state() {
                MeltQuoteState::Paid => {
                    OpStatus::Completed { amount: sat(melt.amount()), fee: sat(melt.fee_paid()) }
                }
                MeltQuoteState::Pending | MeltQuoteState::Unknown => OpStatus::Pending,
                MeltQuoteState::Unpaid | MeltQuoteState::Failed => {
                    OpStatus::failed(format!("the mint reported {:?}", melt.state()))
                }
            },
            // Lost contact mid-payment: the outcome is unknown, so it stays
            // pending and op_status / recover ask the mint later.
            Err(e) if !e.is_definitive_failure() => OpStatus::Pending,
            Err(e) => OpStatus::failed(e.to_string()),
        };
        Ok(PayOp { op, status })
    }

    async fn native_send(&self, amount: Msat, before: BeforeMove<'_>) -> Result<NativeToken> {
        let available = self.spendable().await?;
        if available < amount {
            return Err(Error::InsufficientFunds { available, needed: amount });
        }
        // Preparing only reserves proofs locally; confirming swaps at the mint.
        let prepared = self.wallet.prepare_send(whole_sats(amount)?, SendOptions::default()).await.map_err(|e| self.err(e))?;
        let op = self.op("send", &prepared.operation_id().to_string());
        let fee = sat(prepared.fee());
        if let Err(e) = before(&op, amount) {
            let _ = prepared.cancel().await;
            return Err(e);
        }
        let token = prepared.confirm(None).await.map_err(|e| self.err(e))?.to_string();
        Ok(NativeToken { op, amount, fee, token })
    }

    async fn native_receive(&self, token: &str, before: BeforeMove<'_>) -> Result<ReceiveOp> {
        let op = self.op("token", &token_y(token)?.to_hex());
        let parsed = Token::from_str(token.trim()).map_err(|e| Error::InvalidInput(format!("not a Cashu token: {e}")))?;
        let value = sat(parsed.value().map_err(|e| Error::InvalidInput(format!("bad token amount: {e}")))?);
        // Refuse a spent token up front: CDK 0.18.2 can never finish (or roll
        // back) a receive the mint refused as already spent.
        if self.token_spent(&parsed).await? {
            return Err(Error::AlreadySpent);
        }
        before(&op, value)?;
        let received = match self.wallet.receive(token.trim(), ReceiveOptions::default()).await {
            Ok(amount) => amount,
            Err(e) => {
                // Best effort: the receive error matters more than the cleanup.
                let _ = self.sync_proof_states().await;
                return Err(self.err(e));
            }
        };
        // The token's face value in, minus what the mint charged to swap it.
        let fee = Msat(value.0.saturating_sub(sat(received).0));
        Ok(ReceiveOp {
            op,
            amount: value,
            invoice: None,
            status: OpStatus::Completed { amount: value, fee },
        })
    }

    async fn op_status(&self, op: &OpId) -> Result<OpStatus> {
        let (kind, key) = self.parse_op(op)?;
        match kind {
            "receive" => {
                let quote = self.wallet.check_mint_quote_status(key).await.map_err(|e| self.err(e))?;
                Ok(match quote.state {
                    MintQuoteState::Issued => {
                        OpStatus::Completed { amount: sat(quote.amount_issued), fee: Msat::ZERO }
                    }
                    // Paid but not yet claimed: claim it now (the architecture's
                    // "op_status / claim ecash" step). recover() does the same on start.
                    MintQuoteState::Paid => {
                        let proofs =
                            self.wallet.mint(key, SplitTarget::default(), None).await.map_err(|e| self.err(e))?;
                        let amount = proofs.iter().map(|p| u64::from(p.amount)).sum();
                        OpStatus::Completed { amount: Msat::from_sats(amount), fee: Msat::ZERO }
                    }
                    MintQuoteState::Unpaid if expired(quote.expiry) => OpStatus::Expired,
                    MintQuoteState::Unpaid => OpStatus::Pending,
                })
            }
            "pay" => {
                let quote = self.wallet.check_melt_quote_status(key).await.map_err(|e| self.err(e))?;
                Ok(match quote.state {
                    MeltQuoteState::Paid => {
                        self.sync_proof_states().await?;
                        match self.melt_fee(key).await? {
                            Some(fee) => OpStatus::Completed { amount: sat(quote.amount), fee },
                            // Paid, but CDK has not finished its side yet (change
                            // not restored); recover() completes it on the next start.
                            None => OpStatus::Pending,
                        }
                    }
                    MeltQuoteState::Pending | MeltQuoteState::Unknown => OpStatus::Pending,
                    MeltQuoteState::Failed => {
                        OpStatus::failed("the mint reports the payment failed")
                    }
                    // After a crash, "unpaid" does not prove the request will
                    // never land: one sent just before the kill can still be
                    // executed. Only an expired quote can no longer be paid.
                    MeltQuoteState::Unpaid if expired(quote.expiry) => {
                        OpStatus::failed("the quote expired unpaid; nothing moved")
                    }
                    MeltQuoteState::Unpaid => OpStatus::Pending,
                })
            }
            // Still pending means the process died before the token was handed
            // over (core marks it completed before returning it), so reclaim it.
            "send" => {
                let id = Uuid::parse_str(key).map_err(|_| Error::UnknownOp(op.clone()))?;
                let unclaimed = self.wallet.get_pending_sends().await.map_err(|e| self.err(e))?;
                if unclaimed.contains(&id) {
                    let reclaimed = self.wallet.revoke_send(id).await.map_err(|e| self.err(e))?;
                    Ok(OpStatus::failed(format!("never handed over; reclaimed {reclaimed} sats")))
                } else if self.saga_open(id).await? {
                    Ok(OpStatus::Pending)
                } else {
                    Ok(OpStatus::failed("the token was never created; nothing moved"))
                }
            }
            "token" => {
                let y = PublicKey::from_hex(key).map_err(|_| Error::UnknownOp(op.clone()))?;
                let txs = self.wallet.list_transactions(Some(TransactionDirection::Incoming)).await.map_err(|e| self.err(e))?;
                if let Some(tx) = txs.iter().find(|tx| tx.ys.contains(&y) && tx.status == TransactionStatus::Completed) {
                    // CDK records the amount net of the swap fee.
                    let fee = sat(tx.fee);
                    Ok(OpStatus::Completed { amount: Msat(sat(tx.amount).0 + fee.0), fee })
                } else if self.receive_open(&y).await? {
                    Ok(OpStatus::Pending)
                } else {
                    Ok(OpStatus::failed("the token was not redeemed"))
                }
            }
            _ => Err(Error::UnknownOp(op.clone())),
        }
    }

    async fn restore(&self) -> Result<()> {
        // NUT-13: the mint re-signs what this seed's secrets blinded before;
        // CDK keeps the unspent proofs.
        self.wallet.restore().await.map_err(|e| self.err(e))?;
        Ok(())
    }

    async fn recover(&self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        // Each step talks to the mint; a failure is reported, not fatal, so the
        // wallet still opens with the mint down.
        match self.wallet.recover_incomplete_sagas().await {
            Ok(r) => {
                report.resolved += r.recovered + r.compensated;
                if r.failed > 0 {
                    report.problems.push(format!(
                        "{}: {} interrupted operations could not be finished (RUST_LOG=cdk=info shows why)",
                        self.id, r.failed
                    ));
                }
            }
            Err(e) => report.problems.push(format!("{}: interrupted operations not checked: {e}", self.id)),
        }
        match self.wallet.finalize_pending_melts().await {
            Ok(melts) => report.resolved += melts.len(),
            Err(e) => report.problems.push(format!("{}: pending payments not checked: {e}", self.id)),
        }
        match self.wallet.mint_unissued_quotes().await {
            Ok(amount) => report.claimed = sat(amount),
            Err(e) => report.problems.push(format!("{}: paid invoices not claimed: {e}", self.id)),
        }
        if let Err(e) = self.sync_proof_states().await {
            report.problems.push(format!("{}: proof states not checked: {e}", self.id));
        }
        Ok(report)
    }
}
