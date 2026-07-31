# Adding Parent Chains (L1 Blockchains)

How to add support for a new L1 blockchain that Coinshift can swap against.

## Currently supported

| Chain | Ticker | Asset | Finality | Amount unit |
|-------|--------|-------|----------|-------------|
| Bitcoin | BTC | native | block depth (6) | sats (8 dp) |
| Bitcoin Cash | BCH | native | block depth (3) | sats (8 dp) |
| Litecoin | LTC | native | block depth (3) | litoshis (8 dp) |
| Bitcoin Signet | sBTC | native | block depth (3) | sats (8 dp) |
| Bitcoin Regtest | rBTC | native | block depth (3) | sats (8 dp) |
| Solana | SOL | native | commitment (2) | lamports (9 dp) |
| Solana Devnet | dSOL | native | commitment (2) | lamports (9 dp) |
| USDC on Solana | USDC | SPL | commitment (2) | 6 dp |
| USDC on Solana Devnet | dUSDC | SPL | commitment (2) | 6 dp |

Every chain is selectable and can point at any endpoint you run. Configure with
`coinshift-cli set-l1-config`, the GUI's **L1 Config** panel, or
`coinshift_app init --l1 <chain>=<url>`.

## What a parent chain has to do

Coinshift never builds, signs or broadcasts an L1 transaction. It only *watches*
a parent chain, to answer one question per swap:

> Did address A receive amount N, how final is that payment, and how old is it?

That is the whole contract, and it is expressed as `ParentChainClient` in
`lib/parent_chain/mod.rs`:

```rust
#[async_trait]
pub trait ParentChainClient: Send + Sync {
    async fn identify(&self) -> Result<ChainIdentity, Error>;
    async fn tip(&self) -> Result<u64, Error>;
    async fn find_payments(&self, query: &PaymentQuery) -> Result<Vec<L1Payment>, Error>;
    async fn get_payment(&self, txid: &SwapTxId, query: &PaymentQuery)
        -> Result<Option<L1Payment>, Error>;
}
```

A chain needs **no** scripting, no HTLCs, no SPV and no merkle proofs. L1
verification is deliberately not part of consensus — see
`validate_swap_claim_consensus` in `lib/state/swap.rs` — so an adapter is an
observer and nothing more.

Two implementations are worth reading as examples:
`lib/parent_chain/bitcoin_core.rs` (UTXO model, block depth, JSON-RPC 1.0, basic
auth) and `lib/parent_chain/solana.rs` (account model, commitment levels,
JSON-RPC 2.0, header or query-param auth). Between them they cover most of the
ways a chain can differ.

## Steps

### 1. Append the variant

In `lib/types/swap.rs`, add to the **end** of `ParentChainType`.

> **Append only.** The enum is Borsh-encoded *by variant index* inside
> `TxData::SwapCreate`, which is part of the block body and therefore of the
> sidechain merkle root. It is also a bincode database key and a serde map key in
> `l1_rpc_configs.json`. Inserting or reordering a variant silently changes
> consensus encoding. `borsh_discriminants_are_stable` fails if you do — add your
> variant to that test's table.

### 2. Fill in the per-chain facts

The compiler points at every one of these, because they are exhaustive matches.
That is the design: a new chain cannot be half-added.

| Method | What it means |
|---|---|
| `decimals` | base units per coin, as a power of ten |
| `confirmation_model` | `BlockDepth` or `CommitmentLadder` |
| `txid_encoding` | `BitcoinHex` or `Base58` |
| `asset` | `Native`, or `Spl { mint, decimals }` |
| `default_confirmations` | how final before a swap is claimable |
| `max_l1_tx_age` | **in the chain's own unit** — blocks, slots, … |
| `default_swap_expiration_blocks` | in *L2* blocks; consensus-relevant |
| `bitcoin_network` | `Some` only if `bitcoin::Address` can parse its addresses |
| `validate_l1_address` | reject a typo before it costs someone a swap |
| `ticker`, `coin_name`, `display_name`, `setup_hint`, `default_rpc_url_hint` | display |

