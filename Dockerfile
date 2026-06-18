# syntax=docker/dockerfile:1

# ---- build stage ----
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
# Heavy first build (miden deps). For faster rebuilds, introduce cargo-chef to
# cache the dependency layer; kept simple here.
RUN cargo build --release --locked

# ---- runtime stage ----
FROM debian:bookworm-slim
# ca-certificates: TLS to the Miden node/prover. curl: container healthcheck.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin app
WORKDIR /app

COPY --from=builder /src/target/release/inicio-faucet /usr/local/bin/inicio-faucet
COPY static ./static
# `faucets/` is provided at runtime via a volume; create it owned by `app` so a
# named volume inherits that ownership on first mount.
RUN mkdir -p /app/faucets && chown -R app:app /app
USER app

EXPOSE 8080
# Default command runs the HTTP service against ./faucet.toml. Override the args
# to run a subcommand, e.g. `create-faucet`.
ENTRYPOINT ["inicio-faucet"]
