# ---------- Build stage ----------
FROM rust:1.86-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates pkg-config protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/app

# Cache deps
COPY Cargo.toml Cargo.lock ./
# dummy sources so the deps layer caches
RUN mkdir src && echo "fn main(){}" > src/main.rs && touch src/lib.rs
RUN cargo build --release --locked

# Build real
COPY build.rs ./
COPY proto ./proto
COPY migrations ./migrations
COPY src ./src
# touch so cargo doesn't reuse the dummy build's mtimes
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# ---------- Runtime stage ----------
FROM gcr.io/distroless/cc-debian12:nonroot

WORKDIR /app
COPY --from=builder /usr/src/app/target/release/hexagonal-rs /app/service

EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/app/service"]
