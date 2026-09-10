# Adding Parent Chains (L1 Blockchains)

This guide explains how to add support for new L1 blockchains (parent chains) to Coinshift. Parent chains are the Layer 1 blockchains that users can swap funds from/to via the two-way peg mechanism.

## Currently Supported Parent Chains

| Chain | Ticker | Default RPC Port | Confirmations |
|-------|--------|------------------|---------------|
| Bitcoin | BTC | 8332 | 6 |
| Bitcoin Cash | BCH | 8332 | 3 |
| Litecoin | LTC | 9332 | 3 |
| Bitcoin Signet | sBTC | 38332 | 3 |
| Bitcoin Regtest | rBTC | 18443 | 3 |

## Architecture Overview

The parent chain integration uses a modular architecture with several key components:

```
┌─────────────────────────────────────────────────────────────────┐
│                         Application Layer                        │
├─────────────────────────────────────────────────────────────────┤
│  app/gui/l1_config.rs     │  Per-chain RPC configuration UI     │
│  app/gui/swap/list.rs     │  Swap management with confirmations │
│  app/app.rs               │  Headless mode confirmation checks  │
├─────────────────────────────────────────────────────────────────┤
│                          Library Layer                           │
├─────────────────────────────────────────────────────────────────┤
│  lib/parent_chain_rpc.rs  │  Generic RPC client for all chains  │
│  lib/types/swap.rs        │  ParentChainType enum & helpers     │
│  lib/state/two_way_peg_data.rs │  Swap processing logic        │
└─────────────────────────────────────────────────────────────────┘
```

### Key Components

1. **`ParentChainType` enum** (`lib/types/swap.rs`)
   - Defines all supported parent chains
   - Provides chain-specific configuration (ports, confirmations, names)

2. **`ParentChainRpcClient`** (`lib/parent_chain_rpc.rs`)
   - Generic RPC client using Bitcoin Core JSON-RPC interface
   - Works with any Bitcoin-compatible blockchain

3. **`RpcConfig`** (`lib/parent_chain_rpc.rs`)
   - Stores RPC connection details (URL, user, password)
   - One config per parent chain, persisted to disk

4. **L1 Config UI** (`app/gui/l1_config.rs`)
   - GUI for configuring RPC connections per chain
   - Shows chain-specific hints and defaults

## RPC Compatibility Requirements

To be supported as a parent chain, a blockchain must implement these Bitcoin Core JSON-RPC methods:

### Required Methods

| Method | Purpose |
|--------|---------|
| `getblockchaininfo` | Get current block height and chain info |
| `getrawtransaction` | Fetch transaction details by txid (run the node with `-txindex=1`, otherwise only wallet transactions and unspent outputs resolve) |
| `scantxoutset` | Discover payments to the swap's L1 address across the whole UTXO set, independent of the node's wallet |

### Optional Methods

| Method | Purpose |
|--------|---------|
| `listunspent` | Wallet-only fallback for discovery on nodes without `scantxoutset`. It only sees addresses the node's wallet tracks; the node logs a warning when it is the sole source and finds nothing. Import the swap address as watch-only, or record the fill manually with `update_swap_l1_txid`. |

Discovery sees unspent outputs only. Once a fill's txid is known the node
tracks confirmations with `getrawtransaction` directly, so spending the fill
afterwards does not lose it.

### Response Format

The RPC responses must follow Bitcoin Core's JSON-RPC format:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": { ... },
  "error": null
}
```

Transaction responses (`getrawtransaction` with verbose=true) must include:
- `txid`: Transaction hash
- `confirmations`: Number of confirmations
- `blockheight`: Block height (optional)
- `vout`: Array of outputs with `value` and `scriptPubKey.address`
- `vin`: Array of inputs with `txid` and `vout` references

## Adding a New Parent Chain

Follow these steps to add support for a new blockchain:

### Step 1: Update ParentChainType Enum

Edit `lib/types/swap.rs` to add the new chain variant:

```rust
pub enum ParentChainType {
    BTC,
    BCH,
    LTC,
    Signet,
    Regtest,
    NewChain,  // Add your new chain here
}
```

### Step 2: Implement Helper Methods

Update all the `impl ParentChainType` methods:

```rust
impl ParentChainType {
    pub fn default_confirmations(&self) -> u32 {
        match self {
            // ... existing chains ...
            Self::NewChain => 6,  // Set appropriate confirmations
        }
    }

    pub fn to_bitcoin_network(&self) -> bitcoin::Network {
        match self {
            // ... existing chains ...
            Self::NewChain => bitcoin::Network::Bitcoin, // Or appropriate network
        }
    }

    pub fn default_rpc_port(&self) -> u16 {
        match self {
            // ... existing chains ...
            Self::NewChain => 8555,  // Your chain's default RPC port
        }
    }

