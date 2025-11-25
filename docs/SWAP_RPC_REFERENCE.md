# Swap RPC API Quick Reference

## Endpoints

### `create_swap`

Create a new L2 → L1 swap.

**Parameters:**
```json
{
  "parent_chain": "BTC" | "BCH" | "LTC",
  "l1_recipient_address": "string",
  "l1_amount_sats": 100000,
  "l2_recipient": "Address",
  "l2_amount_sats": 50000,
  "required_confirmations": 1,  // optional, defaults to chain default
  "fee_sats": 1000
}
```

**Response:**
```json
[
  "swap_id_hex",
  "txid"
]
```

**Example:**
```bash
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "create_swap",
    "params": {
      "parent_chain": "BTC",
      "l1_recipient_address": "bc1q...",
      "l1_amount_sats": 100000,
      "l2_recipient": "0x...",
      "l2_amount_sats": 50000,
      "required_confirmations": 1,
      "fee_sats": 1000
    }
  }'
```

### `update_swap_l1_txid`

Update a swap with the L1 transaction ID when detected.

**Parameters:**
```json
{
  "swap_id": "hex_string",
  "l1_txid_hex": "hex_string",
  "confirmations": 1
}
```

**Response:**
```json
null
```

**Example:**
```bash
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "update_swap_l1_txid",
    "params": {
      "swap_id": "abc123...",
      "l1_txid_hex": "def456...",
      "confirmations": 1
    }
  }'
```

### `get_swap_status`

Get the current status of a swap.

**Parameters:**
```json
{
  "swap_id": "hex_string"
}
```

**Response:**
```json
{
  "id": [32 bytes],
  "direction": "L2ToL1",
  "parent_chain": "BTC",
  "l1_txid": {...},
  "required_confirmations": 1,
  "state": "Pending" | "WaitingConfirmations" | "ReadyToClaim" | "Completed" | "Cancelled",
  "l2_recipient": "Address",
  "l2_amount": {...},
  "l1_recipient_address": "string",
  "l1_amount": {...},
  "created_at_height": 100,
  "expires_at_height": null
}
```

### `claim_swap`

Claim a swap that is ready to be claimed.

**Parameters:**
```json
{
  "swap_id": "hex_string"
}
```

**Response:**
```json
"txid"
```

### `list_swaps`

List all swaps in the system.

**Parameters:**
```json
[]
```

**Response:**
```json
[
  {
    "id": [...],
    "state": "Pending",
    ...
  },
  ...
]
```

### `list_swaps_by_recipient`

List all swaps for a specific recipient address.

**Parameters:**
```json
{
  "recipient": "Address"
}
```

**Response:**
```json
[
  {
    "id": [...],
    "state": "ReadyToClaim",
    ...
  },
  ...
]
```

## Swap States

- **Pending**: Swap created, waiting for L1 transaction
- **WaitingConfirmations**: L1 transaction detected, waiting for required confirmations
  - Contains: `current_confirmations`, `required_confirmations`
- **ReadyToClaim**: Required confirmations reached, can be claimed
- **Completed**: Swap claimed and finished
- **Cancelled**: Swap expired or cancelled

## Error Handling

Common errors:

- `Swap not found`: Invalid swap_id
- `Swap is not ready to claim`: Swap state is not `ReadyToClaim`
- `Cannot spend locked output`: Attempting to spend a swap-locked output
- `Swap ID mismatch`: Invalid swap ID in transaction
- `Insufficient funds`: Not enough L2 coins to create swap

## Using with CLI

If CLI commands are added:

```bash
# Create swap
coinshift_app_cli create-swap \
  --parent-chain BTC \
  --l1-recipient bc1q... \
  --l1-amount 100000 \
  --l2-recipient 0x... \
  --l2-amount 50000 \
  --fee 1000

# Get swap status
coinshift_app_cli get-swap-status <swap_id>

# Claim swap
coinshift_app_cli claim-swap <swap_id>

# List swaps
coinshift_app_cli list-swaps
```

