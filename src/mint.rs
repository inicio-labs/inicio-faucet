//! Shared types crossing the HTTP layer (Send) and the faucet worker threads.
//!
//! An HTTP handler builds a [`MintJob`] and sends it to the right faucet worker
//! over an `mpsc` channel, then awaits the worker's reply on the embedded
//! `oneshot`. The `!Send` miden `Client` never appears here — only plain data.

use miden_client::account::{AccountId, Address};
use miden_client::address::AddressId;
use miden_client::note::NoteType;
use tokio::sync::oneshot;

/// A single mint request handed to a faucet worker.
#[derive(Debug)]
pub struct MintJob {
    pub target: AccountId,
    pub amount: u64,
    pub note_type: NoteType,
    /// Worker replies here with the outcome (or an error string).
    pub reply: oneshot::Sender<Result<MintOutcome, String>>,
}

/// What we hand back to the user after a successful mint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MintOutcome {
    /// Hex transaction id (the batch all share one tx).
    pub tx_id: String,
    /// Hex note id of this request's P2ID note.
    pub note_id: String,
    /// For Private notes only: base64 of the serialized `NoteFile` the recipient
    /// imports into their wallet. `None` for Public notes (auto-discovered on sync).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_b64: Option<String>,
}

/// Parse a user-supplied address string into an `AccountId`.
///
/// Accepts a raw hex account id (`0x...`) or a bech32 address (e.g. `mtst1...`),
/// matching the miden-cli's `parse_account_id` logic.
pub fn parse_address(input: &str) -> Result<AccountId, String> {
    let input = input.trim();
    if input.starts_with("0x") {
        return AccountId::from_hex(input).map_err(|e| format!("invalid hex account id: {e}"));
    }
    let (_network, address) =
        Address::decode(input).map_err(|e| format!("invalid bech32 address: {e}"))?;
    match address.id() {
        AddressId::AccountId(id) => Ok(id),
        _ => Err("address is not an account-id based address".to_string()),
    }
}

/// Parse the `note_type` field of a mint request.
pub fn parse_note_type(input: &str) -> Result<NoteType, String> {
    match input.to_ascii_lowercase().as_str() {
        "public" => Ok(NoteType::Public),
        "private" => Ok(NoteType::Private),
        other => Err(format!("note_type must be \"public\" or \"private\", got {other:?}")),
    }
}
