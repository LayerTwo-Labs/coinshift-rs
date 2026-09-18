default:
    @just --list

fmt:
    cargo fmt

build:
    cargo build

clippy:
    cargo clippy --all-targets --all-features

# Run integration tests. Pass runner options to select tests:
# `just test-it --tests deposit_withdraw_roundtrip`
test-it *args:
    scripts/run_integration_tests.sh {{ args }}
