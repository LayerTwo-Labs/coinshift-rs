# L1 Parent Chains: Audit, Runtime Dependencies, and a Path to Solana

Branch to create: `feat/parent-chain-abstraction`

## Context

Coinshift is a BIP300 sidechain node with an L2↔L1 swap system. The README and
`docs/ADDING_PARENT_CHAINS.md` both advertise five supported parent chains
(BTC, BCH, LTC, Signet, Regtest). **That is not what the code does.** A second,
undocumented gate restricts the usable set to two chains pinned to two
hardcoded RPC endpoints, and the guide for adding chains omits the files that
actually control this.

Separately, we need to know what happens operationally when an L1 node is not
running — and the answer is surprising enough to be a production hazard today.

Finally, we want to add Solana (native SOL + SPL tokens such as USDC). Solana
is not a Bitcoin Core fork: account model, no UTXOs, base58 64-byte signatures,
9 decimals, commitment levels instead of confirmation depth, JSON-RPC 2.0 with
no basic auth. The existing "add a parent chain" playbook does not apply.

This plan covers three deliverables: (1) an accurate account of what is
supported, (2) a runtime-dependency fix so operators can reason about outages,
and (3) a chain-abstraction refactor that makes Solana — and chain #7 — additive.

---

## Part 1 — Audit: what is actually implemented today

### 1.1 The type says five chains

`lib/types/swap.rs:106-117` defines `ParentChainType { BTC, BCH, LTC, Signet, Regtest }`,
with one impl block (`:119-231`) carrying per-chain constants:

| Method | BTC | BCH | LTC | Signet | Regtest |
|---|---|---|---|---|---|
| `default_confirmations` | 6 | 3 | 3 | 3 | 3 |
| `default_swap_expiration_blocks` (L2) | 1008 | 432 | 432 | 432 | 50 |
| `max_l1_tx_age_blocks` (L1) | 2016 | 2016 | 8064 | 2016 | 500 |
| `default_rpc_port` | 8332 | 8332 | 9332 | 38332 | 18443 |
| `ticker` | BTC | BCH | LTC | sBTC | rBTC |

Two methods are dead code: `to_bitcoin_network()` (`:162`, returns a
`// Placeholder` for BCH/LTC) and `sats_per_coin()` (`:201`, hardcoded
`100_000_000`, never called — the real conversion is an inline
`value * 100_000_000.0` float multiply at `lib/parent_chain_rpc.rs:335`).

### 1.2 But only two chains are actually usable

`lib/parent_chain_rpc.rs:447-450`:

```rust
pub fn supported_l1_parent_chain_types() -> &'static [ParentChainType] {
    use ParentChainType::{BCH, Signet};
    &[Signet, BCH]
}
```

And `supported_l1_configs()` (`:425-444`) pins those two to **exact** URL + user
+ password strings — Signet at `http://localhost:38332` and BCH at
`http://173.230.135.236:28332`, both `user`/`password`. `is_supported_l1_config`
(`:475-485`) is exact string equality on all three fields.

Consequences:
- The GUI dropdowns (`app/gui/l1_config.rs:41,286`, `app/gui/swap/create.rs:21,56`)
  only ever list Signet and BCH; other variants hit `_ => continue` and vanish silently.
- The GUI's URL/user/password fields are `add_enabled(false, …)`
  (`app/gui/l1_config.rs:343-381`): *"Only predefined networks are supported."*
- Startup validation rejects anything else (see 2.2).
- A hardcoded third-party IP is shipped as the BCH endpoint.

**Answer to "which one is really supported today": Bitcoin Signet and Bitcoin Cash
testnet4, at two fixed endpoints. BTC, LTC, and Regtest exist in the type system
and in the docs, but cannot be configured through the GUI and will refuse to
start if written into the config file by hand.**

The CLI is *not* gated the same way — `cli/lib.rs:30-42` `parse_parent_chain`
accepts all five strings and `set-l1-config` (`cli/lib.rs:430-467`) writes
whatever you give it with no validation. That is how you brick a node (2.2).

### 1.3 What a "swap" actually is — no HTLC, no script, no SPV

This is the single most important architectural fact, and it is what makes
Solana tractable.

There is **no** hashlock, timelock, CLTV/CSV, P2SH/P2WSH, preimage, merkle proof,
or SPV anywhere in the repo. Coinshift never builds, signs, or broadcasts an L1
transaction — `sendrawtransaction` is never called. The swap is an
**RPC-oracle escrow**:

1. Alice's L2 coins are locked into `OutputContent::SwapPending { value, swap_id }`
   (`lib/types/transaction.rs:286-360`), tracked in the `locked_swap_outputs`
   LMDB table (`lib/state/mod.rs:133-146`).
2. Bob pays L1 **out of band**. The app doesn't build or even show him how.
3. Each node independently asks its own configured L1 RPC: *"did address A
   receive exactly N sats?"* (`lib/parent_chain_rpc.rs:319-413`), and flips the
   swap to `WaitingConfirmations` / `ReadyToClaim`.
4. Bob's L2 `SwapClaim` spends the locked output.

Bob's L1 payment has **zero cryptographic linkage** to the L2 swap. And per
`lib/state/swap.rs:351-363`, L1 verification is explicitly **not part of consensus**:

> *"Those are derived from each node's own parent-chain monitoring … and are
> **not part of consensus**, so `connect` trusts the block and advances the local
> state to match."*

`lib/state/block.rs:412-420` implements exactly that — a node that has never seen
the L1 payment will still set `ReadyToClaim` because a block contained the claim.

The security model is therefore already "trust your own RPC + trust the miner."
Solana's account model, ed25519, and lack of UTXOs are **irrelevant to L2
consensus**. Adding Solana is an observer/adapter problem, not a protocol redesign.

### 1.4 The L1 surface is one file, four RPC calls

`lib/parent_chain_rpc.rs` (688 lines) is the entire L1 integration. There is **no
trait abstraction** — `ParentChainRpcClient` is a concrete struct constructed
directly at five call sites. It speaks JSON-RPC **1.0** over `reqwest::blocking`
with HTTP basic auth, and calls exactly four methods:

| Method | Purpose |
|---|---|
| `getrawtransaction [txid, true]` | tx details + `confirmations` + `blockheight` |
| `listunspent [0, 999999, [addr]]` | discover txids touching an address |
| `getreceivedbyaddress` | fire-and-forget, result discarded |
| `getblockchaininfo` | tip height, and `chain` string for identity |

Address discovery via `listunspent` is doubly UTXO-bound: it requires a UTXO set,
requires the address to be in the node's wallet/watch-only set, and **a payment
that has since been spent becomes invisible**.

### 1.5 The docs are stale

`docs/ADDING_PARENT_CHAINS.md` gives a 5-step recipe naming two files to edit
(`lib/types/swap.rs`, `app/gui/l1_config.rs`). It never mentions
`supported_l1_parent_chain_types()` / `supported_l1_configs()` / `detect_chain_type()`
— without which a new chain cannot be selected and will fail startup validation.
It also misses `cli/lib.rs:30-42`, `app/gui/util.rs:35`, `app/gui/swap/create.rs:57`,
and the per-chain boolean flags in `app/cli.rs` / `app/main.rs`. And it is written
entirely on the premise that a new parent chain is a Bitcoin Core fork.

---

## Part 2 — Runtime dependencies: what you can run without

