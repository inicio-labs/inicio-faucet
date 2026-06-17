//! `create-faucet` subcommand: build a PUBLIC fungible faucet account and write
//! its `.mac` (`AccountFile`). Pure construction — no network. The faucet is
//! deployed on-chain automatically by the service's first mint (nonce 0 -> 1).
//!
//! Mirrors the miden-cli's faucet assembly: a `BasicFungibleFaucet` component +
//! a `FungibleTokenMetadata` component (symbol/decimals/max-supply) + a Falcon512
//! single-sig auth component, on a public `FungibleFaucet` account.

use anyhow::{Context, Result};
use clap::Args;
use miden_client::account::component::{
    AccountComponent, BasicFungibleFaucet, FungibleTokenMetadata, TokenName,
};
use miden_client::account::{
    AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountFile, AccountStorageMode, AccountType,
};
use miden_client::asset::TokenSymbol;
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use rand::RngCore;

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
    let symbol = TokenSymbol::new(&args.symbol)
        .map_err(|e| anyhow::anyhow!("invalid token symbol {:?}: {e}", args.symbol))?;
    let name_str = if args.name.is_empty() { args.symbol.as_str() } else { args.name.as_str() };
    let name = TokenName::new(name_str)
        .map_err(|e| anyhow::anyhow!("invalid token name {name_str:?}: {e}"))?;

    // Token metadata component (holds symbol/decimals/max-supply); the faucet
    // component below has no storage of its own and depends on this being present.
    let token_metadata: AccountComponent =
        FungibleTokenMetadata::builder(name, symbol, args.decimals, args.max_supply)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build token metadata: {e}"))?
            .into();
    let faucet_component: AccountComponent = BasicFungibleFaucet.into();

    let key = AuthSecretKey::new_falcon512_poseidon2();

    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);

    let account = AccountBuilder::new(seed)
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            key.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(faucet_component)
        .with_component(token_metadata)
        .build_with_schema_commitment()
        .map_err(|e| anyhow::anyhow!("failed to build faucet account: {e}"))?;

    let account_id = account.id();
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
