//! `indigo`: the project's test harness. Command names are referenced by the
//! test log, so keep them stable once chosen.

mod network;
mod status;
mod wallet;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use network::Network;

#[derive(Parser)]
#[command(name = "indigo", version, about = "INDIGO test harness")]
struct Cli {
    #[arg(long, value_enum, global = true, default_value = "mutinynet")]
    network: Network,

    /// Wallet folder. Default: $INDIGO_DATA_DIR, else ~/.indigo/<network>.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Which pocket to use.
    #[arg(long, global = true, default_value = "cashu")]
    rail: String,

    /// Cashu mint URL. Default: the network's test mint.
    #[arg(long, global = true)]
    mint: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that every service the network needs is reachable.
    Status,
    /// Print a fresh 12-word phrase for INDIGO_TEST_MNEMONIC (test money only).
    NewTestSeed,
    #[command(flatten)]
    Wallet(wallet::WalletCommand),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Piped into `head` and the like, stop quietly when the reader goes away
    // instead of panicking on the next print (Rust ignores SIGPIPE by default).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // Off unless asked for, e.g. RUST_LOG=cdk=info, to see what the pockets do.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Status => {
            let endpoints = cli.network.endpoints();
            let checks = status::run(&endpoints).await?;
            println!("network: {:?}", cli.network);
            let mut failed = 0;
            for check in &checks {
                let ms = check.elapsed.as_millis();
                match &check.result {
                    Ok(detail) => println!("  ok    {:<18} {detail} [{ms} ms]", check.service),
                    Err(err) => {
                        failed += 1;
                        println!("  FAIL  {:<18} {err:#} [{ms} ms]", check.service);
                    }
                }
            }
            if failed > 0 {
                anyhow::bail!("{failed} of {} services unreachable", checks.len());
            }
        }
        Command::NewTestSeed => println!("{}", bip39::Mnemonic::generate(12)?),
        Command::Wallet(command) => {
            let opts = wallet::Options { network: cli.network, data_dir: cli.data_dir, rail: cli.rail, mint: cli.mint };
            wallet::run(opts, command).await?;
        }
    }
    Ok(())
}
