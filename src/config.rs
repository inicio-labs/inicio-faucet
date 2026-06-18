//! Runtime configuration, loaded from a TOML file (mirrors the solver's
//! `SolverConfig::load` pattern: `read_to_string` + `toml::from_str`).

use anyhow::{Context, Result};
use serde::Deserialize;

/// Top-level faucet service config.
#[derive(Debug, Clone, Deserialize)]
pub struct FaucetConfig {
    pub rpc: RpcConfig,
    pub server: ServerConfig,
    /// One entry per faucet/token. Four are expected, but any number works.
    pub tokens: Vec<TokenConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    /// Miden node gRPC endpoint, e.g. `https://rpc.devnet.miden.io`.
    pub endpoint: String,
    pub timeout_ms: u64,
    /// Optional remote transaction prover (e.g. `https://tx-prover.devnet.miden.io`).
    /// When unset, transactions are proved locally (CPU-heavy).
    #[serde(default)]
    pub remote_prover_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Socket address to bind, e.g. `127.0.0.1:8080` (internal-only by default).
    pub bind: String,
    /// Maximum number of P2ID notes minted per transaction. A worker drains up to
    /// this many queued requests at once (no fixed wait); bursts coalesce naturally
    /// while the previous batch is being proved.
    #[serde(default = "default_max_batch_size")]
    pub max_batch_size: usize,
    /// Directory served for the static frontend (index.html, game, assets).
    #[serde(default = "default_static_dir")]
    pub static_dir: String,
}

/// One faucet account, imported from a `.mac` produced by `create-faucet`.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    /// Short ticker, used as the API key for this token (e.g. "TOKA").
    pub symbol: String,
    /// Human-readable name shown in the UI.
    pub name: String,
    pub decimals: u8,
    /// Path to the `.mac` AccountFile (account + Falcon signing key).
    pub account_file: String,
    /// Per-faucet sqlite store path (each worker is isolated).
    pub store_path: String,
    /// Per-faucet keystore directory.
    pub keystore_path: String,
    /// Optional cap on the amount mintable per request (base units). Unset = uncapped.
    #[serde(default)]
    pub max_mint_amount: Option<u64>,
}

fn default_max_batch_size() -> usize {
    256
}

fn default_static_dir() -> String {
    "static".to_string()
}

impl FaucetConfig {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {path}"))?;
        let config: FaucetConfig =
            toml::from_str(&content).with_context(|| format!("failed to parse config file: {path}"))?;
        if config.tokens.is_empty() {
            anyhow::bail!("config has no [[tokens]] entries");
        }
        if config.server.max_batch_size == 0 {
            anyhow::bail!("server.max_batch_size must be at least 1");
        }
        Ok(config)
    }
}