Add the variant to `all()` as well.

### 3. Teach identity about it

`lib/l1/identity.rs` decides whether an endpoint is serving the network the
operator configured. Add the network names a node may report, and the expected
genesis if one can be established.

Prefer *deriving* the expected genesis over hardcoding it. For the Bitcoin family
it is computed with `bitcoin::constants::genesis_block`, so there is no constant
to get wrong. Where nothing in the tree can derive it — Solana — verify the value
against the live cluster and say so in a comment. Never ship a hash from memory:
a wrong one marks a working chain `WrongChain` and silently stops its swaps.

If identity cannot be established exactly, say so rather than pretending. Bitcoin
Cash mainnet shares Bitcoin's genesis because it forked from it, so the two are
not separable by genesis at all. That is documented and tested rather than papered
over, and operators who want a stronger check can pin `expected_genesis`
themselves.

### 4. Write the adapter

Add a module under `lib/parent_chain/` and dispatch to it from `client_for`.

The two things most likely to be got wrong:

**Amounts.** Take the scale from `decimals()`, never a constant. If the chain
reports amounts as JSON floats, *round* rather than truncate: `0.29 * 1e8` is
`28999999.999999996`, and truncating that silently prevented swaps for that
amount from ever matching, until it was fixed.

**Finality versus age.** `L1Payment` carries `confirmations` and `age`
separately. They are the same quantity for Bitcoin and unrelated for a chain
whose finality is a commitment level. If your chain is a `CommitmentLadder`,
synthesize a monotone depth that never reaches `required_confirmations` before
true finality — including when `required_confirmations` is 1, which is the case
easiest to get wrong. `SwapState` is Borsh-encoded to the database, so it cannot
change shape to hold a commitment level directly.

### 5. Test it without a node

`lib/parent_chain/mock.rs` scripts payments, so swap detection can be tested with
no endpoint at all. For the adapter itself, assert against checked-in JSON
fixtures of real responses — much cheaper than standing up a server, and it
documents the shapes you depend on.

Cover the cases that bite: a payment for the wrong amount, one that is too old,
one that is unconfirmed, and whatever the chain's own trap is. For Solana that is
the fee payer's balance delta including the transaction fee; for SPL tokens it is
a look-alike mint.

For a live check, add it `#[ignore]`d — `devnet_identify_and_tip` is the pattern
— so the suite stays offline by default.

### 6. Node and ops requirements

Anything a chain needs at runtime that is not obvious — rate limits, an endpoint
with transaction history, an index that must be enabled — belongs in
`setup_hint`, so it reaches the operator, and in `docs/OPERATIONS.md`.

Bitcoin-family nodes need `-txindex=1` and RPC enabled:

```ini
server=1
txindex=1
rpcuser=myuser
rpcpassword=mypassword
rpcallowip=127.0.0.1
```

Solana needs an endpoint that retains signature history. The public clusters are
heavily rate limited — roughly 100 requests per 10s per IP, with
`getSignaturesForAddress` among the most throttled — so use your own provider for
anything beyond testing. API keys go in a header or query parameter via `L1Auth`.

## Trust model

Coinshift believes whatever a configured endpoint says about L1 payments. There
is no allowlist and no second opinion: point a chain only at a node you run or
trust. The blast radius is your own escrow — L1 verification is not part of
consensus, so a lying endpoint cannot affect anyone else's view of the chain.

## What this guide replaces

An earlier version told you to add an arm to seven `match` statements and edit
the GUI. That worked while every chain was a Bitcoin fork. It also omitted the
files that actually gated which chains could be used, so following it produced a
chain that compiled and then could not be selected. Both problems are gone: the
per-chain facts live in one place, the compiler finds them for you, and there is
no allowlist to update.
