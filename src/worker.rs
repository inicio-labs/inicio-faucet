//! Per-faucet worker. Each faucet account gets its own OS thread running a
//! `current_thread` runtime + `LocalSet`, because the miden `Client` is `!Send`
//! and must never cross a thread boundary (same model the solver uses).
//!
//! This one design gives us three things at once:
//!  1. `!Send` isolation — the `Client` stays on its thread.
//!  2. Nonce serialization — one worker per faucet => its transactions are
//!     strictly sequential, so there are no in-flight nonce conflicts.
//!  3. Batching — the worker drains its queue and mints all pending requests as a
//!     single transaction with N P2ID notes.
//!
//! Fees: on a fee-charging chain every transaction pays a fee in the chain's native asset
//! from the faucet's own vault (the client attaches the payment automatically). A brand-new
//! faucet has an empty vault, so it deploys by consuming a funding note of the native asset
//! sent to it; until one arrives it answers mints with 503 and keeps checking.

// Functions here return miden-client's `ClientError`, which is large (>128 bytes) as of 0.16.
// It's the library's type and only travels on the (rare) error path, so boxing it everywhere
// would add noise without a real benefit.
#![allow(clippy::result_large_err)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use miden_client::account::{AccountFile, AccountId};
use miden_client::asset::FungibleAsset;
use miden_client::block::BlockNumber;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::{Note, NoteDetails, NoteFile, NoteSyncHint, NoteType, P2idNote};
use miden_client::rpc::{AddTransactionError, Endpoint, EndpointError};
use miden_client::transaction::{
    LocalTransactionProver, ProvingOptions, TransactionId, TransactionProver, TransactionRequest,
    TransactionRequestBuilder, TransactionResult,
};
use miden_client::utils::Serializable;
use miden_client::{Client, ClientError, RemoteTransactionProver};
use miden_client_sqlite_store::SqliteStore;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::{RpcConfig, TokenConfig};
use crate::mint::{MintError, MintJob, MintOutcome};

/// How often a faucet that couldn't deploy yet (not funded) re-checks while idle.
const DEPLOY_RETRY: Duration = Duration::from_secs(30);

/// Upper bound on one transaction's fee, as a multiple of the chain's `verification_base_fee`.
/// The fee is `base_fee * (floor(log2(cycles)) + 1)` — at most `30 * base_fee` — and the auth
/// procedure may set aside up to twice that, so `64 * base_fee` always covers one transaction.
const MAX_FEE_MULTIPLIER: u64 = 64;

/// Below this many transactions' worth of fee balance, warn and pull in any funding notes
/// waiting for the faucet, so a top-up lands before the balance runs out.
const LOW_BALANCE_TXS: u64 = 500;

/// Everything a worker thread needs. All fields are `Send` so the struct can be
/// moved into the spawned thread; the `!Send` `Client` is built *on* that thread.
pub struct WorkerParams {
    pub rpc: RpcConfig,
    pub token: TokenConfig,
    pub rx: mpsc::Receiver<MintJob>,
    pub cancel: CancellationToken,
    /// Reports readiness (client built + account imported) or a startup error, so
    /// failures surface at the startup gate instead of in a detached thread.
    pub ready: oneshot::Sender<Result<(), String>>,
    pub max_batch: usize,
}

/// Spawn the worker on a dedicated OS thread. Returns the join handle.
pub fn spawn(params: WorkerParams) -> std::thread::JoinHandle<()> {
    let name = format!("faucet-{}", params.token.symbol);
    std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || run_on_local_runtime(&name, worker_loop(params)))
        .expect("failed to spawn faucet worker thread")
}

/// Build a `current_thread` runtime + `LocalSet` and drive `fut` to completion,
/// so the `!Send` `Client` it builds never leaves this thread.
fn run_on_local_runtime<F: Future<Output = ()>>(thread_name: &str, fut: F) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(thread = thread_name, error = %e, "failed to build thread runtime");
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, fut);
}

/// The faucet account a worker mints from.
struct Faucet {
    id: AccountId,
    symbol: String,
    /// Bech32 address on the node's network — what people fund and look up.
    address: String,
}