### 2.1 Summary

| Dependency | Required to boot? | Behaviour if down |
|---|---|---|
| **bip300301_enforcer gRPC** (default `http://localhost:50051`) | **Yes — hard fail** | Headless exits non-zero; GUI starts with a red banner + "Retry startup". No retry loop, no backoff. |
| Enforcer's `WalletService` | No | No `Miner`; `mine()`/`deposit()` fail with `NoCusfMainchainWalletClient`. |
| Swap L1 chain **not** in `l1_rpc_configs.json` | No | Nothing breaks. Swaps on that chain stay `Pending` forever. Chains are fully independent. |
| Swap L1 chain **configured but node down** | **Yes — hard fail** ⚠️ | Node refuses to start. This is the trap. |
| Swap L1 RPC dying **after** boot | n/a | Log-and-continue everywhere. No task dies. Swaps freeze at their current state. |

### 2.2 The trap: configured-but-down is fatal, unconfigured is fine

`app/app.rs:655-662` runs `validate_l1_config_file` **before the wallet and before
the enforcer probe**. `lib/parent_chain_rpc.rs:536-566` then, for every entry in
the file:

1. Requires an exact allowlist match → else `Error::UnsupportedL1Config`.
2. Makes a **live `getblockchaininfo` HTTP call** (10s timeout) and requires the
   reported `chain` string to match → else `Error::ChainMismatch`.

A stopped signet `bitcoind` therefore produces `Error::Http` →
`app::Error::L1ConfigValidation` (`app/app.rs:61-62`) → **the node will not start**.
Startup also blocks up to 10s per configured chain.

Missing file or invalid JSON returns `Ok(())` and boots fine. So the asymmetry is:
*unconfigured = optional; configured-but-down = fatal.*

Worse, `cli/lib.rs:430-467` `set-l1-config` writes arbitrary values to that file
with no validation — **running the documented CLI command can brick the next
startup** with "L1 config is not supported: only predefined networks are allowed."
Recovery is to delete or hand-edit `l1_rpc_configs.json`.

### 2.3 Once booted, per-chain L1 outages are genuinely safe

- Block-connect path (`lib/state/two_way_peg_data.rs:821-868`): missing config →
  `tracing::debug!` and skip; RPC error → `tracing::warn!` and continue. **Block
  connection never fails because an L1 RPC is down.**
- Headless poller (`app/app.rs:364-576`, 10s interval): per-swap config lookup,
  errors logged at debug, loop never breaks.
- BTC swaps are unaffected by a dead LTC node and vice versa.

Proven by `integration_tests/l1_rpc_dependency.rs`, which boots a full sidechain
+ enforcer with no L1 RPC configured and asserts the swap stays `Pending`.

### 2.4 Known operational sharp edges (all pre-existing)

- `app/gui/swap/list.rs:542-583` falls back to the **hardcoded** predefined configs
  when the file has no entry — so the GUI polls `localhost:38332` and
  `173.230.135.236:28332` even if the user never configured them. It also does
  `std::thread::spawn(...).join()` on the UI thread (`:636-648`), freezing the GUI
  up to 10s per pending swap.
- `lib/state/two_way_peg_data.rs:561-698` performs **blocking HTTP inside an open
  LMDB write transaction** during block connect, N+1 sequential calls per candidate.
  A slow or rate-limited endpoint stalls block processing.
- `App::mainchain_reachable` (`app/app.rs:130-138`) is documented as gating mining,
  is written by `l1_sync_task`, and is **never read**.
- No JSON-RPC method reports L1 or enforcer connectivity. The only surfaces are
  a manual GUI button (one chain at a time) and the Parent Chain → Info tab.
- Four duplicated L1 polling implementations; `RpcConfig` redefined 4×; the config
  path recomputed in 5 places.
- `SwapError::ChainNotConfigured` exists but is never constructed.

### 2.5 Proper setup, given today's code

**(a) Node with no swaps at all** — minimum viable:
1. `bitcoind` (mainchain, BIP300-patched) with ZMQ enabled.
2. `bip300301_enforcer` on `:50051`, sidechain proposed + activated.
3. `coinshift_app` — with **no** `l1_rpc_configs.json` (or the file deleted).

This boots and runs. Deposits, withdrawals, transfers, mining, P2P all work.
Swap *creation* works; swaps just never leave `Pending`.

**(b) Node with one L1 swap chain** — add a second `bitcoind` for the swap target
(see `scripts/regtest/2_start_parentchain.sh`) with `-txindex=1`, then configure it.
The endpoint must exactly match `supported_l1_configs()` or the node won't start.
**Start the L1 node before coinshift, every time.**

**(c) Node with several L1 chains** — not currently possible beyond Signet + BCH.
Each additional configured chain adds up to 10s of startup latency and one more
way for startup to fail.

**Operational rule today: never leave a chain configured whose node you don't
intend to keep running.** Part 3 removes this rule.

---

## Part 3 — Implementation plan

### 3.0 Design decisions

**Keep the on-disk and consensus formats stable.** `ParentChainType` is Borsh-encoded
by variant *index* inside `TxData::SwapCreate` (`lib/types/transaction.rs:517-540`),
which lands in the block body and merkle root; it is also a bincode DB key
(`lib/state/mod.rs:133-146`) and a **serde map key** in `l1_rpc_configs.json`.
Therefore: **new variants are appended last, forever; existing variants are never
renamed or reordered.** A unit test pins each variant's Borsh discriminant so a
future reorder fails CI instead of corrupting the merkle root.

**Do not change `SwapState`.** Solana finality is a categorical ladder
(processed / confirmed / finalized), not a depth. Rather than reshape a
Borsh-serialized enum, the Solana adapter *synthesizes* a monotone confirmation
number: not-found/processed → `0`, confirmed → `min(required-1, 1)`, finalized →
`required`. This never crosses the threshold before true finality (even when
`required_confirmations == 1`), keeps `update_swap_confirmations`'s
"only increase" rule working, and preserves a meaningful GUI progress bar.
Set `Solana::default_confirmations() = 2` so the ladder has room.

**Split finality from age.** Today `lib/state/two_way_peg_data.rs:592-604` uses
`confirmations` for *both* the finality gate and the `max_l1_tx_age_blocks` gate.
For Bitcoin these coincide; for Solana they cannot. `L1Payment` carries both
`confirmations` and `age`; the Bitcoin adapter sets `age == confirmations`, so
Bitcoin behaviour is byte-identical.

**Do not change the amount types.** Verified: in `bitcoin-units-0.1.100`,
`Amount::MAX == Amount(u64::MAX)` and `from_sat` is unchecked — so
`Swap.l1_amount: bitcoin::Amount` is already a fine opaque u64 of base units, and
lamports fit with vast headroom. Keep `l1_amount_sats: u64` in the RPC/CLI
signature (renaming breaks named-param clients); redocument it as "base units of
the parent chain asset". The real bug is only at the format/parse boundary, where
`Denomination::Bitcoin` hardcodes 1e8.

**The `ParentChainClient` trait is async.** The observer is a tokio task and the
GUI must not block its UI thread; `reqwest::blocking` inside an LMDB write txn is
being removed anyway. Drop the `blocking` feature from `lib/Cargo.toml` once the
last sync caller is gone.

