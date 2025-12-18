# Decoupling Plan: Remove Dependencies on bip300301_enforcer_integration_tests

## Overview
This document outlines the plan to decouple coinshift integration tests from `bip300301_enforcer_integration_tests`, making coinshift integration tests self-contained.

## Current Dependencies

### 1. From `bip300301_enforcer_integration_tests::util`
- `AbortOnDrop<T>` - Wrapper to abort tokio tasks on drop
- `BinPaths` (as `EnforcerBinPaths`) - Binary paths structure
- `VarError` - Error type for environment variable resolution
- `get_env_var` - Function to get environment variables
- `spawn_command_with_args` - Function to spawn commands with args
- `AsyncTrial` - Test trial wrapper for async tests

### 2. From `bip300301_enforcer_integration_tests::setup`
- `Mode` enum - (GetBlockTemplate, Mempool, NoMempool)
- `Network` enum - (Regtest, Signet)
- `PostSetup` (as `EnforcerPostSetup`) - Post-setup structure containing:
  - `out_dir: TempDir`
  - `tasks: AbortOnDrop<()>`
  - `bitcoin_cli: BitcoinCli`
  - `signet_miner: SignetMiner` (for signet)
  - `mining_address: Address`
  - `reserved_ports: ReservedPorts`
- `Sidechain` trait - Trait for sidechain implementations
- `setup` function (as `setup_enforcer`) - Main setup function

### 3. From `bip300301_enforcer_integration_tests::integration_test`
- `activate_sidechain` - Function to activate a sidechain
- `fund_enforcer` - Function to fund the enforcer
- `propose_sidechain` - Function to propose a sidechain
- `deposit` - Function to create a deposit
- `withdraw_succeed` - Function to test successful withdrawal
- `deposit_withdraw_roundtrip` - Complete deposit/withdraw test

### 4. From `bip300301_enforcer_integration_tests::mine`
- `mine` function - Function to mine blocks
- `MineError` - Error type for mining operations

## Files That Need Changes

1. **integration_tests/util.rs** - Remove `EnforcerBinPaths` dependency, create own `BinPaths`
2. **integration_tests/setup.rs** - Replicate enforcer setup logic
3. **integration_tests/integration_test.rs** - Replicate integration test helpers
4. **integration_tests/ibd.rs** - Update to use new modules
5. **integration_tests/unknown_withdrawal.rs** - Update to use new modules
6. **integration_tests/setup_test.rs** - Update to use new modules
7. **integration_tests/Cargo.toml** - Remove `bip300301_enforcer_integration_tests` dependency

## Implementation Plan

### Step 1: Create `enforcer_utils.rs` module
**Purpose**: Replicate utility functions from `bip300301_enforcer_integration_tests::util`

**Contents**:
- `AbortOnDrop<T>` - Copy implementation
- `VarError` - Copy implementation  
- `get_env_var` - Copy implementation
- `spawn_command_with_args` - Copy implementation
- `AsyncTrial` - Copy implementation
- `BinPaths` - Create our own version (no dependency on enforcer's BinPaths)

### Step 2: Create `enforcer_setup.rs` module
**Purpose**: Replicate enforcer setup logic

**Contents**:
- `Mode` enum - Copy from enforcer
- `Network` enum - Copy from enforcer
- `EnforcerPostSetup` struct - Replicate structure
- `setup_enforcer` function - Replicate setup logic
- Helper functions: `setup_directories`, `new_bitcoind`, `new_bitcoin_cli`, etc.

**Key Dependencies**:
- `bip300301_enforcer_lib::bins` - Still needed (this is the library, not integration tests)
- `bip300301_enforcer_lib::types` - Still needed

### Step 3: Create `enforcer_mine.rs` module
**Purpose**: Replicate mining functionality

**Contents**:
- `MineError` - Copy error type
- `mine` function - Copy mining logic
- Network-specific mining logic (Signet vs Regtest)

### Step 4: Create `enforcer_integration.rs` module
**Purpose**: Replicate integration test helpers

**Contents**:
- `propose_sidechain` - Function to propose sidechain
- `activate_sidechain` - Function to activate sidechain
- `fund_enforcer` - Function to fund enforcer
- `deposit` - Function to create deposits
- `withdraw_succeed` - Function to test withdrawals
- `deposit_withdraw_roundtrip` - Complete test function

### Step 5: Update `util.rs`
**Changes**:
- Remove dependency on `EnforcerBinPaths`
- Create our own `BinPaths` that includes all needed binaries
- Use our own `AbortOnDrop`, `VarError`, etc. from `enforcer_utils`

### Step 6: Update `setup.rs`
**Changes**:
- Import from `enforcer_setup` instead of `bip300301_enforcer_integration_tests::setup`
- Update `PostSetup` to work with our `EnforcerPostSetup`
- Update `Sidechain` trait usage

### Step 7: Update all test files
**Files to update**:
- `integration_test.rs` - Use new modules
- `ibd.rs` - Use new modules
- `unknown_withdrawal.rs` - Use new modules
- `setup_test.rs` - Use new modules

### Step 8: Update `Cargo.toml`
**Changes**:
- Remove `bip300301_enforcer_integration_tests` dependency
- Keep `bip300301_enforcer_lib` dependency (this is the library, not tests)

## Implementation Details

### BinPaths Structure
```rust
pub struct BinPaths {
    pub coinshift: PathBuf,
    pub bitcoind: PathBuf,
    pub bitcoin_cli: PathBuf,
    pub bitcoin_util: PathBuf,
    pub bip300301_enforcer: PathBuf,
    pub electrs: PathBuf,
    pub signet_miner: PathBuf,
}
```

### EnforcerPostSetup Structure
```rust
pub struct EnforcerPostSetup {
    pub out_dir: tempfile::TempDir,
    pub tasks: AbortOnDrop<()>,
    pub bitcoin_cli: bip300301_enforcer_lib::bins::BitcoinCli,
    pub signet_miner: Option<bip300301_enforcer_lib::bins::SignetMiner>,
    pub mining_address: bitcoin::Address,
    pub reserved_ports: ReservedPorts,
    // ... other fields as needed
}
```

## Testing Strategy

1. Start with one test file (e.g., `setup_test.rs`)
2. Create the new modules incrementally
3. Update the test file to use new modules
4. Verify the test still works
5. Repeat for other test files

## Benefits

1. **Independence**: Coinshift tests no longer depend on enforcer test infrastructure
2. **Maintainability**: Changes to enforcer tests won't break coinshift tests
3. **Clarity**: Clear separation of concerns
4. **Flexibility**: Can customize test utilities for coinshift-specific needs

## Risks & Mitigation

1. **Code Duplication**: Some code will be duplicated, but this is acceptable for independence
2. **Maintenance**: Need to keep enforcer setup logic in sync - mitigated by clear documentation
3. **Breaking Changes**: Changes to `bip300301_enforcer_lib` may still affect us - this is expected as it's the library we depend on

## Notes

- We still depend on `bip300301_enforcer_lib` - this is the actual library, not the integration tests
- The `Sidechain` trait might need to be replicated or we can create our own version
- Some functions might need to be adapted to work with coinshift's specific needs