async fn worker_loop(params: WorkerParams) {
    let WorkerParams { rpc, token, mut rx, cancel, ready, max_batch } = params;
    let symbol = token.symbol.clone();

    let (mut client, faucet, provers, needs_deploy) = match build_client(&rpc, &token).await {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(format!("[{symbol}] {e:#}")));
            return;
        }
    };
    tracing::info!(token = %symbol, faucet = %faucet.id, address = %faucet.address, "faucet worker ready");

    // A new faucet deploys right away if it's funded. If not, keep serving — mints for this token
    // are answered 503 with the address to fund — and retry until a funding note arrives.
    let mut deployed = !needs_deploy || deploy_faucet(&mut client, &faucet, &provers).await;

    // Drain up to `max_batch` queued requests at a time and mint them in one
    // transaction. `recv_many` returns as soon as at least one request is
    // available (no artificial delay), and naturally coalesces bursts — while a
    // batch is being proved/submitted, new requests queue up for the next drain.
    let mut buffer = Vec::with_capacity(max_batch);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(token = %symbol, "worker shutting down");
                break;
            }
            _ = tokio::time::sleep(DEPLOY_RETRY), if !deployed => {
                deployed = deploy_faucet(&mut client, &faucet, &provers).await;
            }
            n = rx.recv_many(&mut buffer, max_batch) => {
                if n == 0 {
                    break; // channel closed
                }
                let batch = std::mem::take(&mut buffer);
                if !deployed {
                    deployed = deploy_faucet(&mut client, &faucet, &provers).await;
                }
                if deployed {
                    process_batch(&mut client, &faucet, batch, &provers).await;
                } else {
                    let msg = not_funded_message(&faucet);
                    for job in batch {
                        let _ = job.reply.send(Err(MintError::Unavailable(msg.clone())));
                    }
                }
            }
        }
    }
}

/// Build the miden client for this faucet (rpc + its own sqlite store + keystore), import the
/// faucet account from its `.mac`, and sync. Also returns whether the faucet still has to be
/// deployed on-chain.
async fn build_client(
    rpc: &RpcConfig,
    token: &TokenConfig,
) -> Result<(Client<FilesystemKeyStore>, Faucet, Provers, bool)> {
    let keystore = FilesystemKeyStore::new(PathBuf::from(&token.keystore_path))
        .map_err(|e| anyhow::anyhow!("failed to create keystore: {e}"))?;

    let account_file = AccountFile::read(&token.account_file)
        .with_context(|| format!("failed to read account file {}", token.account_file))?;
    let account_id = account_file.account.id();
    let AccountFile { account, auth_secret_keys } = account_file;
    for key in &auth_secret_keys {
        keystore
            .add_key(key, account_id)
            .await
            .map_err(|e| anyhow::anyhow!("failed to add key to keystore: {e}"))?;
    }

    let endpoint = Endpoint::try_from(rpc.endpoint.as_str())
        .map_err(|e| anyhow::anyhow!("invalid rpc endpoint {}: {e}", rpc.endpoint))?;
    let store = Arc::new(SqliteStore::new(PathBuf::from(&token.store_path)).await?);

    let builder = ClientBuilder::new()
        .grpc_client(&endpoint, Some(rpc.timeout_ms))
        .authenticator(Arc::new(keystore))
        .store(store);
    let mut client = builder.build().await.context("failed to build miden client")?;

    // Proving: remote first (when configured), local fallback. We call `prove_transaction_with`
    // explicitly per transaction, so we don't set a default prover on the client builder.
    let provers = Provers {
        remote: rpc.remote_prover_url.as_ref().map(|url| {
            Arc::new(RemoteTransactionProver::new(url.clone())) as Arc<dyn TransactionProver>
        }),
        local: Arc::new(LocalTransactionProver::new(ProvingOptions::default())),
        remote_attempts: rpc.remote_prover_attempts.max(1),
    };

    client.ensure_genesis_in_place().await.context("failed to ensure genesis in place")?;

    // Load: pull the faucet's on-chain state if it's already deployed. `ensure_genesis_in_place`
    // above already proved the node is reachable, and if the account WERE on-chain this import
    // would succeed (so we'd skip the deploy). Therefore a failure here means the account
    // simply isn't deployed yet — the node signals that with a "not found"-style RPC error whose
    // exact shape varies by version, so we don't match on it; we just log and treat it as new.
    if let Err(e) = client.import_account_by_id(account_id).await {
        tracing::debug!(token = %token.symbol, error = %e, "faucet not loaded from node; treating as not-yet-deployed");
    }

    // Track the `.mac` account locally if the load above didn't put it in the store.
    if client.get_account(account_id).await?.is_none() {
        match client.add_account(&account, false).await {
            Ok(()) | Err(ClientError::AccountAlreadyTracked(_)) => {}
            Err(e) => return Err(e).context("failed to add faucet account to store"),
        }
    }

    // Sync before anything is submitted: submissions are sealed to a key the client validates
    // against synced chain headers, and the sync also picks up funding notes sent to the faucet.
    sync_with_retry(&mut client).await.context("initial sync failed")?;

    let record = client
        .get_account(account_id)
        .await?
        .context("faucet account missing from store after load")?;
    let needs_deploy = record.is_new();
    if needs_deploy {
        tracing::info!(token = %token.symbol, faucet = %account_id, "faucet not on-chain yet — will deploy it from the .mac");
    } else {
        tracing::info!(token = %token.symbol, faucet = %account_id, "loaded existing on-chain faucet account");
    }

    let address = account_id.to_bech32(client.network_id().await?);
    let faucet = Faucet { id: account_id, symbol: token.symbol.clone(), address };
    Ok((client, faucet, provers, needs_deploy))
}

