//! The Ark pocket on Arkade (Ark Labs' `ark-client`), one operator each.
//! Lightning goes through Boltz swaps.
//!
//! Operation ids, matched with Arkade's records after a crash:
//!
//! - `<rail>:receive:<swap id>.<amount msat>` for Lightning receives (a Boltz
//!   reverse swap; its record keeps only the amount locked for us, so the
//!   invoice amount rides in the key)
//! - `<rail>:pay:<payment hash>.<quote expiry>` for Lightning payments (a
//!   submarine swap, found again by the invoice's payment hash)
//! - `<rail>:send:<destination hash>.<amount msat>.<unix seconds>` for Ark
//!   payments; the transaction made for it is linked to the op
//! - `<rail>:tx:<txid>` for every other change in Arkade's history (payments
//!   to our address, boards, settlements), reported by `recover` as arrivals

mod store;

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use ark_bdk_wallet::Wallet as OnchainWallet;
use ark_client::wallet::OnchainWallet as _;
use ark_client::{
    Blockchain, Client, OfflineClient, OfflineClientConfig, SpendStatus, SwapAmount, SwapStatus, TxStatus,
};
use ark_core::history::Transaction as HistoryTx;
use ark_core::send::SendReceiver;
use ark_core::{ArkAddress, ExplorerUtxo};
use bitcoin::bip32::Xpriv;
use bitcoin::secp256k1::rand::SeedableRng;
use bitcoin::secp256k1::rand::rngs::StdRng;
use bitcoin::{Address as BtcAddress, Amount, Network as BtcNetwork, Transaction, Txid};
use indigo_domain::{
    Arrival, BalanceSnapshot, Bolt11, Capabilities, Error, Msat, NativeToken, Network, OpId, OpKind, OpStatus,
    PayOp, PayQuote, RailId, ReceiveOp, RecoveryReport, Result, now,
};
use indigo_rail::{ArkOps, BeforeMove, ExitStatus, Expiry, Rail, Vtxo};
use sha2::Digest;

use crate::store::Store;

/// Where an Arkade pocket finds its operator, Boltz and the chain.
#[derive(Clone, Debug)]
pub struct ArkadeConfig {
    pub server: String,
    pub boltz: String,
    pub esplora: String,
}

type ArkClient = Client<Esplora, OnchainWallet, Store>;

pub struct ArkadeRail {
    id: RailId,
    network: Network,
    client: ArkClient,
    store: Arc<Store>,
    onchain: Arc<OnchainWallet>,
}

