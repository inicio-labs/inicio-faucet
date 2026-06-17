//! Per-faucet worker. Each faucet account gets its own OS thread running a
//! `current_thread` runtime + `LocalSet`, because the miden `Client` is `!Send`
//! and must never cross a thread boundary (same model the solver uses).
//!
//! This one design gives us three things at once:
//!  1. `!Send` isolation — the `Client` stays on its thread.
//!  2. Nonce serialization — one worker per faucet => its transactions are
//!     strictly sequential, so there are no in-flight nonce conflicts.
//!  3. Batching — the worker drains its queue over a time window and mints all
//!     pending requests as a single transaction with N P2ID notes.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use miden_client::account::{AccountFile, AccountId};
use miden_client::asset::FungibleAsset;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::{Client, Serializable};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_protocol::note::{Note, NoteAttachment, NoteDetails, NoteFile, NoteType};
use miden_standards::note::P2idNote;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::{RpcConfig, TokenConfig};
use crate::mint::{MintJob, MintOutcome};

/// Everything a worker thread needs. All fields are `Send` so the struct can be
/// moved into the spawned thread; the `!Send` `Client` is built *on* that thread.
pub struct WorkerParams {
    pub rpc: RpcConfig,
    pub token: TokenConfig,
    pub rx: mpsc::Receiver<MintJob>,
    pub cancel: CancellationToken,
    /// Reports readiness (client built + account imported + initial sync) or a
    /// startup error, so failures surface at the startup gate instead of in a
    /// detached thread.
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

async fn worker_loop(params: WorkerParams) {
    let WorkerParams { rpc, token, mut rx, cancel, ready, max_batch } = params;
    let symbol = token.symbol.clone();

    let (mut client, faucet_id) = match build_client(&rpc, &token).await {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(format!("[{symbol}] {e:#}")));
            return;
        }
    };
    tracing::info!(token = %symbol, faucet = %faucet_id, "faucet worker ready");

    if let Err(e) = client.sync_state().await {
        tracing::warn!(token = %symbol, error = %e, "initial sync failed (continuing)");
    }

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
            n = rx.recv_many(&mut buffer, max_batch) => {
                if n == 0 {
                    break; // channel closed
                }
                process_batch(&mut client, faucet_id, &symbol, std::mem::take(&mut buffer)).await;
            }
        }
    }
}

/// Build the miden client for this faucet (rpc + its own sqlite store + keystore)
/// and import the faucet account from its `.mac`.
async fn build_client(
    rpc: &RpcConfig,
    token: &TokenConfig,
) -> Result<(Client<FilesystemKeyStore>, AccountId)> {
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
    let rpc_client = Arc::new(GrpcClient::new(&endpoint, rpc.timeout_ms));

    let mut client = ClientBuilder::new()
        .rpc(rpc_client)
        .sqlite_store(PathBuf::from(&token.store_path))
        .authenticator(Arc::new(keystore))
        .build()
        .await
        .context("failed to build miden client")?;

    // Idempotent across restarts: only add the account if the store doesn't have it.
    if client.get_account(account_id).await?.is_none() {
        client
            .add_account(&account, false)
            .await
            .context("failed to add faucet account to store")?;
    }

    Ok((client, account_id))
}

/// Build one P2ID note per job, mint them all in a single transaction, and reply
/// to each waiter. The whole batch shares one tx id.
async fn process_batch(
    client: &mut Client<FilesystemKeyStore>,
    faucet_id: AccountId,
    symbol: &str,
    batch: Vec<MintJob>,
) {
    // Sync before building the transaction so the reference block and the faucet's
    // nonce are current — if a previous transaction failed to land, our local
    // state could otherwise be stale.
    if let Err(e) = client.sync_state().await {
        tracing::warn!(token = %symbol, error = %e, "pre-batch sync failed (continuing)");
    }

    let mut notes: Vec<Note> = Vec::with_capacity(batch.len());
    let mut pending: Vec<(MintJob, Note)> = Vec::with_capacity(batch.len());

    for job in batch {
        let asset = match FungibleAsset::new(faucet_id, job.amount) {
            Ok(asset) => asset,
            Err(e) => {
                let _ = job.reply.send(Err(format!("invalid amount: {e}")));
                continue;
            }
        };
        let note = match P2idNote::create(
            faucet_id,
            job.target,
            vec![asset.into()],
            job.note_type,
            NoteAttachment::default(),
            client.rng(),
        ) {
            Ok(note) => note,
            Err(e) => {
                let _ = job.reply.send(Err(format!("failed to build note: {e}")));
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
                let _ = job.reply.send(Err(msg.clone()));
            }
            return;
        }
    };

    tracing::info!(token = %symbol, batch = count, "minting batch");
    match client.submit_new_transaction(faucet_id, request).await {
        Ok(tx_id) => {
            let tx_hex = tx_id.to_hex();
            for (job, note) in pending {
                let note_b64 = if matches!(job.note_type, NoteType::Private) {
                    let details =
                        NoteDetails::new(note.assets().clone(), note.recipient().clone());
                    let file = NoteFile::NoteDetails {
                        details,
                        after_block_num: 0u32.into(),
                        tag: Some(note.metadata().tag()),
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
            tracing::error!(token = %symbol, error = %e, "mint failed");
            for (job, _) in pending {
                let _ = job.reply.send(Err(msg.clone()));
            }
        }
    }
}
