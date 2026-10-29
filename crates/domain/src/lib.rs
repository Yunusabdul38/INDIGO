//! Shared types: amounts, ids, operation status, errors.
//!
//! Everything here is plain data with no I/O, so it builds for every target,
//! the browser included.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An amount in millisatoshis. Lightning and Fedimint count in msat, so every
/// amount, balance and fee in INDIGO does too; Cashu (whole sats) converts at
/// its edge. Displayed in sats, with decimals only when there is a fraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Msat(pub u64);

impl Msat {
    pub const ZERO: Msat = Msat(0);

    /// More sats than will ever exist still fit, so this cannot overflow in practice.
    pub const fn from_sats(sats: u64) -> Msat {
        Msat(sats.saturating_mul(1000))
    }

    /// Whole sats, any fraction dropped (what a sat-only rail can spend).
    pub const fn sats_floor(self) -> u64 {
        self.0 / 1000
    }

    pub fn checked_add(self, other: Msat) -> Result<Msat> {
        self.0.checked_add(other.0).map(Msat).ok_or(Error::AmountOverflow)
    }
}

impl fmt::Display for Msat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 % 1000 {
            0 => write!(f, "{} sats", self.0 / 1000),
            frac => write!(f, "{}.{} sats", self.0 / 1000, format!("{frac:03}").trim_end_matches('0')),
        }
    }
}

/// The Bitcoin network a wallet is bound to. A wallet never mixes networks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Network {
    Mainnet,
    Testnet,
    /// Includes Mutinynet, which is a signet.
    Signet,
    Regtest,
}

/// Identifies one pocket, e.g. `cashu:https://cashu.mutinynet.com`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RailId(pub String);

impl fmt::Display for RailId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies one operation. Rails build it from ids the operator also knows
/// (a quote or operation id, a payment hash, a proof's Y), so the same
/// operation always gets the same id and can be looked up again after a crash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OpId(pub String);

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpKind {
    LnReceive,
    LnPay,
    NativeSend,
    NativeReceive,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::LnReceive => "ln_receive",
            OpKind::LnPay => "ln_pay",
            OpKind::NativeSend => "native_send",
            OpKind::NativeReceive => "native_receive",
        }
    }
}

impl FromStr for OpKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "ln_receive" => OpKind::LnReceive,
            "ln_pay" => OpKind::LnPay,
            "native_send" => OpKind::NativeSend,
            "native_receive" => OpKind::NativeReceive,
            other => return Err(Error::InvalidInput(format!("unknown operation kind {other}"))),
        })
    }
}

/// Where an operation stands, as the operator reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpStatus {
    /// Waiting on the other side: an invoice not yet paid, a payment in flight.
    Pending,
    /// Final: the money moved. `fee` is what the operator charged.
    Completed { amount: Msat, fee: Msat },
    /// Final: nothing reached the other side. `cost` is what the attempt
    /// still cost, e.g. a federation's fees on a refunded payment; usually zero.
    Failed { reason: String, cost: Msat },
    /// Final: an invoice or quote ran out before it was used; nothing moved.
    Expired,
}

impl OpStatus {
    /// Failed at no cost: nothing moved at all.
    pub fn failed(reason: impl Into<String>) -> OpStatus {
        OpStatus::Failed { reason: reason.into(), cost: Msat::ZERO }
    }

    pub fn is_final(&self) -> bool {
        !matches!(self, OpStatus::Pending)
    }

    pub fn label(&self) -> &'static str {
        match self {
            OpStatus::Pending => "PENDING",
            OpStatus::Completed { .. } => "COMPLETED",
            OpStatus::Failed { .. } => "FAILED",
            OpStatus::Expired => "EXPIRED",
        }
    }
}