impl ArkadeRail {
    pub async fn open(config: &ArkadeConfig, network: Network, seed: [u8; 64], dir: &Path) -> Result<Self> {
        let id = RailId(format!("ark-arkade:{}", config.server.trim_end_matches('/')));
        let btc = btc_network(network);
        std::fs::create_dir_all(dir).map_err(|e| Error::Storage(format!("{id}: {e}")))?;
        let store = Arc::new(Store::open(&dir.join("arkade.sqlite")).map_err(|e| Error::Storage(format!("{id}: {e}")))?);
        let xpriv = Xpriv::new_master(btc, &seed).map_err(|e| Error::Other(format!("{id}: key: {e}")))?;
        let onchain = Arc::new(
            OnchainWallet::new_from_xpriv(xpriv, btc, &config.esplora)
                .map_err(|e| Error::Other(format!("{id}: on-chain wallet: {e}")))?,
        );
        let blockchain = Arc::new(Esplora::new(&config.esplora)?);
        let offline = OfflineClient::with_bip32(
            OfflineClientConfig {
                ark_server_url: config.server.clone(),
                boltz_url: config.boltz.clone(),
                ..Default::default()
            },
            xpriv,
            None,
            blockchain,
            onchain.clone(),
            store.clone(),
        );
        let client = offline
            .connect()
            .await
            .map_err(|e| Error::OperatorUnavailable { rail: id.clone(), reason: e.to_string() })?;
        Ok(Self { id, network, client, store, onchain })
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

    fn err(&self, e: impl std::fmt::Display) -> Error {
        let reason = e.to_string();
        let lower = reason.to_lowercase();
        if ["connect", "unreachable", "timed out", "timeout", "transport", "unavailable", "dns"]
            .iter()
            .any(|w| lower.contains(w))
        {
            Error::OperatorUnavailable { rail: self.id.clone(), reason }
        } else {
            Error::OperatorError { rail: self.id.clone(), reason }
        }
    }

    async fn history(&self) -> Result<Vec<HistoryTx>> {
        self.client.transaction_history().await.map_err(|e| self.err(e))
    }

    async fn ln_pay_status(&self, key: &str) -> Result<OpStatus> {
        let (hash, expiry) = key.split_once('.').ok_or_else(|| Error::InvalidInput(format!("bad pay key {key}")))?;
        let swaps = self.store.all_submarine().map_err(|e| self.err(e))?;
        let Some(swap) = swaps.iter().find(|s| s.invoice.payment_hash().to_string() == hash) else {
            // Arkade stores the swap before funding it.
            let expiry: u64 = expiry.parse().map_err(|_| Error::InvalidInput(format!("bad pay key {key}")))?;
            return Ok(if now() > expiry { OpStatus::failed("never paid; nothing moved") } else { OpStatus::Pending });
        };
        let status = self.client.get_swap_status(&swap.id).await.map_err(|e| self.err(e))?.status;
        let amount = Msat(swap.invoice.amount_milli_satoshis().unwrap_or(0));
        let funded = msat(swap.amount);
        Ok(match status {
            SwapStatus::TransactionClaimed | SwapStatus::InvoicePaid => {
                OpStatus::Completed { amount, fee: Msat(funded.0.saturating_sub(amount.0)) }
            }
            SwapStatus::InvoiceFailedToPay | SwapStatus::TransactionRefunded | SwapStatus::SwapExpired => {
                // Funded but not paid: the VHTLC refunds to us, less nothing.
                OpStatus::failed(format!("Boltz could not pay ({status:?}); refunded"))
            }
            _ => OpStatus::Pending,
        })
    }

    async fn receive_status(&self, key: &str) -> Result<OpStatus> {
        let (swap_id, amount) = key.split_once('.').ok_or_else(|| Error::InvalidInput(format!("bad receive key {key}")))?;
        let amount = Msat(amount.parse().map_err(|_| Error::InvalidInput(format!("bad receive key {key}")))?);
        let swap = self.store.get_reverse_sync(swap_id).map_err(|e| self.err(e))?.ok_or_else(|| {
            Error::InvalidInput(format!("{}: no swap {swap_id}", self.id))
        })?;
        let status = self.client.get_swap_status(swap_id).await.map_err(|e| self.err(e))?.status;
        match status {
            // Boltz locked the funds for us: claim them (this pocket's step).
            SwapStatus::TransactionMempool | SwapStatus::TransactionConfirmed => {
                let preimage = swap.preimage.ok_or_else(|| Error::Other(format!("{}: swap without preimage", self.id)))?;
                self.client.claim_vhtlc(swap_id, preimage).await.map_err(|e| self.err(e))?;
                Ok(OpStatus::Completed { amount, fee: Msat(amount.0.saturating_sub(msat(swap.amount).0)) })
            }
            SwapStatus::InvoiceSettled => {
                Ok(OpStatus::Completed { amount, fee: Msat(amount.0.saturating_sub(msat(swap.amount).0)) })
            }
            SwapStatus::SwapExpired | SwapStatus::TransactionFailed | SwapStatus::InvoiceExpired => Ok(OpStatus::Expired),
            _ => Ok(OpStatus::Pending),
        }
    }

    async fn send_status(&self, op: &OpId, key: &str) -> Result<OpStatus> {
        let at: u64 = key.rsplit('.').next().and_then(|t| t.parse().ok()).unwrap_or(0);
        for tx in self.history().await? {
            if let HistoryTx::Ark { txid, amount, .. } = &tx
                && self.store.linked(&txid.to_string()).map_err(|e| self.err(e))?.as_deref() == Some(&op.0)
            {
                let out = Msat(amount.unsigned_abs().to_sat() * 1000);
                return Ok(OpStatus::Completed { amount: out, fee: Msat::ZERO });
            }
        }
        // Not linked: the app stopped before Arkade answered. Arkade has no
        // checkpoint of its own to ask, so after a minute it counts as never sent.
        Ok(if now() > at + 60 { OpStatus::failed("never sent; nothing moved") } else { OpStatus::Pending })
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

fn whole_sats(amount: Msat) -> Result<Amount> {
    match amount.0 % 1000 {
        0 => Ok(Amount::from_sat(amount.sats_floor())),
        _ => Err(Error::InvalidInput(format!("{amount}: Ark counts whole sats"))),
    }
}

fn hash_hex(s: &str) -> String {
    sha2::Sha256::digest(s.as_bytes())[..12].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Rail for ArkadeRail {
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
        let b = self.client.offchain_balance().await.map_err(|e| self.err(e))?;
        Ok(BalanceSnapshot {
            rail: self.id.clone(),
            spendable: msat(b.confirmed()),
            pending: msat(b.total() - b.confirmed()),
            as_of: now(),
            stale: false,
        })
    }

    async fn ln_receive(&self, amount: Msat, memo: &str) -> Result<ReceiveOp> {
        let memo = (!memo.is_empty()).then(|| memo.to_string());
        let swap = self
            .client
            .get_ln_invoice(SwapAmount::Invoice(whole_sats(amount)?), None, memo)
            .await
            .map_err(|e| self.err(e))?;
        Ok(ReceiveOp {
            op: self.op("receive", &format!("{}.{}", swap.swap_id, amount.0)),
            amount,
            invoice: Some(swap.invoice.to_string()),
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
        let fees = self.client.get_fees().await.map_err(|e| self.err(e))?;
        // Boltz charges a percentage plus a fixed miner fee on submarine swaps.
        let sats = amount.0.div_ceil(1000);
        let fee = (sats as f64 * fees.submarine.percentage / 100.0).ceil() as u64 + fees.submarine.miner_fees;
        let expires_at = invoice.expires_at();
        Ok(PayQuote {
            rail: self.id.clone(),
            quote_id: format!("{}.{expires_at}", invoice.payment_hash()),
            invoice: invoice.clone(),
            amount,
            fee_reserve: Msat::from_sats(fee),
            expires_at,
        })
    }

    async fn ln_pay(&self, quote: PayQuote) -> Result<PayOp> {
        let op = quote.op_id();
        let invoice = ark_client::lightning_invoice::Bolt11Invoice::from_str(&quote.invoice.to_string())
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        let status = match self.client.pay_ln_invoice(invoice).await {
            Ok(_) => self.ln_pay_status(&quote.quote_id).await.unwrap_or(OpStatus::Pending),
            Err(e) => match self.err(e) {
                Error::OperatorUnavailable { .. } => OpStatus::Pending,
                e => return Err(e),
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
        let (address, _) = self.client.get_offchain_address().await.map_err(|e| self.err(e))?;
        Ok(address.encode())
    }

    async fn native_pay(&self, address: &str, amount: Msat, before: BeforeMove<'_>) -> Result<PayOp> {
        let dest = ArkAddress::decode(address.trim()).map_err(|e| Error::InvalidInput(format!("not an Ark address: {e}")))?;
        let op = self.op("send", &format!("{}.{}.{}", hash_hex(address.trim()), amount.0, now()));
        before(&op, amount)?;
        let receiver = SendReceiver { address: dest, amount: whole_sats(amount)?, assets: Vec::new() };
        let status = match self.client.send(vec![receiver]).await {
            Ok(txid) => {
                self.store.link(&txid.to_string(), &op.0).map_err(|e| self.err(e))?;
                OpStatus::Completed { amount, fee: Msat::ZERO }
            }
            Err(e) => match self.err(e) {
                Error::OperatorUnavailable { .. } => OpStatus::Pending,
                e => return Err(e),
            },
        };
        Ok(PayOp { op, status })
    }

    async fn op_status(&self, op: &OpId) -> Result<OpStatus> {
        let (kind, key) = self.parse_op(op)?;
        match kind {
            "receive" => self.receive_status(key).await,
            "pay" => self.ln_pay_status(key).await,
            "send" => self.send_status(op, key).await,
            "tx" => Err(Error::InvalidInput(format!("{op} is recorded complete when found"))),
            _ => Err(Error::UnknownOp(op.clone())),
        }
    }

    /// Reports every change in Arkade's history that no INDIGO operation
    /// accounts for: payments to our address, boards, settlements.
    async fn recover(&self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        if let Err(e) = self.onchain.sync().await {
            report.problems.push(format!("{}: on-chain wallet not synced: {e}", self.id));
        }
        for tx in self.history().await? {
            // Boarding entries are the on-chain deposit; it reaches the Ark
            // balance through a settlement (a commitment), counted there.
            // `is_settled` on an Ark transaction only says whether our
            // outputs in it were spent since: the change counts either way.
            let (txid, change, detail) = match &tx {
                HistoryTx::Ark { txid, amount, .. } => (txid.to_string(), *amount, "ark payment"),
                HistoryTx::Commitment { txid, amount, .. } => (txid.to_string(), *amount, "settlement"),
                HistoryTx::Offboard { commitment_txid, amount, .. } => {
                    (format!("offboard-{commitment_txid}"), -amount.to_signed().unwrap_or_default(), "offboard")
                }
                HistoryTx::Boarding { .. } => continue,
            };
            if self.store.linked(&txid).map_err(|e| self.err(e))?.is_some() {
                continue;
            }
            let sats = change.to_sat();
            report.arrivals.push(Arrival {
                op: self.op("tx", &txid),
                kind: if sats >= 0 { OpKind::NativeReceive } else { OpKind::NativeSend },
                amount: Msat(sats.unsigned_abs() * 1000),
                fee: Msat::ZERO,
                detail: detail.into(),
            });
        }
        Ok(report)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ArkOps for ArkadeRail {
    async fn onchain_address(&self) -> Result<String> {
        Ok(self.client.get_boarding_address().await.map_err(|e| self.err(e))?.to_string())
    }

    async fn onchain_balance(&self) -> Result<Msat> {
        let b = self.client.offchain_balance().await.map_err(|e| self.err(e))?;
        Ok(msat(b.total()))
    }

    /// Arkade boards whatever sits at the boarding address in the next
    /// settlement; `amount` is not chosen separately.
    async fn board(&self, _amount: Option<Msat>) -> Result<String> {
        let mut rng = StdRng::from_entropy();
        match self.client.settle(&mut rng).await.map_err(|e| self.err(e))? {
            Some(txid) => Ok(txid.to_string()),
            None => Err(Error::InvalidInput(format!("{}: nothing to board yet", self.id))),
        }
    }

    async fn send_onchain(&self, address: &str, amount: Msat) -> Result<String> {
        let address = BtcAddress::from_str(address.trim())
            .map_err(|e| Error::InvalidInput(format!("not a Bitcoin address: {e}")))?
            .require_network(btc_network(self.network))
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        let txid = self.client.send_on_chain(address, whole_sats(amount)?).await.map_err(|e| self.err(e))?;
        Ok(txid.to_string())
    }

    async fn vtxos(&self) -> Result<Vec<Vtxo>> {
        let list = self.client.list_vtxos().await.map_err(|e| self.err(e))?;
        Ok(list
            .all()
            .map(|v| Vtxo {
                id: v.vtxo().outpoint.to_string(),
                amount: msat(v.vtxo().amount),
                expiry: Expiry::Time(v.vtxo().expires_at.max(0) as u64),
                state: if v.vtxo().is_spent { "spent".into() } else { "spendable".into() },
            })
            .collect())
    }

    /// Without `all`, Arkade's `settle`: only expired or recoverable VTXOs
    /// (and boarding outputs). With `all`, `settle_all`: every VTXO.
    async fn refresh(&self, all: bool) -> Result<Option<String>> {
        let mut rng = StdRng::from_entropy();
        let settled =
            if all { self.client.settle_all(&mut rng).await } else { self.client.settle(&mut rng).await };
        Ok(settled.map_err(|e| self.err(e))?.map(|t| t.to_string()))
    }

    async fn exit_start(&self) -> Result<usize> {
        let trees = self.client.build_unilateral_exit_trees().await.map_err(|e| self.err(e))?;
        for branch in &trees {
            self.client.broadcast_next_unilateral_exit_node(branch).await.map_err(|e| self.err(e))?;
        }
        Ok(trees.len())
    }

    async fn exit_progress(&self) -> Result<ExitStatus> {
        let trees = self.client.build_unilateral_exit_trees().await.map_err(|e| self.err(e))?;
        let mut pending = 0;
        for branch in &trees {
            if self.client.broadcast_next_unilateral_exit_node(branch).await.map_err(|e| self.err(e))?.is_some() {
                pending += 1;
            }
        }
        Ok(ExitStatus {
            in_progress: pending,
            claimable: Msat::ZERO,
            detail: "exit transactions broadcast one level at a time; claim once the timelock passes".into(),
        })
    }

    async fn exit_claim(&self, address: &str) -> Result<String> {
        let address = BtcAddress::from_str(address.trim())
            .map_err(|e| Error::InvalidInput(format!("not a Bitcoin address: {e}")))?
            .require_network(btc_network(self.network))
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        let balance = self.client.offchain_balance().await.map_err(|e| self.err(e))?;
        let txid = self.client.send_on_chain(address, balance.total()).await.map_err(|e| self.err(e))?;
        Ok(txid.to_string())
    }
}

/// The chain through an Esplora HTTP API, as Arkade needs it.
pub struct Esplora {
    client: esplora_client::AsyncClient,
}

impl Esplora {
    fn new(url: &str) -> Result<Self> {
        let client = esplora_client::Builder::new(url)
            .build_async()
            .map_err(|e| Error::Other(format!("esplora: {e}")))?;
        Ok(Self { client })
    }
}

fn ark_err(e: impl std::fmt::Display) -> ark_client::Error {
    ark_client::Error::consumer(e.to_string())
}

impl Blockchain for Esplora {
    async fn find_outpoints(&self, address: &BtcAddress) -> std::result::Result<Vec<ExplorerUtxo>, ark_client::Error> {
        let script = address.script_pubkey();
        let txs = self.client.scripthash_txs(&script, None).await.map_err(ark_err)?;
        let mut out = Vec::new();
        for tx in txs {
            for (vout, output) in tx.vout.iter().enumerate() {
                if output.scriptpubkey != script {
                    continue;
                }
                let spent = self
                    .client
                    .get_output_status(&tx.txid, vout as u64)
                    .await
                    .map_err(ark_err)?
                    .is_some_and(|s| s.spent);
                out.push(ExplorerUtxo {
                    outpoint: bitcoin::OutPoint::new(tx.txid, vout as u32),
                    amount: Amount::from_sat(output.value),
                    confirmation_blocktime: tx.status.block_time,
                    confirmations: u64::from(tx.status.confirmed),
                    is_spent: spent,
                });
            }
        }
        Ok(out)
    }

    async fn find_tx(&self, txid: &Txid) -> std::result::Result<Option<Transaction>, ark_client::Error> {
        self.client.get_tx(txid).await.map_err(ark_err)
    }

    async fn get_tx_status(&self, txid: &Txid) -> std::result::Result<TxStatus, ark_client::Error> {
        let status = self.client.get_tx_status(txid).await.map_err(ark_err)?;
        Ok(TxStatus { confirmed_at: status.block_time.map(|t| t as i64) })
    }

    async fn get_output_status(&self, txid: &Txid, vout: u32) -> std::result::Result<SpendStatus, ark_client::Error> {
        let status = self.client.get_output_status(txid, vout as u64).await.map_err(ark_err)?;
        Ok(SpendStatus { spend_txid: status.and_then(|s| s.txid) })
    }

    async fn broadcast(&self, tx: &Transaction) -> std::result::Result<(), ark_client::Error> {
        self.client.broadcast(tx).await.map_err(ark_err)
    }

    async fn get_fee_rate(&self) -> std::result::Result<f64, ark_client::Error> {
        let estimates = self.client.get_fee_estimates().await.map_err(ark_err)?;
        Ok(estimates.get(&1).copied().unwrap_or(1.0))
    }

    async fn broadcast_package(&self, txs: &[&Transaction]) -> std::result::Result<(), ark_client::Error> {
        for tx in txs {
            self.client.broadcast(tx).await.map_err(ark_err)?;
        }
        Ok(())
    }
}