    pub fn coin_name(&self) -> &'static str {
        match self {
            // ... existing chains ...
            Self::NewChain => "New Chain",
        }
    }

    pub fn sats_per_coin(&self) -> u64 {
        match self {
            // ... existing chains ...
            Self::NewChain => 100_000_000,  // Satoshis per coin
        }
    }

    pub fn ticker(&self) -> &'static str {
        match self {
            // ... existing chains ...
            Self::NewChain => "NEW",
        }
    }

    pub fn default_rpc_url_hint(&self) -> &'static str {
        match self {
            // ... existing chains ...
            Self::NewChain => "http://localhost:8555",
        }
    }

    pub fn all() -> &'static [ParentChainType] {
        &[
            Self::BTC,
            Self::BCH,
            Self::LTC,
            Self::Signet,
            Self::Regtest,
            Self::NewChain,  // Add to the list
        ]
    }
}
```

### Step 3: Update L1 Config UI Hints

Edit `app/gui/l1_config.rs` to add setup hints for your chain:

```rust
match self.selected_parent_chain {
    // ... existing chains ...
    ParentChainType::NewChain => {
        ui.label("Use NewChain Core with -txindex=1 for full transaction lookup.");
    }
}
```

### Step 4: Test RPC Compatibility

Verify your node's RPC compatibility:

```bash
# Test getblockchaininfo
curl -u user:password --data-binary \
  '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' \
  -H 'content-type: application/json' \
  http://localhost:PORT/

# Test getrawtransaction (replace TXID with a real transaction)
curl -u user:password --data-binary \
  '{"jsonrpc":"2.0","id":1,"method":"getrawtransaction","params":["TXID", true]}' \
  -H 'content-type: application/json' \
  http://localhost:PORT/

# Test listunspent (replace ADDRESS with a real address)
curl -u user:password --data-binary \
  '{"jsonrpc":"2.0","id":1,"method":"listunspent","params":[0, 999999, ["ADDRESS"]]}' \
  -H 'content-type: application/json' \
  http://localhost:PORT/
```

### Step 5: Handle Chain-Specific Quirks (If Needed)

If your chain has RPC differences, you may need to extend `ParentChainRpcClient`:

```rust
impl ParentChainRpcClient {
    // Add chain-specific method variants if needed
    pub fn get_transaction_for_chain(
        &self,
        txid: &str,
        chain: ParentChainType,
    ) -> Result<TransactionInfo, Error> {
        match chain {
            ParentChainType::NewChain => {
                // Custom handling for NewChain
            }
            _ => self.get_transaction(txid),
        }
    }
}
```

## Configuration Guide

### Node Setup Requirements

For each parent chain node, ensure:

1. **Transaction Index Enabled**: Run with `-txindex=1` flag
2. **RPC Enabled**: Configure `rpcuser`, `rpcpassword`, and `rpcport`
3. **Address Indexing** (optional): Some features work better with address index

### Example Node Configurations

#### Bitcoin Core (`bitcoin.conf`)
```ini
server=1
txindex=1
rpcuser=myuser
rpcpassword=mypassword
rpcport=8332
rpcallowip=127.0.0.1
```

#### Bitcoin Cash Node (`bitcoin.conf`)
```ini
server=1
txindex=1
rpcuser=myuser
rpcpassword=mypassword
rpcport=8332
rpcallowip=127.0.0.1
```

#### Litecoin Core (`litecoin.conf`)
```ini
server=1
txindex=1
rpcuser=myuser
rpcpassword=mypassword
rpcport=9332
rpcallowip=127.0.0.1
```

### Coinshift RPC Configuration

Configure the RPC connection in Coinshift:

1. Open the GUI and navigate to "L1 Config"
2. Select your parent chain from the dropdown
3. Enter the RPC URL (e.g., `http://localhost:8332`)
4. Enter RPC credentials if required
5. Click "Save" and verify the connection

Configuration is stored at:
- **Linux**: `~/.local/share/coinshift/l1_rpc_configs.json`
- **macOS**: `~/Library/Application Support/coinshift/l1_rpc_configs.json`
- **Windows**: `%APPDATA%\coinshift\l1_rpc_configs.json`

## Testing

### Unit Tests

Add tests for your chain in the existing test suite:

```rust
#[test]
fn test_new_chain_config() {
    let chain = ParentChainType::NewChain;
    assert_eq!(chain.default_rpc_port(), 8555);
    assert_eq!(chain.coin_name(), "New Chain");
    assert_eq!(chain.ticker(), "NEW");
}
```

### Integration Tests

For full integration testing:

1. Start your chain node in regtest/testnet mode
2. Configure RPC in Coinshift
3. Create a test swap targeting your chain
4. Verify transaction detection and confirmation tracking

### Regtest Testing

For local development, use regtest mode:

```bash
# Start node in regtest
./newchaind -regtest -txindex=1 -rpcuser=test -rpcpassword=test

# Generate some blocks
./newchain-cli -regtest generatetoaddress 101 <your_address>

# Create transactions for testing
./newchain-cli -regtest sendtoaddress <swap_address> <amount>
```

## Troubleshooting

### Common Issues

1. **"RPC error: Method not found"**
   - Your node may not support a required RPC method
   - Check if the method needs to be enabled via config

2. **"Transaction not found"**
   - Ensure `txindex=1` is set in your node config
   - The node may need to reindex: restart with `-reindex`

3. **"Connection refused"**
   - Check the node is running and RPC is enabled
   - Verify the port number matches your configuration

4. **"Invalid response format"**
   - The node's RPC response differs from Bitcoin Core format
   - May need custom handling in `ParentChainRpcClient`

### Debug Logging

Enable debug logging to troubleshoot RPC issues:

```bash
RUST_LOG=coinshift::parent_chain_rpc=debug ./coinshift-gui
```

## Contributing

When adding a new parent chain:

1. Follow the steps above
2. Add comprehensive tests
3. Update this documentation with chain-specific notes
4. Submit a pull request with:
   - Changes to `lib/types/swap.rs`
   - Changes to `app/gui/l1_config.rs`
   - Test cases
   - Documentation updates