/// Where a move between two pockets stands. A move is a Lightning payment
/// from one pocket (the source) to an invoice of the other (the destination),
/// and every state is saved before the next call to an operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentState {
    /// Created; nothing asked of any operator yet.
    Quoted,
    /// The destination issued an invoice and the source quoted paying it:
    /// waiting for the user to accept the fee.
    DestInvoiceCreated,
    /// Accepted; the payment is about to be recorded.
    Authorized,
    /// The payment is recorded and handed to the source.
    SourcePaying,
    /// The source paid; the destination still has to claim it.
    SourcePaid,
    /// The destination claimed it.
    DestClaimed,
    /// Final: the money moved.
    Completed,
    /// Final: nothing left the source.
    Failed,
    /// Final: the quote or invoice ran out before it was paid; nothing moved.
    Expired,
    /// The source paid but the destination can't claim: a person must look.
    NeedsAttention,
}

impl IntentState {
    pub const ALL: [IntentState; 10] = [
        IntentState::Quoted,
        IntentState::DestInvoiceCreated,
        IntentState::Authorized,
        IntentState::SourcePaying,
        IntentState::SourcePaid,
        IntentState::DestClaimed,
        IntentState::Completed,
        IntentState::Failed,
        IntentState::Expired,
        IntentState::NeedsAttention,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IntentState::Quoted => "QUOTED",
            IntentState::DestInvoiceCreated => "DEST_INVOICE_CREATED",
            IntentState::Authorized => "AUTHORIZED",
            IntentState::SourcePaying => "SOURCE_PAYING",
            IntentState::SourcePaid => "SOURCE_PAID",
            IntentState::DestClaimed => "DEST_CLAIMED",
            IntentState::Completed => "COMPLETED",
            IntentState::Failed => "FAILED",
            IntentState::Expired => "EXPIRED",
            IntentState::NeedsAttention => "NEEDS_ATTENTION",
        }
    }

    /// No further step will change it. `NeedsAttention` is not final: it is
    /// checked again on every start in case the destination recovers.
    pub fn is_final(self) -> bool {
        matches!(self, IntentState::Completed | IntentState::Failed | IntentState::Expired)
    }
}

impl FromStr for IntentState {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        IntentState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| Error::InvalidInput(format!("unknown intent state {s}")))
    }
}

impl fmt::Display for IntentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a rail can do. The router (Phase 4) only offers rails whose
/// capabilities match the destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub ln_send: bool,
    pub ln_receive: bool,
    pub native_send: bool,
    pub native_receive: bool,
    pub onchain_in: bool,
    pub onchain_out: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub rail: RailId,
    pub spendable: Msat,
    /// Locked in operations still in flight (e.g. a payment awaiting its result).
    pub pending: Msat,
    /// Unix seconds when the balance was read.
    pub as_of: u64,
    /// True when the operator could not be reached: the numbers are the last
    /// ones known locally.
    pub stale: bool,
}

/// A validated BOLT11 Lightning invoice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bolt11(lightning_invoice::Bolt11Invoice);

impl Bolt11 {
    pub fn parse(s: &str) -> Result<Self> {
        s.trim()
            .parse::<lightning_invoice::Bolt11Invoice>()
            .map(Bolt11)
            .map_err(|e| Error::InvalidInput(format!("not a valid Lightning invoice: {e}")))
    }

    /// The amount the invoice asks for. `None` for an amountless invoice.
    pub fn amount(&self) -> Option<Msat> {
        self.0.amount_milli_satoshis().map(Msat)
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at() <= now()
    }

    /// Hex payment hash: the same for every copy of the invoice, so it
    /// identifies the payment across processes and operators.
    pub fn payment_hash(&self) -> String {
        self.0.payment_hash().to_string()
    }

    /// Unix seconds when the invoice expires.
    pub fn expires_at(&self) -> u64 {
        self.0.expires_at().map(|d| d.as_secs()).unwrap_or(u64::MAX)
    }

    pub fn network(&self) -> Network {
        match self.0.currency() {
            lightning_invoice::Currency::Bitcoin => Network::Mainnet,
            lightning_invoice::Currency::BitcoinTestnet => Network::Testnet,
            lightning_invoice::Currency::Signet => Network::Signet,
            lightning_invoice::Currency::Regtest | lightning_invoice::Currency::Simnet => Network::Regtest,
        }
    }
}