/// `sync_state` with a few retries, so a transient node blip at startup doesn't kill the worker.
async fn sync_with_retry(client: &mut Client<FilesystemKeyStore>) -> Result<(), ClientError> {
    const ATTEMPTS: u32 = 3;
    let mut last_err = None;
    for attempt in 1..=ATTEMPTS {
        match client.sync_state().await {
            Ok(_) => return Ok(()),
            Err(e) => {
                tracing::warn!(attempt, max = ATTEMPTS, error = %e, "sync failed");
                last_err = Some(e);
                if attempt < ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
                }
            }
        }
    }
    Err(last_err.expect("loop runs at least once"))
}

/// The chain's fee parameters, from the latest synced block header.
struct Fees {
    base_fee: u64,
    fee_faucet_id: AccountId,
}

impl Fees {
    /// Upper bound on one transaction's fee (see [`MAX_FEE_MULTIPLIER`]).
    fn max_tx_fee(&self) -> u64 {
        self.base_fee.saturating_mul(MAX_FEE_MULTIPLIER)
    }
}

async fn fee_parameters(client: &Client<FilesystemKeyStore>) -> Result<Fees, ClientError> {
    let header = client.get_latest_block_header().await?;
    let params = header.fee_parameters();
    Ok(Fees {
        base_fee: u64::from(params.verification_base_fee()),
        fee_faucet_id: params.fee_faucet_id(),
    })
}

/// The faucet's balance of the fee asset, as of the last sync. `None` if it can't be read.
async fn fee_balance(client: &Client<FilesystemKeyStore>, faucet: &Faucet, fees: &Fees) -> Option<u64> {
    match client.account_reader(faucet.id).get_balance(fees.fee_faucet_id).await {
        Ok(amount) => Some(amount.as_u64()),
        Err(e) => {
            tracing::warn!(token = %faucet.symbol, error = %e, "could not read the fee balance");
            None
        }
    }
}

/// Notes waiting to be consumed by the faucet — e.g. native-asset funding sent to it.
async fn consumable_notes(
    client: &Client<FilesystemKeyStore>,
    faucet_id: AccountId,
) -> Result<Vec<Note>, ClientError> {
    Ok(client
        .get_consumable_notes(Some(faucet_id))
        .await?
        .into_iter()
        .filter_map(|(record, _)| TryInto::<Note>::try_into(record).ok())
        .collect())
}

fn not_funded_message(faucet: &Faucet) -> String {
    format!(
        "faucet {} is not deployed yet: it needs native MIDEN (the fee asset) to pay for its \
         deployment — send some to {} and retry",
        faucet.symbol, faucet.address
    )
}

