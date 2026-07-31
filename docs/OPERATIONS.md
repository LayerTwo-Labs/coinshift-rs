# Operating a Coinshift node

What has to be running, what happens when something is not, and how to tell
which is which.

## The two kinds of dependency

Coinshift depends on two quite different things, and they behave differently:

- **The BIP300 enforcer** (`bip300301_enforcer`, gRPC, default
  `http://localhost:50051`), plus the mainchain `bitcoind` behind it. This is
  how the sidechain syncs, mines, deposits and withdraws.
- **Parent chains for swaps** (Bitcoin, Solana, …). These are watched, never
  written to. They are entirely optional and independent of each other.

Neither is required for the node to *start*.

## Deployments

### (a) Minimum node, no swaps

1. `bitcoind` (BIP300-patched) with ZMQ enabled
2. `bip300301_enforcer` on `:50051`, sidechain proposed and activated
3. `coinshift_app --headless`, with no L1 config at all

Everything works: deposits, withdrawals, transfers, mining, P2P. Swaps can be
*created*, they simply are not detected automatically — the creator fills them
with `update-swap-l1-txid`. The node logs a warning at startup when no parent
chain is configured.

### (b) One swap chain

The above, plus an endpoint for the chain you want to swap against:

```bash
# Bitcoin-family: your own node, with -txindex=1
coinshift-cli set-l1-config --parent-chain signet \
  --url http://localhost:38332 --user u --password p

# Solana devnet
coinshift-cli set-l1-config --parent-chain solanadevnet \
  --url https://api.devnet.solana.com
```

`set-l1-config` verifies the endpoint before writing: one serving a different
network is refused outright, one that is merely unreachable is accepted with a
warning, since configuring ahead of starting a node is normal. The change takes
effect without a restart.

### (c) Several chains

One entry per chain. They are independent failure domains: a dead Litecoin node
has no effect on Bitcoin or Solana swaps. Order does not matter, and chains can
be added or removed at any time.

## What fails, and what merely degrades

**Hard failures — the node will not start:**

| Cause | Fix |
|---|---|
| datadir, wallet or database unopenable | check permissions and disk |
| RPC or P2P port already in use | `--rpc-addr` / `--net-addr` |
| `--require-mainchain` given and the enforcer is absent | start the enforcer, or drop the flag |
| `--strict-l1-config` given and a configured chain is unusable | fix or remove that chain, or drop the flag |

**Degradations — the node runs, something is unavailable:**

| Cause | Effect | Recovery |
|---|---|---|
| Enforcer down | no mining, no deposits; block sync stalls | automatic, with backoff |
| Enforcer has no wallet service | no mining, no deposits | automatic once it appears |
| Parent chain unreachable | that chain's swaps are not detected | automatic |
| Parent chain on the wrong network | that chain is never used | fix the config |
| Parent chain unconfigured | that chain's swaps stay `Pending` | configure it, or fill manually |

A parent chain being down never affects block processing, another chain, or the
node's ability to serve RPC.

**One thing that keeps running regardless:** swap expiry is height-based and
deterministic, so a swap still expires on schedule even if its parent chain has
been unreachable the whole time. That is deliberate — expiry is consensus — but
it means a long outage can expire swaps that would otherwise have been filled.

## Seeing what is going on

```bash
coinshift-cli get-connectivity-status
```

reports the enforcer (including `can_mine`) and every parent chain's health, with
a count of swaps still waiting on each. Credentials are never included. The GUI
shows the same thing: a status table in **L1 Config**, and an enforcer indicator
in the bottom panel.

Per-chain health is one of `unconfigured`, `disabled`, `probing`, `unreachable`,
`wrong_chain`, or `healthy`. Only `healthy` chains are consulted for swap
detection, and that verdict expires, so a wedged health check pauses detection
rather than letting it run on stale assurance.

## Configuration

L1 config lives at `<data dir>/coinshift/l1_rpc_configs.json` — Linux
`~/.local/share/`, macOS `~/Library/Application Support/`, Windows `%APPDATA%`.
It is written atomically with mode `0600`, because it holds RPC passwords and API
keys.

> **Note.** This path is global, *not* per-datadir. Several instances started
> with different `--datadir` values share one L1 config. Worth knowing if you run
> the multi-instance setup described in the README.

Relevant flags:

| Flag | Default | Effect |
|---|---|---|
| `--l1 <chain>=<url>` | — | write an endpoint before starting; repeatable, credentials may go in the URL |
| `--mainchain-connect-timeout <secs>` | 30 | how long startup waits for the enforcer |
| `--require-mainchain` | off | refuse to start without the enforcer |
| `--strict-l1-config` | off | refuse to start if a configured chain is unusable |

## Trust

Coinshift believes whatever a configured endpoint says about L1 payments. There
is no allowlist and no second opinion, so point a chain only at a node you run or
trust. The blast radius is your own escrow: L1 verification is not part of
consensus, so a lying endpoint cannot change anyone else's view of the sidechain.
