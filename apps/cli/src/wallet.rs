//! Wallet commands: balance, receive, pay, send-token, receive-token, join,
//! move, intents, ops, audit.
//! Every command but `ops` runs `recover()` first; `ops` only reads the log.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Subcommand;
use indigo_core::{ArkadeConfig, BarkChain, BarkConfig, Expiry, IntentRecord, OpRecord, StartupReport, Wallet, WalletConfig};
use indigo_domain::{IntentState, Msat, OpStatus};
use indigo_keys::Seed;

use crate::network::{BarkChainSource, Network};

#[derive(Subcommand)]
pub enum WalletCommand {
    /// Balance of every pocket (stale if its operator can't be reached).
    Balance,
    /// Create a Lightning invoice and wait for it to be paid.
    Receive {
        amount: u64,
        #[arg(long, default_value = "")]
        memo: String,
        /// Seconds to wait for payment before leaving it to the next start.
        #[arg(long, default_value_t = 120)]
        wait: u64,
    },
    /// Pay a Lightning invoice: shows the fee quote first.
    Pay {
        invoice: String,
        /// Refuse if the fee reserve is above this many sats.
        #[arg(long)]
        max_fee: Option<u64>,
        /// Pay without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Create an ecash token for someone else to redeem.
    SendToken { amount: u64 },
    /// Redeem an ecash token.
    ReceiveToken { token: String },
    /// Join a Fedimint federation: shows what it is first.
    Join {
        invite: String,
        /// Join without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Move sats from one pocket to another over Lightning: shows the fee first.
    Move {
        /// Source pocket: `cashu`, `fedimint`, or a full pocket id.
        #[arg(long)]
        from: String,
        /// Destination pocket.
        #[arg(long)]
        to: String,
        amount: u64,
        /// Idempotency key: running the same move again with it never moves twice.
        #[arg(long)]
        key: Option<String>,
        /// Move without asking.
        #[arg(long)]
        yes: bool,
        /// Seconds to wait for the destination to claim the payment.
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
    /// Every move between pockets and where it stands.
    Intents,
    /// Pay a Lightning invoice or an Ark address from whichever pocket covers
    /// it, topping one up from the others if none does alone.
    Send {
        /// The invoice or address.
        to: String,
        /// Sats, for an Ark address.
        amount: Option<u64>,
        /// Pay only from this pocket.
        #[arg(long)]
        from: Option<String>,
    },
    /// Create ecash for someone else (Cashu or Fedimint, picked automatically).
    SendEcash {
        amount: u64,
        /// cashu or fedimint
        #[arg(long)]
        from: Option<String>,
    },
    /// On a new device: bring the wallet back from its 12 words (this
    /// wallet folder must be new). Rejoins the federation by rescanning it.
    Restore {
        /// The federation the wallet was in.
        #[arg(long)]
        federation: Option<String>,
    },
    /// Redeem a Cashu token or Fedimint notes into the matching pocket.
    Redeem { token: String },
    /// A Lightning invoice into the pocket the wallet picks; waits for payment.
    Request {
        amount: u64,
        #[arg(long)]
        into: Option<String>,
        #[arg(long, default_value_t = 120)]
        wait: u64,
    },
    /// An address to be paid at directly (Ark pockets).
    Address,
    /// Pay an address directly (Ark to Ark).
    PayAddress { address: String, amount: u64 },
    /// What only Ark pockets do: boarding, VTXOs, refresh, exit.
    #[command(subcommand)]
    Ark(ArkCommand),
    /// Every operation and its status.
    Ops,
    /// Compare each pocket's balance with its journal, to the sat.
    Audit,
}

#[derive(Subcommand)]
pub enum ArkCommand {
    /// The pocket's own on-chain address and balance, to fund boarding.
    Onchain,
    /// Move on-chain coins into Ark (all of them without an amount).
    Board { amount: Option<u64> },
    /// Send Ark balance to an on-chain address (an offboard).
    SendOnchain { address: String, amount: u64 },
    /// Every VTXO with its expiry height.
    Vtxos,
    /// Refresh the VTXOs that need it in the next round.
    Refresh {
        /// Refresh every VTXO, not only those close to expiry.
        #[arg(long)]
        all: bool,
    },
    /// Exit to the chain without the server: start, then progress, then claim.
    Exit {
        #[command(subcommand)]
        step: ExitStep,
    },
}

#[derive(Subcommand)]
pub enum ExitStep {
    Start,
    Progress,
    /// Send exited coins to an on-chain address.
    Claim { address: String },
}

pub struct Options {
    pub network: Network,
    pub data_dir: Option<PathBuf>,
    pub rail: String,
    pub mint: Option<String>,
}

pub async fn run(opts: Options, command: WalletCommand) -> anyhow::Result<()> {
    let seed = Seed::from_test_env()?;
    let config = WalletConfig {
        network: opts.network.bitcoin(),
        data_dir: match opts.data_dir {
            Some(dir) => dir,
            None => default_data_dir(opts.network)?,
        },
        cashu_mint: opts.mint.or_else(|| opts.network.endpoints().cashu_mint.map(String::from)),
        bark: opts.network.endpoints().bark.map(|(server, chain)| BarkConfig {
            server: server.to_string(),
            chain: match chain {
                BarkChainSource::Esplora(url) => BarkChain::Esplora(url.to_string()),
                BarkChainSource::Bitcoind(url) => BarkChain::Bitcoind {
                    url: url.to_string(),
                    user: "indigo".into(),
                    pass: "indigo".into(),
                },
            },
        }),
        arkade: opts.network.endpoints().arkade_pocket.map(|(server, boltz, esplora)| ArkadeConfig {
            server: server.to_string(),
            boltz: boltz.to_string(),
            esplora: esplora.to_string(),
        }),
    };
    let wallet = Wallet::open(&config, &seed).await?;
    if !matches!(command, WalletCommand::Ops | WalletCommand::Intents) {
        print_recovery(&wallet.recover().await?);
    }
    let result = command_run(&wallet, &opts.rail, command).await;
    wallet.close().await?;
    result
}

async fn command_run(wallet: &Wallet, rail: &str, command: WalletCommand) -> anyhow::Result<()> {
    match command {
        WalletCommand::Balance => {
            for balance in wallet.balances().await {
                let b = balance?;
                let stale = if b.stale { "  [stale: operator unreachable, last known balance]" } else { "" };
                println!("{:<45} {:>12} spendable, {} pending{stale}", b.rail.0, sats(b.spendable), sats(b.pending));
            }
        }
        WalletCommand::Receive { amount, memo, wait } => {
            let op = wallet.receive(rail, Msat::from_sats(amount), &memo).await?;
            println!("invoice: {}", op.invoice.as_deref().unwrap_or("-"));
            println!("op:      {}", op.op);
            let status = wait_final(wallet, &op.op, wait).await?;
            match status {
                OpStatus::Completed { amount, .. } => println!("received {amount}"),
                OpStatus::Pending => println!("not paid yet: if it is paid later, it is claimed on the next start"),
                other => println!("not received: {other:?}"),
            }
        }
        WalletCommand::Pay { invoice, max_fee, yes } => {
            let quote = wallet.quote(rail, &invoice).await?;
            println!("amount:      {}", quote.amount);
            println!("fee reserve: {} (unused fee comes back)", quote.fee_reserve);
            println!("most it can cost: {}", quote.max_total()?);
            // Refuse before asking, with the same rule `pay` applies.
            quote.check_fee_cap(max_fee.map(Msat::from_sats)).context("--max-fee: not paying")?;
            if !yes && !confirm("Pay?")? {
                println!("not paid");
                return Ok(());
            }
            let paid = wallet.pay(quote, max_fee.map(Msat::from_sats)).await?;
            match paid.status {
                OpStatus::Completed { amount, fee } => println!("paid {amount}, fee {fee}"),
                OpStatus::Pending => println!("payment in flight ({}); its result is recorded on the next start", paid.op),
                OpStatus::Failed { reason, cost } if cost > Msat::ZERO => {
                    bail!("payment failed: {reason} (nothing reached the payee; the attempt cost {cost})")
                }
                other => bail!("payment failed: {other:?} (nothing was taken)"),
            }
        }
        WalletCommand::Send { to, amount, from } => {
            let report = wallet.send(&to, amount.map(Msat::from_sats), from.as_deref()).await?;
            print_send(&report);
        }
        WalletCommand::SendEcash { amount, from } => {
            let report = wallet.send_ecash(Msat::from_sats(amount), from.as_deref()).await?;
            print_send(&report);
            println!("{}", report.token.map(|t| t.token).unwrap_or_default());
        }
        WalletCommand::Restore { federation } => {
            if let Some(invite) = federation {
                wallet.restore_federation(&invite, Duration::from_secs(300)).await?;
            }
            let report = wallet.restore().await?;
            for p in &report.problems {
                eprintln!("restore: {p}");
            }
            for b in wallet.balances().await.into_iter().flatten() {
                println!("{:<45} {:>12} restored", b.rail.0, sats(b.spendable.checked_add(b.pending)?));
            }
        }
        WalletCommand::Redeem { token } => {
            let op = wallet.redeem(&token).await?;
            println!("received {} into {}", op.amount, op.op.0.split(':').next().unwrap_or_default());
        }
        WalletCommand::Request { amount, into, wait } => {
            let op = wallet.receive_auto(Msat::from_sats(amount), into.as_deref(), "").await?;
            println!("into:    {}", op.op.0.split(':').next().unwrap_or_default());
            println!("invoice: {}", op.invoice.as_deref().unwrap_or("-"));
            match wait_final(wallet, &op.op, wait).await? {
                OpStatus::Completed { amount, .. } => println!("received {amount}"),
                other => println!("not received yet: {}", other.label()),
            }
        }
        WalletCommand::SendToken { amount } => {
            let token = wallet.send_token(rail, Msat::from_sats(amount)).await?;
            println!("{}", token.token);
        }
        WalletCommand::ReceiveToken { token } => {
            let op = wallet.receive_token(rail, &token).await?;
            println!("received {}", op.amount);
        }
        WalletCommand::Join { invite, yes } => {
            let preview = wallet.preview_federation(&invite).await?;
            println!("federation: {}", preview.name.as_deref().unwrap_or("(unnamed)"));
            println!("network:    {:?}", preview.network);
            println!("guardians:  {}", preview.guardians);
            println!("modules:    {}", preview.modules.join(", "));
            if !yes && !confirm("Join?")? {
                println!("not joined");
                return Ok(());
            }
            println!("joined: {}", wallet.join_federation(&invite).await?);
        }
        WalletCommand::Move { from, to, amount, key, yes, wait } => {
            let started = wallet.start_move(&from, &to, Msat::from_sats(amount), key.as_deref()).await?;
            print_move(&started);
            if started.state != IntentState::DestInvoiceCreated {
                // A used key: that move, as it stands.
                return Ok(());
            }
            let fee = started.fee_reserve.unwrap_or_default();
            println!("fee reserve: {} (unused fee comes back)", fee);
            println!("most it can cost: {}", started.amount.checked_add(fee)?);
            if !yes && !confirm("Move?")? {
                println!("not moved; the quote expires on its own");
                return Ok(());
            }
            let done = wallet.confirm_move(&started.id, Duration::from_secs(wait)).await?;
            print_move(&done);
            match done.state {
                IntentState::Completed => {}
                IntentState::Failed | IntentState::Expired => bail!("not moved: nothing left {from}"),
                _ => println!("still in flight; it is finished on the next start"),
            }
        }
        WalletCommand::Intents => {
            for intent in wallet.moves()? {
                print_move(&intent);
            }
        }
        WalletCommand::Address => println!("{}", wallet.native_address(rail).await?),
        WalletCommand::PayAddress { address, amount } => {
            let paid = wallet.pay_address(rail, &address, Msat::from_sats(amount)).await?;
            match paid.status {
                OpStatus::Completed { amount, fee } => println!("paid {amount}, fee {fee}"),
                OpStatus::Pending => println!("payment in flight ({}); its result is recorded on the next start", paid.op),
                other => bail!("payment failed: {other:?}"),
            }
        }
        WalletCommand::Ark(command) => {
            // `ark` commands use the Ark pocket named by --rail, or Bark.
            let ark = wallet.ark(if rail.starts_with("ark") { rail } else { "ark" })?;
            match command {
                ArkCommand::Onchain => {
                    println!("address: {}", ark.onchain_address().await?);
                    println!("balance: {}", ark.onchain_balance().await?);
                }
                ArkCommand::Board { amount } => {
                    println!("boarding: funding tx {}", ark.board(amount.map(Msat::from_sats)).await?);
                }
                ArkCommand::SendOnchain { address, amount } => {
                    println!("sent: tx {}", ark.send_onchain(&address, Msat::from_sats(amount)).await?);
                }
                ArkCommand::Vtxos => {
                    for v in ark.vtxos().await? {
                        let expiry = match v.expiry {
                            Expiry::Height(h) => format!("block {h}"),
                            Expiry::Time(t) => format!("unix time {t}"),
                        };
                        println!("{:<12} {:>10}  expires at {expiry}  {}", v.state, sats(v.amount), v.id);
                    }
                }
                ArkCommand::Refresh { all } => match ark.refresh(all).await? {
                    Some(round) => println!("refreshed: {round}"),
                    None => println!("nothing needs refreshing"),
                },
                ArkCommand::Exit { step } => match step {
                    ExitStep::Start => println!("exiting {} VTXOs", ark.exit_start().await?),
                    ExitStep::Progress => {
                        let s = ark.exit_progress().await?;
                        println!("{} exiting, {} claimable; {}", s.in_progress, s.claimable, s.detail);
                    }
                    ExitStep::Claim { address } => println!("claimed: tx {}", ark.exit_claim(&address).await?),
                },
            }
        }
        WalletCommand::Ops => {
            for op in wallet.ops()? {
                print_op(&op);
            }
        }
        WalletCommand::Audit => {
            let mut ok = true;
            for a in wallet.audit().await? {
                let verdict = match (a.settled(), a.matches()) {
                    (false, _) => "IN FLIGHT: compare again once settled",
                    (true, true) => "OK",
                    (true, false) => "MISMATCH",
                };
                ok &= !a.settled() || a.matches();
                println!(
                    "{:<45} balance {:>10}  journal {:>10}  in flight {}  {verdict}",
                    a.rail.0, sats(a.balance), signed_sats(a.journal), a.in_flight
                );
            }
            if !ok {
                bail!("balance and journal disagree");
            }
        }
    }
    Ok(())
}

/// Polls the operator until the operation is final or `seconds` pass.
async fn wait_final(wallet: &Wallet, op: &indigo_domain::OpId, seconds: u64) -> anyhow::Result<OpStatus> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let status = wallet.refresh(op).await?;
        if status.is_final() || tokio::time::Instant::now() >= deadline {
            return Ok(status);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn print_recovery(report: &StartupReport) {
    if report.is_quiet() {
        return;
    }
    for (rail, r) in &report.rails {
        if r.resolved > 0 || r.claimed > Msat::ZERO {
            eprintln!("recover: {rail}: {} interrupted operations resolved, {} claimed", r.resolved, r.claimed);
        }
        for problem in &r.problems {
            eprintln!("recover: {problem}");
        }
    }
    for problem in &report.problems {
        eprintln!("recover: {problem}");
    }
    for intent in &report.moves {
        eprintln!("recover: move {} -> {}", intent.id, intent.state);
    }
    for (op, status) in &report.settled {
        eprintln!("recover: {op} -> {}", status.label());
    }
}

fn print_move(intent: &IntentRecord) {
    let note = intent.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default();
    println!(
        "{:<20} {:>10}  {} -> {}  {}{note}",
        intent.state.as_str(),
        sats(intent.amount),
        intent.from,
        intent.to,
        intent.id
    );
}

fn print_op(op: &OpRecord) {
    let extra = match &op.status {
        OpStatus::Completed { fee, .. } if *fee > Msat::ZERO => format!(" fee {}", sats(*fee)),
        OpStatus::Failed { reason, cost } if *cost > Msat::ZERO => format!(" ({reason}; cost {})", sats(*cost)),
        OpStatus::Failed { reason, .. } => format!(" ({reason})"),
        _ => String::new(),
    };
    println!("{:<10} {:<15} {:>10}  {}{extra}  {}", op.status.label(), op.kind.as_str(), sats(op.amount), op.created_at, op.op);
}

/// An amount as a bare number of sats, for columns: `2000`, or `17.5` with a fraction.
fn sats(amount: Msat) -> String {
    amount.to_string().trim_end_matches(" sats").to_string()
}

fn signed_sats(msat: i64) -> String {
    let sign = if msat < 0 { "-" } else { "" };
    format!("{sign}{}", sats(Msat(msat.unsigned_abs())))
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn default_data_dir(network: Network) -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("INDIGO_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").context("set --data-dir or INDIGO_DATA_DIR")?;
    Ok(PathBuf::from(home).join(".indigo").join(format!("{network:?}").to_lowercase()))
}

/// The top-ups a send made, then how it ended.
fn print_send(report: &indigo_core::SendReport) {
    for m in &report.top_ups {
        println!("topped up: {} from {} to {}", m.amount, m.from.0, m.to.0);
    }
    match &report.status {
        OpStatus::Completed { amount, fee } => println!("paid {amount}, fee {fee}, from {}", report.from.0),
        other => println!("{} from {}", other.label(), report.from.0),
    }
}