/// Deploy a new faucet on-chain. On a fee-charging chain the deploy transaction consumes the
/// funding note(s) waiting for the faucet, so it can pay its own fee; without one it can't deploy
/// yet. Returns whether the faucet is now deployed.
async fn deploy_faucet(
    client: &mut Client<FilesystemKeyStore>,
    faucet: &Faucet,
    provers: &Provers,
) -> bool {
    if let Err(e) = client.sync_state().await {
        tracing::warn!(token = %faucet.symbol, error = %e, "sync before deploy failed; will retry");
        return false;
    }
    let fees = match fee_parameters(client).await {
        Ok(fees) => fees,
        Err(e) => {
            tracing::warn!(token = %faucet.symbol, error = %e, "could not read fee parameters; will retry deploy");
            return false;
        }
    };

    let request = if fees.base_fee == 0 {
        // Fee-free chain: an empty transaction (nonce 0 -> 1) deploys the account.
        TransactionRequestBuilder::new().build()
    } else {
        let notes = match consumable_notes(client, faucet.id).await {
            Ok(notes) => notes,
            Err(e) => {
                tracing::warn!(token = %faucet.symbol, error = %e, "could not list funding notes; will retry deploy");
                return false;
            }
        };
        if notes.is_empty() {
            tracing::warn!(token = %faucet.symbol, address = %faucet.address, "{}", not_funded_message(faucet));
            return false;
        }
        tracing::info!(token = %faucet.symbol, notes = notes.len(), "deploying faucet by consuming its funding note(s)");
        TransactionRequestBuilder::new().build_consume_notes(notes)
    };
    let request = match request {
        Ok(request) => request,
        Err(e) => {
            tracing::error!(token = %faucet.symbol, error = %e, "failed to build deploy transaction");
            return false;
        }
    };

    match submit_batch(client, faucet.id, request, provers).await {
        Ok((tx_id, _)) => {
            tracing::info!(token = %faucet.symbol, faucet = %faucet.id, tx = %tx_id.to_hex(), "faucet deployed on-chain");
            true
        }
        Err(e) => {
            tracing::error!(token = %faucet.symbol, error = ?e, "faucet deploy failed; will retry");
            false
        }
    }
}

/// Make sure the faucet can pay for its next transaction's fee. When the balance runs low, pull in
/// any funding notes waiting for it; if it still can't pay, return the message to answer with (503).
async fn ensure_fee_funds(
    client: &mut Client<FilesystemKeyStore>,
    faucet: &Faucet,
    provers: &Provers,
) -> Result<(), String> {
    let fees = match fee_parameters(client).await {
        Ok(fees) => fees,
        Err(e) => {
            // Can't tell — let the mint go ahead rather than refuse it over a read error.
            tracing::warn!(token = %faucet.symbol, error = %e, "could not read fee parameters");
            return Ok(());
        }
    };
    if fees.base_fee == 0 {
        return Ok(());
    }
    let Some(mut balance) = fee_balance(client, faucet, &fees).await else {
        return Ok(());
    };
    if balance < fees.max_tx_fee().saturating_mul(LOW_BALANCE_TXS) {
        tracing::warn!(token = %faucet.symbol, balance, address = %faucet.address, "faucet fee balance is low — send native MIDEN to top it up");
        if absorb_funding(client, faucet, provers).await {
            balance = fee_balance(client, faucet, &fees).await.unwrap_or(balance);
        }
    }
    if balance < fees.max_tx_fee() {
        return Err(format!(
            "faucet {} is out of fee funds: send native MIDEN (the fee asset) to {} and retry",
            faucet.symbol, faucet.address
        ));
    }
    Ok(())
}