**One SPL asset per `ParentChainType` variant**, not a variant + asset field.
Adding an `asset` field to `TxData::SwapCreate` changes the Borsh encoding of
*every* SwapCreate → merkle root change → hard fork. It would also make the
`(ParentChainType, SwapTxId)` uniqueness key wrong, since one Solana transaction
can move both SOL and USDC. Variants keep the mint compiled in, so a user can
never point a swap at a fake token calling itself USDC. Add a derived,
non-serialized `asset() -> L1Asset` accessor.

**Health gates *use*, not *boot*.** `validate_l1_config_file` is deleted. Boot is
parse-only and never does network I/O. A background health task owns per-chain
status, and the swap observer can only obtain a client via
`registry.verified_client(chain)`, which returns `Some` only when the chain is
`Healthy`, its identity probe is within TTL, and the config generation matches.
This is strictly stronger than the current boot check — it also catches a node
that goes wrong-chain *after* startup, which today's check cannot.

### 3.1 Phases

Each phase is independently reviewable and mergeable. **Phase 0 must precede
Phase 7 without exception** (see risk 2).

| # | Phase | Size | Key files |
|---|---|---|---|
| 0 | Chain-neutral seams, no behaviour change | ~400 LOC | `lib/types/swap.rs`, `cli/lib.rs`, `app/gui/util.rs` |
| 1 | Config consolidation + auth enum | M (1-2d) | new `lib/l1/config.rs` |
| 2 | `ParentChainClient` trait + Bitcoin impl + first L1 mock | ~600 LOC | new `lib/parent_chain/` |
| 3 | Identity + registry; unlock all chains + custom endpoints | L (3d) | new `lib/l1/{identity,registry}.rs` |
| 4 | Single swap observer; HTTP out of the write txn | L (4-5d) ⚠️ | new `lib/node/swap_observer.rs` |
| 5 | Enforcer boot backoff; runtime-installable `Miner` | M (1-2d) | `app/app.rs`, `app/cli.rs` |
| 6 | Connectivity status surface (RPC + CLI + GUI) | M (1-2d) | `rpc-api/lib.rs`, `app/rpc_server.rs` |
| 7 | Solana native SOL | ~800 LOC | new `lib/parent_chain/solana/` |
| 8 | SPL / USDC | ~400 LOC | `lib/parent_chain/solana/` |
| 9 | Docs + migration note | S | `docs/`, `README.md` |

Phases 0-5 already remove the startup trap and are shippable without any Solana work.

---

**Phase 0 — chain-neutral seams.** Add to `ParentChainType`: `decimals()`,
`confirmation_model() -> {BlockDepth, CommitmentLadder}`, `txid_encoding()`,
`validate_l1_address()`. Delete the dead `sats_per_coin()` and the dead-and-wrong
`to_bitcoin_network()`. Add `format_l1_amount`/`parse_l1_amount` and
`SwapTxId::parse_for_chain`/`display_for_chain` (base58 for Solana via
`bitcoin::base58` — already a transitive dep, **no new crate**; always dispatch on
the chain, never sniff the string).

Kill the silent-miss sites *by construction*: derive `strum::{Display, EnumString,
EnumIter, EnumCount}` (strum is already in `lib/Cargo.toml:52`), rebuild
`cli/lib.rs:30-42 parse_parent_chain` from `EnumString`, rebuild `all()` from
`EnumIter`, and replace the GUI's `_ => continue` arms
(`app/gui/l1_config.rs:287,296`, `app/gui/swap/create.rs:57,66`) with
`chain.display_name()`. `app/gui/util.rs:35 show_l1_amount` stops needing a match
entirely. Also fix `app/gui/swap/create.rs:305`, which resets the selector to
`ParentChainType::BTC` — a chain that isn't even selectable.

**Phase 1 — config consolidation.** Keep JSON (it must be readable before
`Node::new` opens the LMDB env, LMDB is single-process so the CLI could never
touch it while the node runs, and offline repair is a real need). New
`lib/l1/config.rs` with the **one** `default_path()` helper, a versioned
`L1ConfigFile`, and an auth enum ready for Solana:

```rust
pub enum L1Auth { None, Basic{user,password}, Bearer{token},
                  Header{name,value}, QueryParam{name,value} }
```

Custom `Deserialize` accepts the legacy flat `{url,user,password}` map and
upgrades in memory; `save()` writes atomically (temp + rename) with `0600` on
unix — the file holds passwords and will hold API keys, and all three current
writers use plain `fs::write` with default perms. Delete the 5 duplicate path
computations (`cli/lib.rs:14-19`, `app/main.rs:202-207`, `app/app.rs:656-659`,
`app/gui/l1_config.rs:65-70`, `app/gui/swap/list.rs:557-560`) and the 4 duplicate
`RpcConfig` definitions.

**Phase 2 — the trait.** New `lib/parent_chain/` module tree; keep
`lib/parent_chain_rpc.rs` as a `pub use` shim for one release so the call sites
don't all churn at once.

```rust
#[async_trait]
pub trait ParentChainClient: Send + Sync {
    async fn identify(&self) -> Result<ChainIdentity, Error>;
    async fn tip(&self) -> Result<u64, Error>;
    async fn find_payments(&self, q: &PaymentQuery) -> Result<Vec<L1Payment>, Error>;
    async fn get_payment(&self, txid: &SwapTxId, q: &PaymentQuery)
        -> Result<Option<L1Payment>, Error>;
}

pub struct L1Payment {
    pub txid: SwapTxId, pub txid_display: String,
    pub amount: u64, pub sender: Option<String>,
    pub confirmations: u32,   // finality measure (synthesized for Solana)
    pub age: u64,             // age measure, chain units
    pub included: bool, pub height: Option<u64>,
}
```

`L1Payment` replaces `TransactionInfo`/`Vout`/`ScriptPubKey`/`Vin` entirely.
The JSON-RPC version becomes per-adapter (Bitcoin 1.0, Solana **requires** 2.0).

This phase lands **the first L1 mock in the repo** (`lib/parent_chain/mock.rs`,
behind `#[cfg(any(test, feature = "test-utils"))]`), which finally lets us unit-test
`query_and_update_swap`'s matching / age-rejection / `is_new` / uniqueness logic —
all currently at zero coverage.

**Phase 3 — identity + registry, unlock everything.** `ChainIdentity` probe
replaces the allowlist: `getblockhash 0` plus a disambiguator. **BTC and BCH share
a genesis hash** (BCH forked at 478558) and LTC also reports `chain: "main"`, so
genesis alone is insufficient — use a checkpoint (`getblockhash 478559`) or
`getnetworkinfo().subversion`. Custom signet/regtest genesis varies, so
`expected_genesis` in the config overrides the table.

`L1Registry` (`Arc<RwLock<BTreeMap<ParentChainType, ChainEntry>>>`) is owned by
`Node`, replacing the `l1_rpc_config_path` plumbing (`lib/node/mod.rs:110,233`).
One writer: an `l1_health_task`. `L1ClientHandle::new` becomes crate-private so
`verified_client()` is the only way the observer can get a client — the
non-corruption invariant is structural, not by discipline.

Per-chain health: `Unconfigured | Disabled | Probing | Unreachable{..} |
WrongChain{..} | Healthy{..}`. A degraded chain pauses detection only; everything
else on the node is unaffected. `create_swap` rejects `Unconfigured`/`Disabled`/
`WrongChain` with the finally-constructed `SwapError::ChainNotConfigured`;
`Unreachable` is allowed with a warning since it's transient.

