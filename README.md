# inicio-faucet

Internal-only faucet service for the Miden testnet. Mints test tokens (P2ID notes)
to wallet addresses so the team can exercise the wallet and the nProtocol DEX.

Four tokens, each backed by its own public fungible faucet account. A user enters an
address (and amount), plays a quick interaction game, and clicks Mint. The service
mints a P2ID note from the matching faucet and returns it.

## Architecture

The miden `Client` is `!Send`, so each faucet account runs on its own OS thread
(`current_thread` runtime + `LocalSet`) and is reached only over `Send` channels —
the same model the solver uses. One worker per faucet gives three things at once:

- `!Send` isolation (the client never crosses a thread boundary),
- nonce serialization (a faucet's transactions are strictly sequential), and
- batching (a worker drains its queue over `batch_window_ms` and mints all pending
  requests as a single transaction with N P2ID notes).

The axum HTTP server runs on a normal multi-thread runtime; handlers only touch
`Send` data and hand jobs to workers over `mpsc`, awaiting a `oneshot` reply.

```
src/
  main.rs          CLI (serve | create-faucet), runtime + worker wiring, startup gate
  config.rs        faucet.toml model
  worker.rs        per-faucet thread: import .mac, sync, batch-mint loop
  mint.rs          MintJob/MintOutcome, address + note-type parsing
  http.rs          axum router: /api/tokens, /api/mint, /health, /readyz, static
  create_faucet.rs create-faucet subcommand (pure construction; no network)
static/            frontend (token cards, mint form, interaction game)
```

## Setup

1. Create the four faucets (writes `.mac` files; no network needed):

   ```
   cargo run --release -- create-faucet --symbol TOKA --name "Token A" \
       --decimals 8 --max-supply 1000000000000 --out ./faucets/toka.mac
   # repeat for TOKB / TOKC / TOKD
   ```

2. `cp faucet.toml.example faucet.toml` and confirm the `[[tokens]]` paths.

3. Run the service:

   ```
   cargo run --release            # defaults to `serve`, reads ./faucet.toml
   ```

   Then open http://127.0.0.1:8080.

## HTTP API

- `GET /api/tokens` -> `[{ symbol, name, decimals }]`
- `POST /api/mint` `{ token, address, amount, note_type }` where `note_type` is
  `"public"` (default; wallet auto-discovers on sync) or `"private"` (response
  includes `note_b64`, a serialized note file to import into the wallet).
  Returns `{ tx_id, note_id, note_b64? }`.
- `GET /health` (liveness), `GET /readyz` (readiness).

## Dependencies

The miden crates are pinned (in `Cargo.toml`) to the same 0xMiden upstream `next`
commits the solver builds against, so minted notes are compatible with the live
testnet the wallet/DEX use. If the faucet is pointed at a fork-based private
testnet instead, repoint those git revs to the matching inicio-labs fork commits.