/// Consume any funding notes waiting for the faucet, topping up its fee balance. Returns whether a
/// top-up landed.
async fn absorb_funding(
    client: &mut Client<FilesystemKeyStore>,
    faucet: &Faucet,
    provers: &Provers,
) -> bool {
    let notes = match consumable_notes(client, faucet.id).await {
        Ok(notes) if !notes.is_empty() => notes,
        Ok(_) => return false,
        Err(e) => {
            tracing::warn!(token = %faucet.symbol, error = %e, "could not list funding notes");
            return false;
        }
    };
    let count = notes.len();
    let request = match TransactionRequestBuilder::new().build_consume_notes(notes) {
        Ok(request) => request,
        Err(e) => {
            tracing::warn!(token = %faucet.symbol, error = %e, "failed to build top-up transaction");
            return false;
        }
    };
    match submit_batch(client, faucet.id, request, provers).await {
        Ok(_) => {
            tracing::info!(token = %faucet.symbol, notes = count, "absorbed funding note(s) into the fee balance");
            true
        }
        Err(e) => {
            tracing::warn!(token = %faucet.symbol, error = ?e, "failed to absorb funding notes");
            false
        }
    }
}

/// Proving strategy for a worker: try the remote prover up to `remote_attempts` times, then fall
/// back to LOCAL proving. The public testnet prover flakes (intermittent "Timeout expired"); local
/// is the guaranteed (slower) backstop. Proving is BEFORE submit, so trying multiple provers is
/// safe — nothing lands on-chain until `submit_proven_transaction`.
struct Provers {
    remote: Option<Arc<dyn TransactionProver>>,
    local: Arc<dyn TransactionProver>,
    remote_attempts: u32,
}

/// Execute a transaction, retrying transient RPC errors. Execution is BEFORE submit (nothing has
/// landed on-chain yet), so re-running is safe — a transient node blip on execute won't fail the mint.
async fn execute_with_retry(
    client: &mut Client<FilesystemKeyStore>,
    faucet_id: AccountId,
    request: &TransactionRequest,
) -> Result<TransactionResult, ClientError> {
    const ATTEMPTS: u32 = 3;
    let mut last_err = None;
    for attempt in 1..=ATTEMPTS {
        match client.execute_transaction(faucet_id, request.clone()).await {
            Ok(t) => return Ok(t),
            Err(e) => {
                if !matches!(e, ClientError::RpcError(_)) {
                    return Err(e);
                }
                tracing::warn!(attempt, max = ATTEMPTS, error = %e, "execute failed (transient RPC), resyncing and retrying");
                last_err = Some(e);
                if attempt < ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
                    let _ = client.sync_state().await;
                }
            }
        }
    }
    Err(last_err.expect("loop runs at least once"))
}

