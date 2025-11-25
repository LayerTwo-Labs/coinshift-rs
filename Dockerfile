# Stable Rust version, as of January 2025. 
FROM rust:1.84-slim-bookworm AS builder
WORKDIR /workspace
COPY . .

RUN cargo build --locked --release

# Runtime stage
FROM debian:bookworm-slim

COPY --from=builder /workspace/target/release/coinshift_app /bin/coinshift_app
COPY --from=builder /workspace/target/release/coinshift_app_cli /bin/coinshift_app_cli

# Verify we placed the binary in the right place, 
# and that it's executable.
RUN coinshift_app --help

ENTRYPOINT ["coinshift_app"]

