# Decoupling Action Plan - Quick Reference

## Summary
Remove dependency on `bip300301_enforcer_integration_tests` by replicating needed code in coinshift integration tests.

## Files to Create

### 1. `enforcer_utils.rs` (NEW)
**Replicate from**: `bip300301_enforcer_integration_tests::util`

**Contents**:
- `AbortOnDrop<T>` - Wrapper for tokio tasks
- `VarError` - Error type for env vars
- `get_env_var()` - Get environment variable
- `spawn_command_with_args()` - Spawn command with args
- `AsyncTrial<Fut>` - Async test trial wrapper
- `BinPaths` - Binary paths (include coinshift + all enforcer binaries)

### 2. `enforcer_setup.rs` (NEW)
**Replicate from**: `bip300301_enforcer_integration_tests::setup`

**Contents**:
- `Mode` enum (GetBlockTemplate, Mempool, NoMempool)
- `Network` enum (Regtest, Signet)
- `EnforcerPostSetup` struct
- `setup_enforcer()` function
- Helper functions for bitcoind, directories, etc.

### 3. `enforcer_mine.rs` (NEW)
**Replicate from**: `bip300301_enforcer_integration_tests::mine`

**Contents**:
- `MineError` enum
- `mine()` function
- Network-specific mining logic

### 4. `enforcer_integration.rs` (NEW)
**Replicate from**: `bip300301_enforcer_integration_tests::integration_test`

**Contents**:
- `propose_sidechain()`
- `activate_sidechain()`
- `fund_enforcer()`
- `deposit()`
- `withdraw_succeed()`
- `deposit_withdraw_roundtrip()`

## Files to Modify

### 1. `util.rs`
**Changes**:
- Remove: `use bip300301_enforcer_integration_tests::util::*`
- Add: `use crate::enforcer_utils::*`
- Update `BinPaths` to include all binaries (not just coinshift + EnforcerBinPaths)
- Update `BinPaths::from_env()` to read all env vars directly

### 2. `setup.rs`
**Changes**:
- Remove: `use bip300301_enforcer_integration_tests::*`
- Add: `use crate::enforcer_setup::*`
- Add: `use crate::enforcer_mine::*`
- Add: `use crate::enforcer_integration::*`
- Update all references to use new modules

### 3. `integration_test.rs`
**Changes**:
- Remove: `use bip300301_enforcer_integration_tests::*`
- Add: `use crate::enforcer_setup::*`
- Add: `use crate::enforcer_integration::*`
- Update `deposit_withdraw_roundtrip` to use new modules

### 4. `ibd.rs`
**Changes**:
- Remove: `use bip300301_enforcer_integration_tests::*`
- Add: `use crate::enforcer_setup::*`
- Add: `use crate::enforcer_integration::*`

### 5. `unknown_withdrawal.rs`
**Changes**:
- Remove: `use bip300301_enforcer_integration_tests::*`
- Add: `use crate::enforcer_setup::*`
- Add: `use crate::enforcer_integration::*`

### 6. `setup_test.rs`
**Changes**:
- Remove: `use bip300301_enforcer_integration_tests::*`
- Add: `use crate::enforcer_utils::*`
- Update `Mode` import

### 7. `main.rs`
**Changes**:
- Update module declarations to include new modules

### 8. `Cargo.toml`
**Changes**:
- Remove: `bip300301_enforcer_integration_tests = { workspace = true }`
- Remove from features: `bip300301_enforcer_integration_tests/openssl` and `bip300301_enforcer_integration_tests/rustls`
- Keep: `bip300301_enforcer_lib` (this is the library, not tests)

## Key Structures

### New BinPaths
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

### EnforcerPostSetup (simplified)
```rust
pub struct EnforcerPostSetup {
    pub out_dir: tempfile::TempDir,
    pub tasks: AbortOnDrop<()>,
    pub bitcoin_cli: bip300301_enforcer_lib::bins::BitcoinCli,
    pub signet_miner: Option<bip300301_enforcer_lib::bins::SignetMiner>,
    pub mining_address: bitcoin::Address,
    pub reserved_ports: ReservedPorts,
    pub network: Network,
    pub mode: Mode,
    // ... other fields as needed from original
}
```

## Implementation Order

1. **Create `enforcer_utils.rs`** - Start with utilities (AbortOnDrop, VarError, etc.)
2. **Update `util.rs`** - Use new utilities, update BinPaths
3. **Create `enforcer_setup.rs`** - Replicate setup logic
4. **Create `enforcer_mine.rs`** - Replicate mining logic
5. **Create `enforcer_integration.rs`** - Replicate integration helpers
6. **Update `setup.rs`** - Use new modules
7. **Update test files** - Update one at a time
8. **Update `Cargo.toml`** - Remove dependency
9. **Test** - Verify all tests still work

## Dependencies to Keep

✅ **Keep**: `bip300301_enforcer_lib` - This is the actual library
❌ **Remove**: `bip300301_enforcer_integration_tests` - This is test infrastructure

## Notes

- Code will be duplicated, but this provides independence
- Some functions may need adaptation for coinshift-specific needs
- The `Sidechain` trait might need to be replicated or adapted
- Test incrementally - update one file at a time and verify it works
