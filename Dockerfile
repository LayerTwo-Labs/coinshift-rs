FROM rust:1.93-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
COPY . .

RUN cargo build --locked --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/coinshift_app /bin/coinshift_app
COPY --from=builder /workspace/target/release/coinshift_app_cli /bin/coinshift_app_cli

# Verify we placed the binary in the right place, 
# and that it's executable.
RUN coinshift_app --help

ENTRYPOINT ["coinshift_app"]

