//! Inicio faucet service.
//!
//! Two modes:
//!  - `serve` (default): runs the HTTP service. One worker thread per faucet
//!    account (each owns a `!Send` miden `Client`), reached over `mpsc`.
//!  - `create-faucet`: builds a public fungible faucet and writes its `.mac`.

mod config;
mod create_faucet;
mod http;
mod mint;
mod worker;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use config::FaucetConfig;
use create_faucet::CreateFaucetArgs;
use http::{AppState, TokenMeta};
use mint::MintJob;
use worker::WorkerParams;

#[derive(Parser)]
#[command(name = "inicio-faucet", version, about = "Internal faucet service for minting test tokens")]
struct Cli {
    /// Path to the TOML config file.
    #[arg(long, env = "FAUCET_CONFIG", default_value = "faucet.toml")]
    config: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the faucet HTTP service (this is the default).
    Serve,
    /// Create a new public fungible faucet and write its `.mac` file.
    CreateFaucet(CreateFaucetArgs),
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).compact().init();
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::CreateFaucet(args) => create_faucet::run(&args),
        Command::Serve => {
            let rt = tokio::runtime::Runtime::new().context("failed to build tokio runtime")?;
            rt.block_on(serve(&cli.config))
        }
    }
}

async fn serve(config_path: &str) -> Result<()> {
    let config = FaucetConfig::load(config_path)?;
    let cancel = CancellationToken::new();

    let mut senders = HashMap::new();
    let mut tokens = Vec::new();
    let mut ready_rxs = Vec::new();
    let mut handles = Vec::new();

    for token in &config.tokens {
        if senders.contains_key(&token.symbol) {
            cancel.cancel();
            join_all(handles);
            anyhow::bail!("duplicate token symbol in config: {}", token.symbol);
        }
        let (tx, rx) = mpsc::channel::<MintJob>(1024);
        let (ready_tx, ready_rx) = oneshot::channel();
        senders.insert(token.symbol.clone(), tx);
        tokens.push(TokenMeta {
            symbol: token.symbol.clone(),
            name: token.name.clone(),
            decimals: token.decimals,
        });
        let params = WorkerParams {
            rpc: config.rpc.clone(),
            token: token.clone(),
            rx,
            cancel: cancel.clone(),
            ready: ready_tx,
            max_batch: config.server.max_batch_size,
        };
        handles.push(worker::spawn(params));
        ready_rxs.push((token.symbol.clone(), ready_rx));
    }

    // Startup gate: every worker must report ready (client built + account
    // imported + initial sync) before we serve, so failures surface here.
    for (symbol, rx) in ready_rxs {
        match rx.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                cancel.cancel();
                join_all(handles);
                anyhow::bail!("faucet {symbol} failed to start: {e}");
            }
            Err(_) => {
                cancel.cancel();
                join_all(handles);
                anyhow::bail!("faucet {symbol} worker exited before signalling readiness");
            }
        }
    }
    tracing::info!(count = config.tokens.len(), "all faucet workers ready");

    let state = AppState { tokens: Arc::new(tokens), senders: Arc::new(senders) };
    let app = http::router(state, &config.server.static_dir);

    let listener = tokio::net::TcpListener::bind(&config.server.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.server.bind))?;
    tracing::info!(bind = %config.server.bind, "faucet service listening");

    let shutdown = {
        let cancel = cancel.clone();
        async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("ctrl-c received, shutting down"),
                _ = cancel.cancelled() => {}
            }
        }
    };
    let serve_result = axum::serve(listener, app).with_graceful_shutdown(shutdown).await;

    cancel.cancel();
    join_all(handles);
    serve_result.context("http server error")
}

fn join_all(handles: Vec<std::thread::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.join();
    }
}