impl fmt::Display for Bolt11 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An incoming payment the user asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiveOp {
    pub op: OpId,
    pub amount: Msat,
    /// The invoice to show the payer; `None` for a native receive.
    pub invoice: Option<String>,
    pub status: OpStatus,
}

/// A rail's offer to pay an invoice: what it will cost and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayQuote {
    pub rail: RailId,
    /// The operator's quote id.
    pub quote_id: String,
    pub invoice: Bolt11,
    pub amount: Msat,
    /// The most the payment can cost on top of `amount`; unused fee comes back.
    pub fee_reserve: Msat,
    /// Unix seconds.
    pub expires_at: u64,
}

impl PayQuote {
    pub fn op_id(&self) -> OpId {
        OpId(format!("{}:pay:{}", self.rail, self.quote_id))
    }

    /// The worst case leaving the wallet: amount plus the full fee reserve.
    pub fn max_total(&self) -> Result<Msat> {
        self.amount.checked_add(self.fee_reserve)
    }

    /// Refuses a quote whose fee reserve is above `cap`.
    pub fn check_fee_cap(&self, cap: Option<Msat>) -> Result<()> {
        match cap {
            Some(cap) if self.fee_reserve > cap => Err(Error::FeeCapExceeded { fee: self.fee_reserve, cap }),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayOp {
    pub op: OpId,
    pub status: OpStatus,
}

/// Bearer money handed over outside Lightning: a Cashu token, Fedimint notes,
/// an Ark out-of-round payment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeToken {
    pub op: OpId,
    /// What was handed over. Fedimint notes come in fixed denominations, so
    /// this can be a little more than was asked for.
    pub amount: Msat,
    /// What creating the token cost on top of `amount`.
    pub fee: Msat,
    pub token: String,
}

/// What `recover()` found and did on start-up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Interrupted operations finished or rolled back.
    pub resolved: usize,
    /// Paid invoices claimed while the app was down.
    pub claimed: Msat,
    /// Sent tokens or notes nobody redeemed, which came back to the wallet:
    /// the send's op id and the amount returned.
    pub reclaimed: Vec<(OpId, Msat)>,
    /// Money that moved without a call from INDIGO: a payment to this
    /// pocket's address, a board, a refresh's fee. Each is recorded once,
    /// by its op id.
    pub arrivals: Vec<Arrival>,
    /// Problems that need the user's attention.
    pub problems: Vec<String>,
}

/// See [`RecoveryReport::arrivals`]. `NativeReceive` adds `amount - fee` to
/// the balance; `NativeSend` takes `amount + fee` out of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub op: OpId,
    pub kind: OpKind,
    pub amount: Msat,
    pub fee: Msat,
    /// What it was, for people: "ark payment received", "refresh".
    pub detail: String,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("not enough funds: {available} available, {needed} needed")]
    InsufficientFunds { available: Msat, needed: Msat },
    #[error("fee {fee} is above the cap of {cap}")]
    FeeCapExceeded { fee: Msat, cap: Msat },
    #[error("this invoice or quote has expired")]
    Expired,
    #[error("this {what} is for {found:?}, but the wallet is on {expected:?}")]
    WrongNetwork { what: &'static str, expected: Network, found: Network },
    #[error("{rail} could not be reached: {reason}")]
    OperatorUnavailable { rail: RailId, reason: String },
    /// The operator answered, but with an error of its own.
    #[error("{rail} refused the request: {reason}")]
    OperatorError { rail: RailId, reason: String },
    #[error("already spent: this token or these notes were redeemed before")]
    AlreadySpent,
    #[error("amount too large")]
    AmountOverflow,
    #[error("unknown operation {0}")]
    UnknownOp(OpId),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Unix seconds now. Browsers have no system clock for Rust's `SystemTime`
/// (it panics there), so a browser build asks JavaScript's `Date`.
pub fn now() -> u64 {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msat_displays_in_sats() {
        assert_eq!(Msat::from_sats(2_000).to_string(), "2000 sats");
        assert_eq!(Msat(999).to_string(), "0.999 sats");
        assert_eq!(Msat(17_500).to_string(), "17.5 sats");
        assert_eq!(Msat(17_500).sats_floor(), 17);
    }
}
