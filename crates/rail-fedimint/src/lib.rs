//! The Fedimint pocket: a `Rail` over `fedimint-sdk`, one federation each.
//!
//! The SDK writes each operation and the money it moves in one database
//! transaction, so an operation it has no record of moved nothing. Operation
//! ids are built so they can be matched with the SDK's records after a crash:
//!
//! - `<rail>:receive:<SDK operation id>` for Lightning receives (no money
//!   moves until the invoice is paid, so the SDK's id is known in time)
//! - `<rail>:pay:<payment hash>.<quote expiry>` for Lightning payments,
//!   recorded before the SDK is asked to pay. No SDK payment for that hash
//!   once the quote has expired means nothing was paid.
//! - `<rail>:send:<SDK operation id>` for notes sent: recorded before the
//!   notes are handed over, which is when the money leaves
//! - `<rail>:notes:<hash of the notes>` for notes received, recorded before
//!   they are redeemed

use std::collections::HashMap;
use std::sync::Mutex;

use fedimint_sdk::{
    ActivityStatus, Amount, AnyOperation, Cursor, Bolt11Invoice, EcashReceiveState, EcashSendState, ErrorCode, Federation,
    LnQuote, LnReceiveState, LnSendState, Notes, OperationId, OperationKind,
};
use indigo_domain::{
    BalanceSnapshot, Bolt11, Capabilities, Error, Msat, NativeToken, Network, OpId, OpStatus, PayOp, PayQuote,
    RailId, ReceiveOp, RecoveryReport, Result, now,
};
use indigo_rail::{BeforeMove, Rail};
use sha2::{Digest, Sha256};

pub struct FedimintRail {
    id: RailId,
    network: Network,
    federation: Federation,
    /// Quotes from `ln_quote` not yet paid. The SDK pays exactly what it
    /// quoted; a quote made in another process is quoted again in `ln_pay`.
    quotes: Mutex<HashMap<String, LnQuote>>,
}

