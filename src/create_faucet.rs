//! `create-faucet` subcommand: build a PUBLIC fungible faucet account and write
//! its `.mac` (`AccountFile`). Pure construction — no network. The faucet is
//! deployed on-chain automatically by the service's first mint.
//!
//! Uses the crates.io miden-client 0.15 faucet model (`create_fungible_faucet` +
//! `TokenPolicyManager`), matching the deployed public-testnet faucet.

use anyhow::{Context, Result};
use clap::Args;
use miden_client::account::component::{
    create_fungible_faucet, AccessControl, AuthScheme, BurnPolicyConfig, FungibleFaucet,
    MintPolicyConfig, PolicyRegistration, TokenName, TokenPolicyManager, TransferPolicy,
};
use miden_client::account::{AccountFile, AccountType};
use miden_client::asset::{AssetAmount, TokenSymbol};
use miden_client::auth::{AuthMethod, AuthSecretKey};
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
}

pub fn run(args: &CreateFaucetArgs) -> Result<()> {
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
    let auth_method = AuthMethod::SingleSig {
        approver: (secret.public_key().to_commitment().into(), AuthScheme::Falcon512Poseidon2),
    };

    // AllowAll policies (test faucet): mint/burn plus send/receive transfers.
    let policies = TokenPolicyManager::new()
        .with_mint_policy(MintPolicyConfig::AllowAll, PolicyRegistration::Active)
        .map_err(|e| anyhow::anyhow!("mint policy: {e}"))?
        .with_burn_policy(BurnPolicyConfig::AllowAll, PolicyRegistration::Active)
        .map_err(|e| anyhow::anyhow!("burn policy: {e}"))?
        .with_send_policy(TransferPolicy::AllowAll, PolicyRegistration::Active)
        .map_err(|e| anyhow::anyhow!("send policy: {e}"))?
        .with_receive_policy(TransferPolicy::AllowAll, PolicyRegistration::Active)
        .map_err(|e| anyhow::anyhow!("receive policy: {e}"))?;

    let account = create_fungible_faucet(
        rand::random(),
        faucet,
        AccountType::Public,
        auth_method,
        AccessControl::AuthControlled,
        policies,
    )
    .map_err(|e| anyhow::anyhow!("failed to create faucet account: {e}"))?;

    let account_id = account.id();
    let key = AuthSecretKey::Falcon512Poseidon2(secret);
    AccountFile::new(account, vec![key])
        .write(&args.out)
        .with_context(|| format!("failed to write account file {}", args.out))?;

    println!("Created public fungible faucet");
    println!("  symbol:     {}", args.symbol);
    println!("  decimals:   {}", args.decimals);
    println!("  account id: {account_id}");
    println!("  written to: {}", args.out);
    println!();
    println!("Add a [[tokens]] entry to faucet.toml referencing this .mac, with its own");
    println!("store_path and keystore_path. The faucet deploys on-chain on its first mint.");
    Ok(())
}
