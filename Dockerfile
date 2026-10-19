# Stage 1: Build (Unchanged)
FROM rust:slim-bookworm AS builder

WORKDIR /usr/src/app

RUN apt-get update && apt-get install -y pkg-config libssl-dev libsqlite3-dev build-essential sqlite3

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations

RUN cat migrations/*.sql | sqlite3 build.db
ENV DATABASE_URL="sqlite://build.db"

RUN cargo build --release

# ---------------------------------------------------
# Stage 2: Runtime (Powered by Chainguard)
# ---------------------------------------------------
FROM cgr.dev/chainguard/wolfi-base:latest

WORKDIR /app

# Install runtime dependencies using Wolfi's package manager
# Note: wolfi uses 'libssl3' (or 'openssl') for SSL libraries, and 'sqlite' for sqlite3
RUN apk update && apk add --no-cache ca-certificates sqlite libssl3 openssl

# Copy the compiled binary and migrations from the builder
COPY --from=builder /usr/src/app/target/release/comment-service ./
COPY --from=builder /usr/src/app/migrations ./migrations

# Expose the Axum port
EXPOSE 3000

# Run the server (Chainguard defaults to a secure 'nonroot' user!)
CMD ["./comment-service"]