impl FedimintRail {
    pub fn new(federation: Federation, network: Network) -> Self {
        Self {
            id: RailId(format!("fedimint:{}", federation.id())),
            network,
            federation,
            quotes: Mutex::new(HashMap::new()),
        }
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

    fn err(&self, e: fedimint_sdk::Error) -> Error {
        let reason = e.message.clone();
        match e.code {
            ErrorCode::FederationUnreachable | ErrorCode::Timeout | ErrorCode::GatewayUnavailable => {
                Error::OperatorUnavailable { rail: self.id.clone(), reason }
            }
            ErrorCode::QuoteExpired => Error::Expired,
            ErrorCode::InsufficientBalance => Error::Other(format!("{}: {reason}", self.id)),
            ErrorCode::InvalidInput | ErrorCode::AmountlessInvoice | ErrorCode::NetworkMismatch => {
                Error::InvalidInput(reason)
            }
            _ => Error::OperatorError { rail: self.id.clone(), reason },
        }
    }

    fn lightning(&self) -> Result<fedimint_sdk::Lightning> {
        self.federation.lightning().ok_or_else(|| self.unsupported("Lightning"))
    }

    fn ecash(&self) -> Result<fedimint_sdk::Ecash> {
        self.federation.ecash().ok_or_else(|| self.unsupported("ecash"))
    }

    fn unsupported(&self, what: &str) -> Error {
        Error::InvalidInput(format!("{} has no {what} module", self.id))
    }

    async fn operation(&self, id: &str) -> Result<Option<AnyOperation>> {
        let id: OperationId = id.parse().map_err(|_| Error::InvalidInput(format!("bad operation id {id}")))?;
        self.federation.operation(&id).await.map_err(|e| self.err(e))
    }

    /// One page of SDK operations of one kind, newest first, and the cursor
    /// to the next page (`None` after the last). Callers stop at a match.
    async fn page(&self, kind: OperationKind, cursor: Option<Cursor>) -> Result<(Vec<AnyOperation>, Option<Cursor>)> {
        let page = self.federation.activity(cursor, 100).await.map_err(|e| self.err(e))?;
        let mut ops = Vec::new();
        for item in page.items.iter().filter(|i| i.kind == kind) {
            if let Some(op) = self.federation.operation(&item.operation_id).await.map_err(|e| self.err(e))? {
                ops.push(op);
            }
        }
        let next = if page.items.is_empty() { None } else { page.next };
        Ok((ops, next))
    }

    /// The SDK payment of the invoice with this payment hash, if any. The
    /// newest come first, so a payment just made is found at once.
    async fn find_payment(&self, hash: &str) -> Result<Option<fedimint_sdk::Operation<LnSendState>>> {
        let mut cursor = None;
        loop {
            let (ops, next) = self.page(OperationKind::LnSend, cursor).await?;
            for send in ops.iter().filter_map(AnyOperation::as_ln_send) {
                let details = send.details().await.map_err(|e| self.err(e))?;
                if Bolt11::parse(&details.invoice.to_string())?.payment_hash() == hash {
                    return Ok(Some(send));
                }
            }
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }

    /// The SDK redemption of these notes, by the hash of their canonical form.
    async fn find_redemption(&self, hash: &str) -> Result<Option<fedimint_sdk::Operation<EcashReceiveState>>> {
        let mut cursor = None;
        loop {
            let (ops, next) = self.page(OperationKind::EcashReceive, cursor).await?;
            for receive in ops.iter().filter_map(AnyOperation::as_ecash_receive) {
                let details = receive.details().await.map_err(|e| self.err(e))?;
                if details.notes.is_some_and(|n| notes_hash(&n) == hash) {
                    return Ok(Some(receive));
                }
            }
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }

    async fn pay_status(&self, key: &str) -> Result<OpStatus> {
        let (hash, expiry) = key.split_once('.').ok_or_else(|| Error::InvalidInput(format!("bad pay key {key}")))?;
        let Some(send) = self.find_payment(hash).await? else {
            // The SDK records a payment in the same transaction that funds it.
            let expiry: u64 = expiry.parse().map_err(|_| Error::InvalidInput(format!("bad pay key {key}")))?;
            return Ok(if now() > expiry {
                OpStatus::failed("never paid; nothing moved")
            } else {
                OpStatus::Pending
            });
        };
        let details = send.details().await.map_err(|e| self.err(e))?;
        Ok(match send.state().await.map_err(|e| self.err(e))? {
            LnSendState::Created | LnSendState::Funded => OpStatus::Pending,
            LnSendState::Success { fee, .. } => {
                OpStatus::Completed { amount: msat(details.invoice_amount), fee: msat(fee) }
            }
            // Found after a restart: the balance before is gone and the SDK
            // does not report the refund's fees, so they can't be counted.
            LnSendState::Refunded => OpStatus::failed(
                "the gateway could not pay; refunded, less federation fees this wallet could not measure \
                 (the app stopped during the refund): the audit will be off by them",
            ),
            LnSendState::Failed { reason } => OpStatus::failed(reason),
            // A state newer than this code: not final as far as we know.
            _ => OpStatus::Pending,
        })
    }

    async fn balance_now(&self) -> Result<Msat> {
        Ok(msat(self.federation.balance().await.map_err(|e| self.err(e))?))
    }

    /// Spending notes can leave change, which the federation issues after
    /// the operation itself has finished. Waits (briefly) until the balance
    /// is `before - spent`, so a caller that reads it next, or exits, sees the
    /// change. If the app stops first, the SDK issues it on the next start.
    async fn await_change(&self, before: Msat, spent: Msat) {
        let expected = before.0.saturating_sub(spent.0);
        for _ in 0..60 {
            match self.balance_now().await {
                Ok(now) if now.0 >= expected => return,
                Err(_) => return,
                Ok(_) => {}
            }
            futures_timer::Delay::new(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Gross and fee of a final note redemption.
    fn redeemed(details: &fedimint_sdk::EcashReceiveDetails) -> OpStatus {
        OpStatus::Completed { amount: msat(details.notes_value), fee: msat(details.fee) }
    }
}

fn msat(amount: Amount) -> Msat {
    Msat(amount.msats())
}

fn notes_hash(notes: &Notes) -> String {
    hex(&Sha256::digest(notes.to_string().as_bytes())[..16])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Rail for FedimintRail {
    fn id(&self) -> RailId {
        self.id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        let c = self.federation.capabilities();
        Capabilities {
            ln_send: c.lightning,
            ln_receive: c.lightning,
            native_send: c.ecash,
            native_receive: c.ecash,
            onchain_in: c.onchain,
            onchain_out: c.onchain,
        }
    }

    /// Read from the local database: always available, never stale.
    async fn balance(&self) -> Result<BalanceSnapshot> {
        let spendable = msat(self.federation.balance().await.map_err(|e| self.err(e))?);
        Ok(BalanceSnapshot { rail: self.id.clone(), spendable, pending: Msat::ZERO, as_of: now(), stale: false })
    }

    async fn ln_receive(&self, amount: Msat, memo: &str) -> Result<ReceiveOp> {
        let receive =
            self.lightning()?.receive(Amount::from_msats(amount.0), memo).await.map_err(|e| self.err(e))?;
        Ok(ReceiveOp {
            op: self.op("receive", &receive.operation.id().to_string()),
            amount,
            invoice: Some(receive.invoice.to_string()),
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
        let parsed: Bolt11Invoice =
            invoice.to_string().parse().map_err(|e: fedimint_sdk::Error| Error::InvalidInput(e.message))?;
        let quote = self.lightning()?.quote(&parsed).await.map_err(|e| self.err(e))?;
        let expires_at = quote.expires_at().epoch_millis() / 1000;
        let quote_id = format!("{}.{expires_at}", invoice.payment_hash());
        let out = PayQuote {
            rail: self.id.clone(),
            quote_id: quote_id.clone(),
            invoice: invoice.clone(),
            amount: msat(quote.invoice_amount()),
            fee_reserve: msat(quote.fee()),
            expires_at,
        };
        self.quotes.lock().unwrap_or_else(|p| p.into_inner()).insert(quote_id, quote);
        Ok(out)
    }

    async fn ln_pay(&self, quote: PayQuote) -> Result<PayOp> {
        let op = quote.op_id();
        let cached = self.quotes.lock().unwrap_or_else(|p| p.into_inner()).remove(&quote.quote_id);
        let sdk_quote = match cached {
            Some(q) => q,
            None => {
                // Quoted in another process: quote again, and refuse if it now costs more.
                let parsed: Bolt11Invoice = quote
                    .invoice
                    .to_string()
                    .parse()
                    .map_err(|e: fedimint_sdk::Error| Error::InvalidInput(e.message))?;
                let fresh = self.lightning()?.quote(&parsed).await.map_err(|e| self.err(e))?;
                if msat(fresh.fee()) > quote.fee_reserve {
                    return Err(Error::FeeCapExceeded { fee: msat(fresh.fee()), cap: quote.fee_reserve });
                }
                fresh
            }
        };
        let before = self.balance_now().await?;
        let send = self.lightning()?.send(sdk_quote).await.map_err(|e| self.err(e))?;
        // Funded from here on: nothing below may say "nothing moved". What
        // can't be read now is left Pending for op_status to settle.
        let status = match send.await_final().await {
            Ok(LnSendState::Success { fee, .. }) => {
                let (amount, fee) = (quote.amount, msat(fee));
                self.await_change(before, Msat(amount.0 + fee.0)).await;
                OpStatus::Completed { amount, fee }
            }
            Ok(LnSendState::Refunded) => match self.balance_now().await {
                // The refund comes back less the federation's fees on the
                // funding and refund transactions, which the SDK does not
                // report: measured here instead.
                Ok(after) => OpStatus::Failed {
                    reason: "the gateway could not pay; refunded".into(),
                    cost: Msat(before.0.saturating_sub(after.0)),
                },
                Err(_) => OpStatus::Pending,
            },
            Ok(LnSendState::Failed { reason }) => OpStatus::failed(reason),
            // Not final, or contact lost: recover() finds it by its payment hash.
            Ok(_) | Err(_) => OpStatus::Pending,
        };
        Ok(PayOp { op, status })
    }

    async fn native_send(&self, amount: Msat, before: BeforeMove<'_>) -> Result<NativeToken> {
        let ecash = self.ecash()?;
        let quote = ecash.quote(Amount::from_msats(amount.0)).await.map_err(|e| self.err(e))?;
        // Notes come in fixed denominations, so what is handed over can be a
        // little more than asked for: that is the amount that leaves.
        let handed_over = msat(quote.notes_value());
        let fee = Msat(quote.total().msats().saturating_sub(handed_over.0));
        // Splits the notes out of the balance; they leave when handed over.
        let balance = self.balance_now().await?;
        let sent = ecash.send(quote).await.map_err(|e| self.err(e))?;
        self.await_change(balance, Msat(handed_over.0 + fee.0)).await;
        let op = self.op("send", &sent.operation.id().to_string());
        if let Err(e) = before(&op, handed_over) {
            let _ = sent.operation.request_cancel().await;
            return Err(e);
        }
        Ok(NativeToken { op, amount: handed_over, fee, token: sent.notes.to_string() })
    }

    async fn native_receive(&self, token: &str, before: BeforeMove<'_>) -> Result<ReceiveOp> {
        let notes: Notes = token.trim().parse().map_err(|e: fedimint_sdk::Error| Error::InvalidInput(e.message))?;
        // The canonical form: the same notes pasted with different line
        // breaks or case get the same id.
        let key = notes_hash(&notes);
        // Redeemed before by this wallet. Checked here rather than read from
        // the SDK's error, whose wording differs between mint generations.
        if let Some(earlier) = self.find_redemption(&key).await?
            && matches!(earlier.state().await, Ok(EcashReceiveState::Done))
        {
            return Err(Error::AlreadySpent);
        }
        let op = self.op("notes", &key);
        let value = msat(notes.value());
        before(&op, value)?;
        let receive = self.ecash()?.receive(&notes).await.map_err(|e| self.err(e))?;
        let status = match receive.await_final().await.map_err(|e| self.err(e))? {
            EcashReceiveState::Done => Self::redeemed(&receive.details().await.map_err(|e| self.err(e))?),
            EcashReceiveState::Failed { reason } if reason.to_lowercase().contains("spent") => {
                return Err(Error::AlreadySpent);
            }
            EcashReceiveState::Failed { reason } => return Err(Error::OperatorError { rail: self.id.clone(), reason }),
            _ => OpStatus::Pending,
        };
        Ok(ReceiveOp { op, amount: value, invoice: None, status })
    }

    async fn op_status(&self, op: &OpId) -> Result<OpStatus> {
        let (kind, key) = self.parse_op(op)?;
        match kind {
            "receive" => {
                let receive = self
                    .operation(key)
                    .await?
                    .and_then(|o| o.as_ln_receive())
                    .ok_or_else(|| Error::UnknownOp(op.clone()))?;
                Ok(match receive.state().await.map_err(|e| self.err(e))? {
                    LnReceiveState::Claimed => {
                        let d = receive.details().await.map_err(|e| self.err(e))?;
                        OpStatus::Completed { amount: msat(d.invoice_amount), fee: msat(d.fee) }
                    }
                    LnReceiveState::Expired => OpStatus::Expired,
                    LnReceiveState::Canceled { reason } => OpStatus::failed(reason),
                    LnReceiveState::Failed => OpStatus::failed("the federation refused the payment"),
                    _ => OpStatus::Pending,
                })
            }
            "pay" => self.pay_status(key).await,
            // Still pending means the app stopped before the notes were handed
            // over (core marks the send completed before returning them).
            "send" => {
                let send = self
                    .operation(key)
                    .await?
                    .and_then(|o| o.as_ecash_send())
                    .ok_or_else(|| Error::UnknownOp(op.clone()))?;
                Ok(match send.state().await.map_err(|e| self.err(e))? {
                    EcashSendState::Created => {
                        send.request_cancel().await.map_err(|e| self.err(e))?;
                        OpStatus::Pending
                    }
                    EcashSendState::CancelRequested => OpStatus::Pending,
                    EcashSendState::Canceled => OpStatus::failed("never handed over; reclaimed"),
                    EcashSendState::Redeemed => {
                        let d = send.details().await.map_err(|e| self.err(e))?;
                        OpStatus::Completed { amount: msat(d.notes_value), fee: msat(d.fee) }
                    }
                    _ => OpStatus::Pending,
                })
            }
            "notes" => match self.find_redemption(key).await? {
                None => Ok(OpStatus::failed("the notes were not redeemed")),
                Some(receive) => Ok(match receive.state().await.map_err(|e| self.err(e))? {
                    EcashReceiveState::Done => Self::redeemed(&receive.details().await.map_err(|e| self.err(e))?),
                    EcashReceiveState::Failed { reason } => OpStatus::failed(reason),
                    _ => OpStatus::Pending,
                }),
            },
            _ => Err(Error::UnknownOp(op.clone())),
        }
    }

    /// The SDK resumes its own operations when the federation opens. Here,
    /// sends whose notes were never redeemed are looked at, which is what
    /// makes the SDK reclaim them once their deadline has passed.
    async fn recover(&self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        let mut cursor = None;
        loop {
            let (ops, next) = self.page(OperationKind::EcashSend, cursor).await?;
            for send in ops.iter().filter_map(AnyOperation::as_ecash_send) {
                match send.state().await {
                    Ok(EcashSendState::Canceled) => {
                        let d = send.details().await.map_err(|e| self.err(e))?;
                        report.reclaimed.push((self.op("send", &send.id().to_string()), msat(d.notes_value)));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        report.problems.push(format!("{}: send {} not checked: {}", self.id, send.id(), e.message))
                    }
                }
            }
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        // Activity rows the SDK could not read are worth a look.
        let unknown = self
            .federation
            .activity(None, 100)
            .await
            .map_err(|e| self.err(e))?
            .items
            .iter()
            .filter(|i| i.status == ActivityStatus::Unknown)
            .count();
        if unknown > 0 {
            report.problems.push(format!("{}: {unknown} operations in an unknown state", self.id));
        }
        Ok(report)
    }
}
