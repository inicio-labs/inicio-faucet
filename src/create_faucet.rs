//! `create-faucet` subcommand: build a PUBLIC fungible faucet account and write
//! its `.mac` (`AccountFile`). Pure construction — no network. The service
//! deploys the faucet on-chain when it starts; on a fee-charging chain it first
//! needs some of the chain's native asset (MIDEN) sent to its address to pay the
//! deployment fee.
//!
//! Uses the crates.io miden-client 0.17 faucet model
//! (`create_singlesig_user_fungible_faucet` + `TokenPolicyManager`).

use anyhow::{Context, Result};
use clap::Args;
use miden_client::account::component::{
    create_singlesig_user_fungible_faucet, AuthSingleSig, BurnPolicy, FungibleFaucet, MintPolicy,
    TokenName, TokenPolicyManager,
};
use miden_client::account::{AccountFile, AccountType};
use miden_client::address::NetworkId;
use miden_client::asset::{AssetAmount, TokenSymbol};
use miden_client::auth::AuthSecretKey;
use miden_client::crypto::rpo_falcon512::SecretKey;

#[derive(Debug, Args)]
pub struct CreateFaucetArgs {
    /// Token ticker, e.g. "TOKA".
    #[arg(long)]
    pub symbol: String,
    /// Human-readable token name. Defaults to the symbol.
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long)]
    pub decimals: u8,
    /// Maximum supply, in base units.
    #[arg(long)]
    pub max_supply: u64,
    /// Output path for the `.mac` AccountFile.
    #[arg(long)]
    pub out: String,
    /// Network whose address format to print: `testnet`, `devnet` or `mainnet`.
    #[arg(long, default_value = "testnet")]
    pub network: String,
}

pub fn run(args: &CreateFaucetArgs) -> Result<()> {
    let network = match args.network.as_str() {
        "testnet" => NetworkId::Testnet,
        "devnet" => NetworkId::Devnet,
        "mainnet" => NetworkId::Mainnet,
        other => anyhow::bail!("unknown network {other:?} (expected testnet, devnet or mainnet)"),
    };
    let symbol = TokenSymbol::try_from(args.symbol.as_str())
        .map_err(|e| anyhow::anyhow!("invalid token symbol {:?}: {e}", args.symbol))?;
    let name_str = if args.name.is_empty() { args.symbol.as_str() } else { args.name.as_str() };
    let name = TokenName::new(name_str)
        .map_err(|e| anyhow::anyhow!("invalid token name {name_str:?}: {e}"))?;
    let max_supply = AssetAmount::new(args.max_supply)
        .map_err(|e| anyhow::anyhow!("invalid max supply {}: {e}", args.max_supply))?;

    let faucet = FungibleFaucet::builder()
        .name(name)
        .symbol(symbol)
        .decimals(args.decimals)
        .max_supply(max_supply)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build faucet metadata: {e}"))?;

    // Falcon512 single-sig auth.
    let secret = SecretKey::new();
    let auth = AuthSingleSig::falcon512_poseidon2(secret.public_key());

    // AllowAll mint/burn policies only. No send/receive transfer policies: those
    // enable asset callbacks (and flip the account ID's callback flag), which
    // require a callback-aware custom mint script — the standard
    // `own_output_notes` mint path can't satisfy them.
    let policies = TokenPolicyManager::builder()
        .active_mint_policy(MintPolicy::allow_all())
        .active_burn_policy(BurnPolicy::allow_all())
        .build();

    let account = create_singlesig_user_fungible_faucet(
        rand::random(),
        faucet,
        auth,
        policies,
        AccountType::Public,
    )
    .map_err(|e| anyhow::anyhow!("failed to create faucet account: {e}"))?;

    let account_id = account.id();
    let key = AuthSecretKey::Falcon512Poseidon2(secret);
    AccountFile::new(account, vec![key])
        .write(&args.out)
        .with_context(|| format!("failed to write account file {}", args.out))?;

    // The .mac contains the faucet's Falcon signing key — restrict to owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&args.out, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set 0600 permissions on {}", args.out))?;
    }

    println!("Created public fungible faucet");
    println!("  symbol:     {}", args.symbol);
    println!("  decimals:   {}", args.decimals);
    println!("  account id: {account_id}");
    println!("  address:    {}", account_id.to_bech32(network));
    println!("  written to: {}", args.out);
    println!();
    println!("Add a [[tokens]] entry to faucet.toml referencing this .mac, with its own");
    println!("store_path and keystore_path. On a fee-charging chain, send the address some");
    println!("native MIDEN (the fee asset) before starting: the service deploys the faucet by");
    println!("consuming that note, and answers mints for it with 503 until it arrives.");
    Ok(())
}