/// Execute → prove (remote, with local fallback) → submit → apply, returning the tx id and the
/// block height reported at submission.
///
/// Retries the whole execute→prove→submit on a mempool-head conflict. Under load, consecutive mints
/// for one faucet chain through the node's mempool: tx N+1's initial account commitment must match the
/// mempool head (tx N's output), not the last committed block. If tx N is still in-flight when we
/// build tx N+1 against the just-synced committed state, the node rejects it with a `StateConflict`
/// (see [`is_mempool_state_conflict`]). That rejection is definitive — nothing landed on-chain — so we
/// wait ~1 block for the in-flight tx to commit, resync, and REBUILD (re-execute, since the proof is
/// bound to the stale initial state). A submission with no definite answer is handled separately by
/// [`resolve_unknown_submission`], which never builds a replacement, so a mint is never sent twice.
async fn submit_batch(
    client: &mut Client<FilesystemKeyStore>,
    faucet_id: AccountId,
    request: TransactionRequest,
    provers: &Provers,
) -> Result<(TransactionId, BlockNumber), ClientError> {
    const SUBMIT_ATTEMPTS: u32 = 3;
    let mut last_err = None;
    for submit_attempt in 1..=SUBMIT_ATTEMPTS {
        let tx_result = execute_with_retry(client, faucet_id, &request).await?;
        let tx_id = tx_result.executed_transaction().id();

        // Prove the SAME executed transaction: remote first (fast when healthy), local as fallback.
        let mut proven = None;
        if let Some(remote) = &provers.remote {
            for attempt in 1..=provers.remote_attempts {
                match client.prove_transaction_with(&tx_result, remote.clone()).await {
                    Ok(p) => {
                        proven = Some(p);
                        break;
                    }
                    // A proof for a different transaction is as useless as no proof: fall back too.
                    Err(
                        e @ (ClientError::TransactionProvingError(_)
                        | ClientError::MismatchedProvenTransaction { .. }),
                    ) => {
                        tracing::warn!(attempt, max = provers.remote_attempts, error = %e, "remote proving failed");
                        if attempt < provers.remote_attempts {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        let proven = match proven {
            Some(p) => p,
            None => {
                if provers.remote.is_some() {
                    tracing::warn!("remote prover exhausted — falling back to LOCAL proving");
                }
                client.prove_transaction_with(&tx_result, provers.local.clone()).await?
            }
        };

        match client.submit_proven_transaction(proven, &tx_result).await {
            Ok(height) => {
                client.apply_transaction(&tx_result, height).await?;
                return Ok((tx_id, height));
            }
            Err(e @ ClientError::SubmissionOutcomeUnknown { .. }) => {
                return resolve_unknown_submission(client, faucet_id, &tx_result, e).await;
            }
            Err(e) => {
                // A mempool-head conflict is safe to rebuild (the tx was rejected, not applied); wait
                // ~1 block for the in-flight tx to commit, resync, and let the loop re-execute. Any
                // other error is returned as-is.
                if is_mempool_state_conflict(&e) && submit_attempt < SUBMIT_ATTEMPTS {
                    tracing::warn!(
                        attempt = submit_attempt,
                        max = SUBMIT_ATTEMPTS,
                        error = %e,
                        "submit conflicted with mempool head — waiting one block, resyncing, and rebuilding"
                    );
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if let Err(sync_err) = client.sync_state().await {
                        tracing::warn!(error = %sync_err, "resync after mempool conflict failed (continuing)");
                    }
                    last_err = Some(e);
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(last_err.expect("loop only continues after storing a conflict error"))
}

/// The node gave no definite answer to a submission (timeout, dropped connection, ...), so the
/// transaction may or may not have landed. Never build a replacement — if the original landed that
/// would be a second mint. Instead resubmit the SAME proven transaction (its id is fixed), and if the
/// node rejects the resubmission, check whether the original landed.
async fn resolve_unknown_submission(
    client: &mut Client<FilesystemKeyStore>,
    faucet_id: AccountId,
    tx_result: &TransactionResult,
    mut err: ClientError,
) -> Result<(TransactionId, BlockNumber), ClientError> {
    const ATTEMPTS: u32 = 3;
    let tx_id = tx_result.executed_transaction().id();
    for attempt in 1..=ATTEMPTS {
        let (transaction, transaction_inputs) = match err {
            ClientError::SubmissionOutcomeUnknown { transaction, transaction_inputs, .. } => {
                (transaction, transaction_inputs)
            }
            other => return Err(other),
        };
        tracing::warn!(attempt, max = ATTEMPTS, tx = %tx_id.to_hex(), "submission outcome unknown — resubmitting the same transaction");
        tokio::time::sleep(Duration::from_secs(3)).await;
        match client.submit_proven_transaction(*transaction, *transaction_inputs).await {
            Ok(height) => {
                client.apply_transaction(tx_result, height).await?;
                return Ok((tx_id, height));
            }
            Err(e @ ClientError::SubmissionOutcomeUnknown { .. }) => err = e,
            Err(e) => {
                // A definite rejection of the resubmission — most likely because the original
                // already landed and the faucet moved past the transaction's initial state.
                return match wait_until_landed(client, faucet_id, tx_result).await {
                    Some(height) => Ok((tx_id, height)),
                    None => Err(e),
                };
            }
        }
    }
    match wait_until_landed(client, faucet_id, tx_result).await {
        Some(height) => Ok((tx_id, height)),
        None => Err(err),
    }
}

/// Wait a few blocks for our transaction to show up on-chain: resync and compare the faucet's
/// state with the transaction's final state. Returns the synced height if it landed.
async fn wait_until_landed(
    client: &mut Client<FilesystemKeyStore>,
    faucet_id: AccountId,
    tx_result: &TransactionResult,
) -> Option<BlockNumber> {
    let final_commitment = tx_result.executed_transaction().final_account().to_commitment();
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if client.sync_state().await.is_err() {
            continue;
        }
        if let Ok(Some(account)) = client.get_account(faucet_id).await {
            if account.to_commitment() == final_commitment {
                return client.get_sync_height().await.ok();
            }
        }
    }
    None
}

/// True when the node rejected the submission because the transaction's initial account commitment
/// doesn't match the mempool's current one — a prior mint for this faucet is still in-flight. That
/// rejection is definitive (nothing landed), so rebuilding on the fresh state can't double-mint.
/// Other rejections (expired, mempool full, missing fee, ...) are not matched: rebuilding wouldn't help.
fn is_mempool_state_conflict(e: &ClientError) -> bool {
    let ClientError::RpcError(rpc) = e else {
        return false;
    };
    matches!(
        rpc.endpoint_error(),
        Some(EndpointError::AddTransaction(AddTransactionError::StateConflict { message }))
            if message.contains("initial account commitment")
    )
}

/// Build one P2ID note per job, mint them all in a single transaction, and reply
/// to each waiter. The whole batch shares one tx id.
async fn process_batch(
    client: &mut Client<FilesystemKeyStore>,
    faucet: &Faucet,
    batch: Vec<MintJob>,
    provers: &Provers,
) {
    // Sync before building the transaction so the reference block and the faucet's
    // nonce are current — if a previous transaction failed to land, our local
    // state could otherwise be stale.
    if let Err(e) = client.sync_state().await {
        tracing::warn!(token = %faucet.symbol, error = %e, "pre-batch sync failed (continuing)");
    }

    // Refuse up front (503) if the faucet can't pay this transaction's fee.
    if let Err(msg) = ensure_fee_funds(client, faucet, provers).await {
        for job in batch {
            let _ = job.reply.send(Err(MintError::Unavailable(msg.clone())));
        }
        return;
    }

    let mut notes: Vec<Note> = Vec::with_capacity(batch.len());
    let mut pending: Vec<(MintJob, Note)> = Vec::with_capacity(batch.len());

    for job in batch {
        let asset = match FungibleAsset::new(faucet.id, job.amount) {
            Ok(asset) => asset,
            Err(e) => {
                let _ = job.reply.send(Err(MintError::Failed(format!("invalid amount: {e}"))));
                continue;
            }
        };
        let note: Note = match P2idNote::builder()
            .sender(faucet.id)
            .target(job.target)
            .asset(asset)
            .note_type(job.note_type)
            .generate_serial_number(client.rng())
            .build()
        {
            Ok(note) => note.into(),
            Err(e) => {
                let _ = job.reply.send(Err(MintError::Failed(format!("failed to build note: {e}"))));
                continue;
            }
        };
        notes.push(note.clone());
        pending.push((job, note));
    }

    if pending.is_empty() {
        return;
    }
    let count = pending.len();

    let request = match TransactionRequestBuilder::new().own_output_notes(notes).build() {
        Ok(request) => request,
        Err(e) => {
            let msg = format!("failed to build mint transaction: {e}");
            for (job, _) in pending {
                let _ = job.reply.send(Err(MintError::Failed(msg.clone())));
            }
            return;
        }
    };

    tracing::info!(token = %faucet.symbol, batch = count, "minting batch");
    match submit_batch(client, faucet.id, request, provers).await {
        Ok((tx_id, height)) => {
            let tx_hex = tx_id.to_hex();
            for (job, note) in pending {
                let note_b64 = if matches!(job.note_type, NoteType::Private) {
                    let details =
                        NoteDetails::new(note.assets().clone(), note.recipient().clone());
                    // The sync hint tells the recipient's wallet from which block (and under
                    // which tag) to look for the note — the height this mint was submitted at.
                    let file = NoteFile::ExpectedNote {
                        details,
                        sync_hint: NoteSyncHint::new(height, note.metadata().tag()),
                    };
                    Some(BASE64_STANDARD.encode(file.to_bytes()))
                } else {
                    None
                };
                let _ = job.reply.send(Ok(MintOutcome {
                    tx_id: tx_hex.clone(),
                    note_id: note.id().to_hex(),
                    note_b64,
                }));
            }
        }
        Err(e) => {
            let msg = format!("mint transaction failed: {e}");
            tracing::error!(token = %faucet.symbol, error = ?e, "mint failed");
            for (job, _) in pending {
                let _ = job.reply.send(Err(MintError::Failed(msg.clone())));
            }
        }
    }
}
