//! The Rail trait every protocol pocket implements.
//!
//! Each protocol (Cashu, Fedimint, Ark, on-chain) is one adapter behind this
//! trait. Everything above it (the engine, the router, the CLI and the
//! platform bindings) only ever talks to `dyn Rail`.
//!
//! Browser futures are not `Send`, so on `wasm32` the trait drops the
//! `Send + Sync` requirement. Code written against `MaybeSend` works on both.

use indigo_domain::{
    BalanceSnapshot, Bolt11, Capabilities, Error, Msat, NativeToken, OpId, OpStatus, PayOp, PayQuote, RailId,
    ReceiveOp, RecoveryReport, Result,
};

/// Called by `native_send` / `native_receive` once the operation's id is
/// known and before any money moves, so the caller can record it first.
/// An error cancels the operation.
pub type BeforeMove<'a> = &'a (dyn Fn(&OpId, Msat) -> Result<()> + Sync);

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSend for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Rail: MaybeSend {
    /// e.g. `cashu:https://cashu.mutinynet.com`
    fn id(&self) -> RailId;

    fn capabilities(&self) -> Capabilities;

    /// The balance as the rail knows it. If the operator can't be reached, the
    /// last known balance comes back marked `stale` instead of an error.
    async fn balance(&self) -> Result<BalanceSnapshot>;

    /// Ask the operator for a Lightning invoice to receive `amount`.
    async fn ln_receive(&self, amount: Msat, memo: &str) -> Result<ReceiveOp>;

    /// What paying `invoice` would cost, before anything is paid.
    async fn ln_quote(&self, invoice: &Bolt11) -> Result<PayQuote>;

    /// Pay a quote from `ln_quote`. Returns once the payment is final or still
    /// in flight (`Pending`); `op_status` and `recover` take it from there.
    async fn ln_pay(&self, quote: PayQuote) -> Result<PayOp>;

    /// Hand over bearer money outside Lightning (a Cashu token, Fedimint notes).
    async fn native_send(&self, amount: Msat, before: BeforeMove<'_>) -> Result<NativeToken>;

    /// Redeem bearer money someone handed over.
    async fn native_receive(&self, token: &str, before: BeforeMove<'_>) -> Result<ReceiveOp>;

    /// An address another wallet of the same kind pays into directly (an Ark
    /// address). Rails without addresses (Cashu, Fedimint) don't have one.
    async fn native_address(&self) -> Result<String> {
        Err(Error::InvalidInput(format!("{} has no native addresses", self.id())))
    }

    /// Pays a native address (Ark to Ark), recording through `before` first.
    async fn native_pay(&self, address: &str, amount: Msat, before: BeforeMove<'_>) -> Result<PayOp> {
        let _ = (address, amount, before);
        Err(Error::InvalidInput(format!("{} has no native addresses", self.id())))
    }

    /// The operation's status, asked of the operator: local state alone is
    /// never trusted. A receive that is paid but not yet claimed is claimed
    /// here, so it comes back `Completed`.
    async fn op_status(&self, op: &OpId) -> Result<OpStatus>;

    /// Finish or roll back whatever a crash interrupted. Runs on every start.
    async fn recover(&self) -> Result<RecoveryReport>;

    /// On a new device, finds what this pocket's seed owns at its operator
    /// (restoring from the 12 words). Pockets that do this on open, or not
    /// at all, keep the default.
    async fn restore(&self) -> Result<()> {
        Ok(())
    }
}

/// One VTXO: a piece of Ark balance, and when it must be refreshed (or
/// exited) by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vtxo {
    pub id: String,
    pub amount: Msat,
    pub expiry: Expiry,
    pub state: String,
}

/// Bark VTXOs expire at a block height, Arkade's at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    Height(u32),
    /// Unix seconds.
    Time(u64),
}

/// Where a unilateral exit stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    /// VTXOs still exiting.
    pub in_progress: usize,
    /// Exited and claimable on chain.
    pub claimable: Msat,
    /// What still needs to happen, for people.
    pub detail: String,
}

/// What only an Ark pocket can do: board on-chain coins, refresh VTXOs before
/// they expire, and exit to the chain without the server.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ArkOps: Rail {
    /// An on-chain address of this pocket's own wallet, to fund boarding.
    async fn onchain_address(&self) -> Result<String>;
    async fn onchain_balance(&self) -> Result<Msat>;
    /// Moves on-chain coins into Ark (all of them if `amount` is `None`).
    /// Returns the funding transaction id; the balance appears once the
    /// server accepts the board.
    async fn board(&self, amount: Option<Msat>) -> Result<String>;
    /// Sends Ark balance to an on-chain address with the server's help (an
    /// offboard); returns the transaction id.
    async fn send_onchain(&self, address: &str, amount: Msat) -> Result<String>;
    async fn vtxos(&self) -> Result<Vec<Vtxo>>;
    /// Refreshes the VTXOs that need it (all of them with `all`) in the next
    /// round; `None` if none did.
    async fn refresh(&self, all: bool) -> Result<Option<String>>;
    /// Starts a unilateral exit of every VTXO, without the server.
    async fn exit_start(&self) -> Result<usize>;
    /// Pushes exits along (broadcasts the next transactions) and reports.
    async fn exit_progress(&self) -> Result<ExitStatus>;
    /// Sends exited coins to an on-chain address; returns the txid.
    async fn exit_claim(&self, address: &str) -> Result<String>;
}