Delete from `lib/parent_chain_rpc.rs`: `supported_l1_configs`,
`is_supported_l1_config`, `Error::UnsupportedL1Config`, `detect_chain_type`,
`write_l1_config_file`, `validate_l1_config_file`, the dead `get_rpc_config`
placeholder, and their tests. `supported_l1_parent_chain_types()` → `all()`.
Re-enable the disabled GUI fields (`app/gui/l1_config.rs:343-381`) and generalize
`--l1-signet`/`--l1-bch-testnet4` to `--l1-config <chain>=<url>`.

`set-l1-config` becomes a **node RPC that probes identity before writing**, so
bricking is structurally impossible: the writer validates, the reader never
rejects. Keep a CLI `--offline` flag for repair when the node is down.

⚠️ **Ops note for the PR:** unlocking custom endpoints means a lying RPC can flip
a swap to `ReadyToClaim`. The blast radius is the operator's own escrow — L1
verification is non-consensus and `connect` trusts the block regardless — so this
is the *same* exposure as today; the allowlist was never a real mitigation. Add a
GUI "you are trusting this endpoint" warning.

**Phase 4 — single observer.** New `lib/node/swap_observer.rs`, spawned by
`Node::new`. Iteration shape: (a) `RoTxn` → snapshot the work list → drop the txn;
(b) all network I/O with **no txn held**, async client, bounded per-chain
concurrency, timeouts, `verified_client()` only; (c) one short `RwTxn` applying
results via the existing `update_swap_l1_txid`/`update_swap_confirmations` with
compare-and-set so a concurrent block connect isn't clobbered.

Removes: `query_and_update_swap` and the whole `rpc_config_getter` parameter chain
(`lib/state/two_way_peg_data.rs:560-705,895-900`, `lib/state/mod.rs:1998-2015`,
`lib/node/net_task.rs:89-135,418-437`); `App::swap_confirmation_check_task`
(`app/app.rs:364-576`); `SwapList::load_rpc_config` +
`check_confirmations_dynamically` including the `thread::spawn(..).join()` on the
UI thread (`app/gui/swap/list.rs:542-583,636-648`); and the inline RPC in
`app/gui/swap/detail.rs:767-805,855-895` (→ `poll_promise` against the node RPC).

**The height-based expiry block (`two_way_peg_data.rs:750-790`) stays inside the
write txn** — it is deterministic and consensus-relevant, and the `expired_swaps`
reversal table depends on it.

While here: `find_transactions_by_address_and_amount` currently issues
`listunspent` + N×`getrawtransaction` + a prevout lookup *per swap per tick*. Once
there is a single caller, add a per-tick per-chain `listunspent` cache, skip
terminal swap states, and rate-limit — otherwise the consolidated task hammers the
public BCH node harder than the four uncoordinated ones did.

**Phase 5 — enforcer boot.** Move the requirement from process-start time to
operation time. The node genuinely can't sync or mine without the enforcer, but
"can't mine" is not "can't run" — it can still serve wallet/swap RPC, keep peers,
and self-heal. Today a 6-second-slow enforcer is a crash loop, and there's no way
to express "start me, wait for my dependency" in systemd/compose without wrapper
scripts. Retry is also the only way a *late* `WalletService` can ever produce a
`Miner` — currently that's fixed forever at construction.

- `App::new` probes once with `--mainchain-connect-timeout` (default 30s); failure
  → WARN and continue, not `Err`.
- Delete the write-only `mainchain_reachable: Arc<AtomicBool>`
  (`app/app.rs:130-138,813-815,842`) and its `#[allow(dead_code)]`; replace with a
  `MainchainMonitor` folded into the existing `l1_sync_task` (`app/app.rs:191-234`),
  which already polls `get_chain_tip()` every 10s. On disconnect, re-run
  `check_proto_support` on a 1s→30s jittered backoff.
- `App.miner: Option<Arc<RwLock<Miner>>>` → `Arc<RwLock<Option<Miner>>>` so a
  `Miner` can be installed at runtime.
- Use status as a **fast pre-check with a good error message, never as the
  authority** — `mine()` still attempts `get_chain_tip()` and maps the real failure.
  A cached bool must not refuse a mine that would have worked.
