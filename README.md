# inicio-faucet

Internal-only faucet service for the Miden devnet. Mints test tokens (P2ID notes)
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

## Deployment (Docker)

Recommended: build a container in CI, push to `ghcr.io`, and run it on internal
infra with a persistent volume for `faucets/` (the signing keys + per-faucet state).
The `Dockerfile` (multi-stage) and `docker-compose.yml` are included.

```
# 1. Create the four faucets into the data volume (writes .mac files):
docker compose run --rm faucet create-faucet --symbol TOKA --name "Token A" \
    --decimals 8 --max-supply 1000000000000000 --out faucets/toka.mac
#    ... repeat for TOKB / TOKC / TOKD ...

# 2. Put faucet.toml next to docker-compose.yml. For the container set
#    bind = "0.0.0.0:8080" and account_file = "faucets/<x>.mac".

# 3. Run it:
docker compose up -d            # /readyz drives the container healthcheck
```

Operational notes specific to this service:
- Secrets + state live in `faucets/` (the `.mac` keys are 0600). They're kept in a
  named volume here — never bake them into the image or commit them.
- Minting does STARK proving. For throughput, set `remote_prover_url =
  "https://tx-prover.devnet.miden.io"` in `faucet.toml` to offload it; otherwise it
  proves locally (give the host a few cores). Unset = local prover.
- No built-in auth / rate limit and it mints assets — keep it on an internal
  network / VPN / authenticated proxy, and use the per-token `max_mint_amount` cap.
- `SIGTERM` is handled, so `docker stop` / systemd / k8s shut it down gracefully.

## HTTP API

- `GET /api/tokens` -> `[{ symbol, name, decimals }]`
- `POST /api/mint` `{ token, address, amount, note_type }` where `note_type` is
  `"public"` (default; wallet auto-discovers on sync) or `"private"` (response
  includes `note_b64`, a serialized note file to import into the wallet).
  Returns `{ tx_id, note_id, note_b64? }`.
- `GET /health` (liveness), `GET /readyz` (readiness).

## Health check (CI)

`.github/workflows/faucet-healthcheck.yml` is a scheduled synthetic monitor (every
6 hours, plus manual `workflow_dispatch`). Each run builds the service, starts it
against the configured node (devnet by default), mints a real note, and asserts the
response carries a `tx_id`/`note_id` — i.e. that execute → prove → submit → apply all succeeded on
chain. A failure (mint broken / node unreachable) fails the run and notifies
watchers; the faucet log is uploaded as an artifact.

By default it creates a throwaway faucet per run and self-mints. Optional config:
- repo variable `FAUCET_TEST_RECIPIENT` — mint to a specific wallet instead of self.
- repo variable `MIDEN_RPC_ENDPOINT` — point at a different node.
- repo secret `FAUCET_MAC_B64` — base64 of a `.mac` to reuse one persistent test
  faucet across runs (set `FAUCET_TEST_RECIPIENT` too, since the id isn't derived).

When the faucet is deployed behind a URL, a lighter black-box variant can simply
`POST /api/mint` against the deployment instead of building it each run.

## Dependencies

The miden crates use the crates.io `miden-client = "0.15"` release, which speaks
the 0.15 protocol the devnet node at `rpc.devnet.miden.io` runs — so the client
handshakes cleanly and minted notes are compatible with the wallet/DEX on devnet.
All types come from `miden-client` re-exports. To target a different node, set the
`endpoint` in `faucet.toml` and bump `miden-client`/`miden-client-sqlite-store` to the
version that node runs.