- New flags: `--require-mainchain` (default false, restores today's hard exit) and
  `--strict-l1-config` (default false, for CI/supervised deploys that want fail-fast).

**Phase 6 — status surface.** New `get_connectivity_status` in `rpc-api/lib.rs`
(after `get_bmm_inclusions`, ~line 92; add types to the `ref_schemas[...]` list at
:21-25), impl in `app/rpc_server.rs`, plus `get_l1_config`/`set_l1_config` as real
RPC methods. Response carries a `mainchain` block (state, `validator_service`,
`wallet_service`, `can_mine`) and a per-chain `l1_chains` array (health, block
height, last success, `swaps_awaiting`). **Secrets are never included.** CLI gets
`get-connectivity-status`; the GUI L1 Config tab becomes a table of all chains with
a status dot each, and an enforcer dot goes in the bottom panel.

**Phase 7 — Solana native SOL.** Append `Solana`, `SolanaDevnet`.
`getSignaturesForAddress(A, {limit:25, until:<cached cursor>})` →
`getTransaction(sig, {encoding:"jsonParsed", maxSupportedTransactionVersion:0})` →
match on `meta.postBalances[i] - meta.preBalances[i] == amount` where `i` is A's
index in `accountKeys`. **Use the balance delta, not instruction parsing** — correct
for System transfers, CPI transfers, and multi-transfer txs alike. Require
`meta.err == null`. Reject if A is `accountKeys[0]` (fee payer), since fees make
the delta ambiguous. Sender = the signer with the largest negative delta.
Identity via `getGenesisHash`; `tip()` via `getSlot({commitment:"finalized"})`;
cheap re-poll via `getSignatureStatuses`. `max_l1_tx_age` = 432,000 slots (~2 days).

Rate limits are the real engineering here: the public endpoint is ~100 req/10s per
IP with `getSignaturesForAddress` among the most throttled. Mitigations: per-
`(chain,address)` `until:` cursor, an immutable-result LRU keyed by signature, a
`governor` limiter per endpoint (already a `lib` dep, precedent at
`lib/net/peer/request_queue.rs:178`), and 429 + `Retry-After` backoff treated as
"no information" (matching today's log-and-continue).

**Phase 8 — SPL / USDC.** Append `SolanaUsdc`, `SolanaDevnetUsdc` with the mint
compiled into `asset()`. `getTokenAccountsByOwner(A, {mint})` — **ask the RPC, do
not derive the ATA locally** (that needs a `findProgramAddress` PDA search) — then
watch the *token account's* signatures and diff `meta.pre/postTokenBalances` on
entries where `owner == A && mint == MINT`. **Assert mint equality; that is the
anti-spoof check.** Parse `uiTokenAmount.amount` (a decimal string of base units);
never touch `uiAmount`. A missing `preTokenBalances` entry means pre = 0 — the
create-ATA-and-transfer-in-one-tx case, which is the common first payment.

**Phase 9 — docs.** Rewrite `docs/ADDING_PARENT_CHAINS.md` around the trait (it
currently prescribes exactly the "add an arm to seven matches" pattern this work
replaces). Fix the README's supported-chains table. New `docs/OPERATIONS.md` with
the (a)/(b)/(c) deployment shapes from §2.5 and a fails-vs-degrades table.

### 3.2 Risks

1. ~~**Phase 4 is the highest-risk merge.**~~ **Partly mistaken — corrected during
   Phase 4.** The premise was that detection happening inside the connect txn
   meant a disconnect reversed it, so moving it out would lose that. It does not:
   `two_way_peg_data::disconnect` only ever reversed **expiry**, via the
   `expired_swaps` table. Detection — `l1_txid`, `WaitingConfirmations`,
   `ReadyToClaim` — was never reversed on reorg even while it ran inside the
   write transaction. Moving it out therefore loses no guarantee that existed.
   The real half of this risk was the other one: two writers to swap rows on a
   single-writer LMDB env. That is handled by snapshotting outside any
   transaction and re-reading under the write transaction before applying
   (compare-and-set), covered by
   `a_result_computed_against_a_stale_snapshot_is_discarded`.
2. **Phase 0 must precede Phase 7.** If Solana lands before `parse_l1_amount`, the
   GUI's `Amount::from_str_in(.., Denomination::Bitcoin)` at
   `app/gui/swap/create.rs:173-176` silently creates a 10×-wrong SOL swap.
3. **Removing the GUI's hardcoded fallback** (`app/gui/swap/list.rs:575-583`) means
   Signet/BCH stop working with zero config. Ship a one-time migration: if the
   config file is absent on first run of the new version, write the two legacy
   entries (unvalidated — they'll simply probe and report health).
4. **Default-non-fatal enforcer is a behaviour change** for supervisors relying on
   the non-zero exit. `--require-mainchain` plus a release note.
5. **Swap expiry is height-based and deterministic**, so a chain that is down for a
   week still expires its swaps. Do not change that (it's consensus) — surface it:
   WARN per tick and a `degraded` flag in the status RPC.
6. **`SwapId` does not commit to `parent_chain`** (`lib/types/swap.rs:32-51`), so
   the same sender/amount/recipient collides across chains and the second
   `SwapCreate` is rejected as "Swap already exists". Pre-existing (BTC vs Signet
   collide today) and practically unreachable given differing decimals. **Document,
   do not fix here** — the fix is a consensus change needing a height-activated
   `from_l2_to_l1_v2`.
7. **Identity pinning can lock out legitimate custom networks** (custom signet,
   custom regtest, BTC/BCH shared genesis). Hence the `expected_genesis` override,
   regtest chain-string-only matching, and a BCH checkpoint.
8. **Solana signature history depth.** Non-archival RPC nodes retain limited
   history; a node offline for days may not see an old fill. The `until:` cursor
   means normal operation never reaches back far. Document that Solana needs an
   endpoint with reasonable history, and keep the manual `update_swap_l1_txid`
   escape hatch working with base58 input.
9. **The integration tests use an invalid L1 address.**
   `bcrt1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh`
   (`integration_tests/l1_rpc_dependency.rs:67` and the other swap trials) is the
   mainnet BIP173 example with its HRP rewritten to `bcrt`, which breaks the
   bech32 checksum. It is harmless today because nothing validates addresses, but
   **wiring `validate_l1_address` into `create_swap` will fail those tests.**
   Replace the literal with a constructed address before that lands — see
   `validate_l1_address_checks_network_where_it_can` in `lib/types/swap.rs` for
   the pattern.
10. **The two L1-txid ingestion paths store opposite byte orders.** Automatic
    detection stores the node's txid via `SwapTxId::from_hex_rpc` (which
    reverses), while the manual `update_swap_l1_txid` RPC stores user input via
    `SwapTxId::from_hex` (which does not). The same transaction therefore has
    two different representations depending on how it was recorded, which means
    the `swaps_by_l1_txid` uniqueness index can be bypassed, and a
    `getrawtransaction` lookup succeeds for one and fails for the other. The
    `l1_txid_uniqueness` integration test only exercises the manual path, so it
    cannot catch this. **Not fixed here**: picking a canonical order rewrites
    stored swap records, so it needs its own change with a migration. Until
    then, `BitcoinCoreClient::get_payment` deliberately keeps sending
    `to_hex()`, matching what the existing pollers already do.

---

### 3.3 Phase 0 — implementation notes (done)

Landed on `feat/parent-chain-abstraction`. Three deliberate deviations from the
phase description above:

- **`to_bitcoin_network()` was replaced, not deleted.** `validate_l1_address`
  needs a network to check against, so it became
  `bitcoin_network() -> Option<bitcoin::Network>`, returning `None` for BCH and
  LTC instead of the old `// Placeholder` that claimed both were
  `Network::Bitcoin`. `sats_per_coin()` was deleted outright, replaced by
  `decimals()`.
- **`all()` stayed a `&'static [ParentChainType]` slice** rather than being
  rebuilt from `EnumIter`; callers want a slice. The safety property is provided
  instead by `all_variants_are_listed`, which asserts the slice matches both
  `ParentChainType::COUNT` and the `EnumIter` order — so it catches ordering
  drift as well as a missing variant.
- **`validate_l1_address` is not yet wired into `create_swap`.** Phase 0 is
  behaviour-neutral by design, and switching address validation on is a
  behaviour change that would fail the integration tests (risk 9). Wire it in
  with the Phase 3 work, once those literals are fixed.

Also fixed in passing: `app/gui/swap/create.rs` reset the chain selector to
`ParentChainType::BTC` after a successful swap — a chain that is not even
selectable — despite the comment claiming it kept the selection. It now preserves
whatever the user had chosen.

### 3.4 Phase 1 — implementation notes (done)

`lib/l1/config.rs` is now the only definition of an L1 endpoint and the only
place the config path is computed. Deleted: `parent_chain_rpc::RpcConfig`,
`parent_chain_rpc::LocalRpcConfigFile`, the private `RpcConfig` in
`app/gui/l1_config.rs`, and the two private `LocalRpcConfig` copies in
`app/app.rs` and `app/gui/swap/list.rs`; plus all six path computations and the
dead `parent_chain_rpc::get_rpc_config` placeholder.

Deliberate choices:

- **`url` stayed a `String`.** `url::Url` normalises on parse — it appends a
  trailing slash to an empty path — which would silently break the exact-string
  comparison in `is_supported_l1_config` and lock everyone out at startup. Once
  Phase 3 removes that allowlist the type can be tightened.
- **`is_supported_l1_config` compares only url + auth**, not the whole struct, so
  the new local-only knobs (`enabled`, `timeout_secs`, `poll_interval_secs`)
  cannot accidentally make a valid endpoint unsupported.
- **Legacy configs with a blank user become `L1Auth::None`.** The old client
  skipped basic auth entirely when the user was empty; mapping blank credentials
  to `Basic { user: "", .. }` would have started sending an empty
  Authorization header.
- **`ParentChainType` gained `Ord`/`PartialOrd`** so the config can use a
  `BTreeMap` and write keys in a stable order. Neither derive affects the Borsh
  or serde encoding, which the Phase 0 tests still pin.

Behaviour changes worth a release note:

1. **The file is rewritten in v2 form on the next save.** v2 is
   `{"version": 2, "chains": {…}}`; the old flat `{"Signet": {…}}` is still read
   and upgraded in memory. An older coinshift binary cannot read a v2 file — it
   would see no configured chains. Downgrades need the file removed.
2. **`get-l1-config` output is now the whole file object** (`version` + `chains`)
   rather than a bare chain map, and each entry carries an `auth` object instead
   of flat `user`/`password`.
3. **`set-l1-config` now fails on a malformed config file** instead of silently
   discarding every other chain's entry, and writes atomically with mode 0600.
   The file holds RPC passwords and will hold API keys; all three previous
   writers used plain `fs::write` with default permissions.

Still deliberately unchanged: the startup trap (Phase 3), the four duplicated
pollers (Phase 4), and the GUI's hardcoded predefined-config fallback in
`app/gui/swap/list.rs` (risk 3).

### 3.5 Phase 2 — implementation notes (done)

`lib/parent_chain/` holds the trait, the chain-neutral payment types, the
Bitcoin Core adapter and the first L1 mock in the repo. `lib/parent_chain_rpc.rs`
is now only the allowlist plus startup validation, re-exporting the client under
its old name so call sites did not all have to churn at once.

`L1Payment` replaces `TransactionInfo`/`Vout`/`ScriptPubKey`/`Vin` at the swap
boundary, and carries **`confirmations` and `age` as separate fields**. They are
the same quantity for Bitcoin-family chains, which is why the old code could use
one value for both the finality gate and the `max_l1_tx_age` cutoff, but they are
unrelated for a chain whose finality is a commitment level. The Bitcoin adapter
sets `age == confirmations`, so behaviour is unchanged.

**Deviation: the trait is synchronous, not async as §3.0 stated.** Its only
caller today is `query_and_update_swap`, which runs inside an LMDB write
transaction on a synchronous path — an `async` trait could not be awaited there,
and blocking on the runtime from inside it panics. Phase 4 moves observation into
its own task and flips the trait to `async` with it; no signature other than the
`async`/`.await` pair differs. `L1Asset` was also left out until Phase 7 rather
than added speculatively.

**Bug fixed: amounts were truncated, not rounded.** `(vout.value * 100_000_000.0)
as u64` turned `0.29` into 28_999_999 satoshis, because 0.29 is not representable
in binary floating point. Any swap for such an amount could never match. The
conversion now rounds, and takes its scale from `ParentChainType::decimals()`
rather than a hardcoded 1e8. Pinned by
`coins_to_base_units_rounds_instead_of_truncating`.

**Bug found and left alone: risk 10**, the two ingestion paths storing opposite
txid byte orders. Fixing it rewrites stored swap records, so it needs its own
change.

`app/gui/swap/detail.rs` no longer reimplements address and amount matching with
its own copy of the 1e8 constant — it asks the adapter via `matches_query`.

New coverage: `swap_detection_tests` in `lib/state/two_way_peg_data.rs` drives
`query_and_update_swap` through the mock across eight cases — detection, the
confirmation threshold, the age cutoff, txid uniqueness, monotonic
confirmations, an unreachable endpoint, an unconfigured chain, and a wrong
amount. None of that had any test at all before.

### 3.6 Phase 3 — implementation notes (done)

**The startup trap is gone.** `App::new` performs no L1 network I/O at all.
`lib/l1/registry.rs` reads the config file, and a background task on `Node`
probes each endpoint every 30s and records its health.
`L1Registry::verified_client` is the only way to obtain a client, and returns
`None` unless the chain last probed healthy within `HEALTH_TTL`. This is
*stronger* than the check it replaces: it also catches an endpoint that starts
serving the wrong network after startup, which a boot-time check cannot.

**The allowlist is deleted**, and with it `lib/parent_chain_rpc.rs` entirely.
`supported_l1_configs`, `is_supported_l1_config`, `detect_chain_type`,
`validate_l1_config_file`, `write_l1_config_file` and
`Error::{ChainMismatch, UnsupportedL1Config}` are gone. All five chains are now
selectable, and the GUI's URL/user/password fields are editable, carrying a
warning that Coinshift trusts whatever the endpoint says about L1 payments.

**Identity uses computed genesis hashes, not remembered ones.**
`lib/l1/identity.rs` derives the expected genesis from
`bitcoin::constants::genesis_block(network)` for the chains the crate models
(BTC, Signet, Regtest), so there are no hand-copied constants to get wrong.
`expected_genesis` in the config overrides it, which is how custom signet
(`-signetchallenge`) and non-default regtest are supported.

Deliberate limitation, tested and documented rather than papered over: **BCH and
LTC fall back to matching the reported network name.** The `bitcoin` crate cannot
supply their genesis, and BCH mainnet shares Bitcoin's genesis anyway because it
forked from it — so genesis could not separate them even if we had the value.
The plan suggested pinning the BCH fork block as a checkpoint; that needs a
specific block hash which is not derivable from anything in the tree, and
shipping one from memory risks marking a working chain `WrongChain`. Operators
who want the stronger check can set `expected_genesis` themselves. What *is*
caught conclusively is the realistic mistake: a Signet or Regtest swap pointed at
a mainnet node.

**Deviation:** `set-l1-config` stayed a CLI-local command rather than becoming a
node RPC (§3.1 Phase 3 called for the RPC; Phase 6 adds the RPC surface). It now
probes before writing — refusing a wrong-network endpoint outright, and warning
but still writing for one that is merely unreachable, since configuring before
starting a node is normal. That is enough, because with startup no longer fatal
a bad config can no longer brick anything.

Behaviour changes worth a release note:

1. **A configured-but-down parent chain no longer stops the node from starting.**
   Detection for that chain pauses and resumes on its own. `--strict-l1-config`
   restores fail-fast for supervised deployments.
2. **The GUI no longer silently polls two hardcoded endpoints** for chains the
   user never configured (one of them a third-party IP). On first run with no
   config file, those two entries are written out once with a WARN, so existing
   users keep working and can now see, edit, or delete them.
3. **`--l1-signet` and `--l1-bch-testnet4` are replaced by `--l1 <chain>=<url>`**,
   repeatable, with credentials in the URL userinfo.
4. **`create_swap` now fails** with `SwapError::ChainNotConfigured` (finally
   constructed, having been dead code since it was written) for a chain that is
   unconfigured, disabled, or serving the wrong network. A merely unreachable
   chain is still allowed — the node being briefly down says nothing about a swap
   that will be filled minutes later.

Risk 3 is resolved by the migration in (2). Risk 7 is handled by the
`expected_genesis` override plus name-only matching for BCH/LTC.

### 3.7 Phase 4 — implementation notes (done)

`lib/node/swap_observer.rs` is now the only place Coinshift watches a parent
chain. A round snapshots the work list under a read transaction and drops it,
does all network I/O with **no transaction held** and all chains concurrently,
then applies results in one short write transaction.

**The write-transaction problem is gone.** Detection used to make blocking HTTP
calls while holding the LMDB write lock — `listunspent` plus a
`getrawtransaction` per candidate, per pending swap, per block. Against a remote
or rate-limited endpoint that is seconds during which nothing else in the node
can write. `process_coinshift_transactions` is now `process_swap_expiries` and
does only the deterministic, consensus-relevant expiry work; the whole
`client_getter` parameter chain through `connect`, `connect_two_way_peg_data`,
`connect_tip_` and `net_task` is deleted.

**Three duplicate pollers deleted:** `App::swap_confirmation_check_task`,
and `SwapList::{check_confirmations_dynamically, load_rpc_config}` — the latter
including the `std::thread::spawn(...).join()` that blocked the egui thread for
up to ten seconds per pending swap, every ten seconds.

**The trait is now async**, as §3.0 intended and Phase 2 deferred. With exactly
one caller left this was the cheapest possible moment to flip it. `reqwest`'s
`blocking` feature is dropped from `lib`.

**Correction to risk 1.** The plan assumed detection was reorg-reversed because
it ran inside the connect transaction, and that moving it out would lose that.
It was not: `disconnect` only ever reversed expiry. Detection was never reversed
on reorg, then or now. So this phase changes *when* detection happens but takes
away no guarantee — which is why the planned "re-verify swaps whose validation
block left the active chain" work is not here. Reorg-aware re-verification is
worth doing on its own merits, but it would be a new guarantee, not a restored
one, and belongs in its own change.

The half of risk 1 that was real — two writers to swap rows on a single-writer
environment — is handled by compare-and-set: the observer re-reads each swap
under the write transaction and skips it if the state moved since the snapshot,
so a claim or expiry landing mid-round always wins. Covered by
`a_result_computed_against_a_stale_snapshot_is_discarded`.

Known remaining, both deliberate: the swap detail view's two **click-triggered**
fetches still block briefly via `runtime.block_on` (bounded by the endpoint
timeout, and no longer per-frame), and `app/gui/l1_config.rs` still has its own
hand-rolled `getblockchaininfo` call for the "test connection" button. Phase 6
replaces that panel with a registry-driven status table, which removes it.

### 3.8 Running the integration tests

`scripts/run_integration_tests.sh` sets everything up. Two things had to be
fixed before it worked, and two bugs in phases 0–4 fell out of it.

**The runner downloaded the wrong enforcer.** It fetched
`bip300301-enforcer-latest-*` from releases.drivechain.info, but the tests drive
that binary with the harness from `bip300301_enforcer_integration_tests`, which
Cargo.toml pins to `a9ca43d`. The two must agree on the CLI and no longer do:
the pinned rev takes `--serve-json-rpc-addr`, the published latest renamed it to
`--serve-rpc-addr`, so every test died at enforcer startup with
`unexpected argument`. The script now reads the rev out of Cargo.toml and builds
the enforcer from source at exactly that commit, so the two cannot drift again.
(Only `latest` is published, so pinning a download was never an option.)

Also worth knowing on Apple Silicon: `bitcoin-patched` publishes an
x86_64-darwin build only, so `bitcoind` runs under Rosetta 2. The script checks
for it.

**Two bugs in the phases above, both found by thinking about these tests:**

1. **Phase 3's `create_swap` gate was too strict.** It refused swaps on any
   chain that was not configured — but every swap integration test creates
   swaps on `Regtest` with no L1 config at all, and more importantly, a swap on
   an unconfigured chain is a *supported workflow*: the creator fills it with
   `update_swap_l1_txid`, which is exactly what `l1_rpc_dependency` and
   `l1_verification_rpc_only` exercise and what
   `docs/COINSHIFT_HOW_IT_WORKS.md` documents. The gate now refuses only
   `WrongChain`, where accepting a payment would corrupt state. Pinned by
   `only_a_wrong_network_blocks_swap_creation`.
2. **Phase 3's config seeding wrote to a shared location.** The L1 config path
   is global (`dirs::data_dir()`), not per-datadir, so seeding it on first run
   meant *any* run of the app — the integration suite included — silently
   creating a config file on the machine containing a third-party endpoint the
   operator never asked for. Replaced with a warning that names the exact
   command to configure each chain. Existing users get a loud pointer instead of
   silent breakage, and the node no longer configures itself behind their back.

The suite passes with phases 0–4 applied: 10 trials, 0 failures,
`multi_node_verification` ignored as flaky (issue #76) exactly as on `main`.

The worry that prompted this — that Phase 4 broke `l1_verification_rpc_only` and
`confirmations_block_inclusion` — was unfounded: both drive swap state through
the manual `update_swap_l1_txid` RPC, not through detection, so moving detection
into the observer does not touch them. No `wait_for_swap_state` helper is
needed. The genuinely detection-dependent assertion,
`l1_rpc_dependency`'s "stays Pending without RPC config", holds a fortiori now
that an unconfigured chain yields no client at all.

**Note on the config path.** That it is global rather than per-datadir is a
pre-existing wart worth fixing on its own: `README.md` documents running several
instances with separate datadirs, and they all share one L1 config today.

### 3.9 Phase 5 — implementation notes (done)

**The enforcer is no longer a boot requirement.** `App::new` still probes it,
but a failure now warns and continues instead of aborting. The node genuinely
cannot sync or mine without the enforcer, but "cannot mine" is not "cannot run":
it can still serve wallet and swap RPC, keep peers, and recover by itself. The
old behaviour turned an enforcer that was merely slow to start into a crash
loop, and gave no way to express "start me, wait for my dependency" to a
supervisor. `--require-mainchain` restores it, and
`--mainchain-connect-timeout` (default 30s, was a hardcoded 5s) controls how
long startup waits.

**`mainchain_reachable` is deleted.** It was an `Arc<AtomicBool>` documented as
gating mining, written by the L1 sync task, and **never read anywhere** — it
carried `#[allow(dead_code)]` from the day it was added. `crate::mainchain::MainchainMonitor`
replaces it with something that is actually consulted: it drives reconnection
backoff and is displayed in the GUI's bottom panel, so an operator can see that
mining is unavailable rather than inferring it from a failed action.

Note what the monitor deliberately does **not** do: gate `mine()`. Mining still
asks the enforcer and maps the real failure to `MainchainUnreachable`. A cached
status must never be able to refuse an operation that would have succeeded —
which is the trap the original boolean was set up for but never sprung, having
never been read.

**The miner is installable at runtime.** `App.miner` moves from
`Option<Arc<RwLock<Miner>>>` to `Arc<RwLock<Option<Miner>>>`. The enforcer's
wallet service is optional, and without it there is no miner; previously that
was decided once at construction and fixed forever, so an enforcer that gained
its wallet service later still could not mine until the node restarted. The sync
task now re-probes while disconnected and installs a miner when one becomes
possible.

Reconnection rides on the existing `l1_sync_task`, which already polled
`get_chain_tip` every ten seconds, rather than adding a task; on failure it
backs off 1s → 30s.

Verified against the integration suite: 10 passed, 0 failed — the enforcer boot
path is exactly what those tests exercise.

### 3.10 Phase 6 — implementation notes (done)

Three new RPC methods in `rpc-api/lib.rs`, implemented in `app/rpc_server.rs`:

- `get_connectivity_status` — the enforcer's reachability (with `can_mine`,
  which is `connected && wallet_service`) and every parent chain's health, plus
  `swaps_awaiting` per chain so an operator can tell whether an unhealthy chain
  actually matters right now.
- `get_l1_config` — the config with credentials removed.
- `set_l1_config` — verifies the endpoint before writing, then reloads and
  re-probes so the change takes effect without a restart.

**Credentials never leave the node.** `sanitize_url` strips userinfo from any
endpoint before it is returned, and an unparseable URL renders as
`<unparseable url>` rather than being echoed verbatim — the config file can
contain a password in the URL, and both of these APIs are readable by anyone
with RPC access. `auth` is reported as a scheme name only.

**This closes the Phase 3 deviation.** `set-l1-config` was left as a CLI-local
file write there; it now goes through the node, which is what lets it validate
and hot-reload. `get-l1-config` follows, and `get-connectivity-status` is new.

**The last duplicate RPC implementation is gone.** `app/gui/l1_config.rs` had
its own hand-rolled `getblockchaininfo` with a blocking `reqwest` client — a
fourth copy of the call — that only ran when a button was pressed. The panel now
renders a table of every chain's live health from the registry. `reqwest` is no
longer a dependency of `app` at all.

**Also closed here: risk 9.** `validate_l1_address` was added in Phase 0 but
never wired up, because doing so would have failed the integration tests, whose
L1 address literal is the mainnet BIP173 example with its HRP rewritten to
`bcrt` — which breaks the bech32 checksum. Those literals are now a genuinely
valid regtest address (`bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080`, derived
with the `bitcoin` crate rather than typed from memory), and `create_swap`
validates the recipient. A typo'd address previously made a swap unfillable
until it expired.

Suite: 10 passed, 0 failed.

### 3.11 Phase 7 — implementation notes (done)

`Solana` and `SolanaDevnet` are appended to `ParentChainType` (discriminants 5
and 6, pinned by `borsh_discriminants_are_stable`), and
`lib/parent_chain/solana.rs` implements the trait for them.

**Genesis hashes are verified, not remembered.** Both were queried from the live
clusters: mainnet-beta `5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d`, devnet
`EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG`. Solana has no
`getblockchaininfo`-style network name, so identity rests entirely on genesis —
which, unlike Bitcoin Cash, is exact. `expected_genesis` changed from
`Option<bitcoin::BlockHash>` to `Option<String>` to hold them; existing configs
are unaffected because `BlockHash` already serialized as a hex string.

**Payments are balance deltas, not outputs.** `credited_lamports` diffs
`preBalances`/`postBalances` for the recipient's index in `accountKeys`, which
is correct for plain transfers, CPI transfers and multi-transfer transactions
alike — instruction parsing would not be. A transaction where the recipient is
the fee payer is rejected, because the fee is folded into that account's delta
and no exact amount can be attributed. Failed transactions credit nobody.

**Finality is synthesized.** `ladder()` maps `finalized` → `required`,
`confirmed` → `min(required-1, 1)`, anything else → 0. `SwapState` is
Borsh-encoded to the database and `required_confirmations` is inside the block
body, so neither could change shape to carry a commitment level. The mapping is
monotone and never reaches `required` before true finality — including when
`required` is 1, the case that would be easiest to get wrong, where `confirmed`
reports 0. `default_confirmations` is 2 for Solana so the ladder has a rung to
show progress on.

Age is measured in slots (`max_l1_tx_age` = 432,000, ~2 days), which is the
whole reason `L1Payment` splits `age` from `confirmations`: for this chain they
are unrelated quantities.

**Rate limits.** A per-address cursor (`until:`) means each poll asks only for
signatures since the last one, requests are paced to a minimum interval, and a
429 is reported as an ordinary failure so the swap is left alone rather than
retried into a deeper hole.

Three Phase 0 tests failed when Solana landed, all for the right reason: they
asserted every chain had 8 decimals and Bitcoin-hex txids. They are now
chain-aware and assert the *differences* — that lamport amounts format
differently from sats, and that a hex txid is rejected on a base58 chain and
vice versa.

`devnet_identify_and_tip` talks to the real devnet endpoint. It is `#[ignore]`d
so the suite stays offline; run it with `--ignored`. It confirms the adapter
reads the right genesis, that the registry accepts it for `SolanaDevnet`, and
that a `Solana` (mainnet) swap pointed at devnet is rejected.

Integration suite unchanged: 10 passed, 0 failed.

---

**Running the suite outside a terminal.** The harness draws `tracing-indicatif`
progress bars and panics if stdout is not a TTY. `scripts/run_integration_tests.sh`
handles that with `script -q /dev/null`, but that fails in a nested background
context (`tcgetattr/ioctl: Operation not supported on socket`). Driving
`cargo run … --example integration_tests` through Python's `pty.spawn` works and
gives the same result; without a pty exactly one test dies with "footer progress
bar was hidden despite there being pending progress bars", which is the harness,
not the code under test.

---

## Verification

**Per phase, unit tests.** Phase 0: decimals/format/parse round-trips per chain;
`assert_eq!(all().len(), ParentChainType::COUNT)`; `parse_parent_chain` round-trips
every `all()` entry; base58 txid round-trip; **a test pinning each variant's Borsh
discriminant**. Phase 2: the new mock client drives `query_and_update_swap` matching,
age rejection, `is_new`, and `swaps_by_l1_txid` uniqueness — all currently untested.
Bitcoin response parsers get checked-in `getrawtransaction`/`listunspent` JSON
fixtures. Phase 3: `identify()` table tests over recorded `getblockhash 0` /
`getnetworkinfo` JSON; rewrite the three allowlist tests in `lib/parent_chain_rpc.rs`
as identity tests. Phase 4: assert the mock client's call counter is **0** across a
`connect`. Phase 7: `getTransaction` fixtures for a plain transfer, recipient-is-fee-
payer (must reject), a v0 tx with address-lookup tables, a 3-transfer tx where one
matches, and a failed tx. Phase 8: transfer to an existing ATA; ATA created in the
same tx; `transferChecked`; **a wrong-mint token with the same symbol (must not
match)**; a Token-2022 transfer with a fee.

**Existing tests are the regression net.** `cargo test --workspace` plus
`scripts/run_integration_tests.sh`. `integration_tests/l1_rpc_dependency.rs` and
`l1_txid_uniqueness.rs` must pass **unchanged** — the former's invariant ("no config
→ stays Pending") is exactly what the registry preserves.
`l1_verification_rpc_only.rs` and `confirmations_block_inclusion.rs` do
`bmm_single` + 500ms sleep + assert, which assumes detection at block connect;
after Phase 4 they need a `wait_for_swap_state(id, pred, timeout)` helper in
`integration_tests/util.rs` (or a deterministic `refresh_now` RPC).

**Manual end-to-end.**
1. *Startup trap fixed:* configure Signet, stop `bitcoind`, start
   `coinshift_app --headless`. Before: refuses to start. After: boots, and
   `get-connectivity-status` reports Signet `Unreachable`. Start `bitcoind`; the
   chain flips to `Healthy` with no restart.
2. *Enforcer resilience:* start coinshift with the enforcer down; confirm it boots,
   `mine` returns a clear error, and it self-heals when the enforcer comes up.
   Re-run with `--require-mainchain` and confirm the old hard exit.
3. *Bricking is impossible:* `set-l1-config` with a bogus URL → rejected before
   write; the config file is unchanged and the next startup is clean.
4. *Bitcoin regression:* run the full regtest swap walkthrough in
   `docs/MANUAL_SETUP_SWAP_REGTEST.md` end to end (Alice creates, Bob pays L1, swap
   reaches `ReadyToClaim`, Bob claims) and confirm behaviour is identical.
5. *Solana devnet:* create a SOL swap, pay from a devnet wallet, watch the state go
   `Pending` → `WaitingConfirmations(1,2)` at `confirmed` → `ReadyToClaim` at
   `finalized`, then claim. Repeat for devnet USDC, and separately confirm a
   payment in a *different* devnet token with the same symbol does **not** match.
