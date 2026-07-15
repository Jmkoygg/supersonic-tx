# supersonic-tx — Threat Model

> Status: **draft for review** (Phase 1). This document defines the adversaries,
> what they can actually observe on Solana, the concrete security goal and metric,
> the distinguishability attack surface, and the invariants the program must hold.
> Everything downstream (program design, decoy strategies, the adversarial proof
> harness) is derived from this document. If a claim here is wrong, the tool is
> wrong — so this is where scrutiny is cheapest.

## 0. One-paragraph summary

On Solana there is **no persistent public mempool** and every confirmed transaction
is **public forever**. So `supersonic-tx` does **not** try to hide that a transaction
happened, or hide who signed it. Its single, falsifiable goal is **intent ambiguity**:
when a user's real action is submitted as one "leg" inside an atomic bundle of `K`
economically-real legs, an adversary reconstructing the transaction should not be able
to identify *which* leg was the user's actual intent with probability materially
better than random guessing (`1/K`). We state this as an instruction-level
k-anonymity target and we measure it against real classifiers, not by assertion.

## 1. What is actually observable on Solana (grounded facts)

These facts define the boundaries of what any privacy tool on Solana *can* do. They
are the reason the goal is "ambiguity," not "concealment."

1. **No public mempool.** Solana forwards transactions via Gulf Stream directly to the
   current and upcoming leaders; there is no shared public pending-tx pool the way
   Ethereum has. There is nothing to "hide in the mempool" because there is no public
   mempool to observe.
2. **Pre-inclusion visibility is limited and shrinking.** The main pre-inclusion
   observation channel was Jito's relayer/block-engine (searchers seeing txs ~200ms
   before the leader). As of 2026 that broad mempool visibility was suspended; what
   remains is more fragmented (private orderflow, ShredStream on shreds already in
   propagation). We treat pre-inclusion leakage as a **minimize**, not **eliminate**,
   target — and mostly out of scope for the retail copy-trader adversary, who observes
   *after* confirmation.
3. **Post-confirmation, everything is public and permanent.** Any confirmed tx is
   retrievable via RPC `getTransaction`/`getBlock` and streamed in real time via Geyser
   plugins (notably Yellowstone gRPC, sub-50ms). This is the adversary channel that
   actually matters.
4. **Balance deltas are first-class observable.** `getTransaction` returns
   `preBalances`/`postBalances` and `preTokenBalances`/`postTokenBalances`. An observer
   sees the *net economic effect* of the transaction, not just the instruction data.
   **This single fact is the most important constraint in the design** (see §4).
5. **Address Lookup Tables are not privacy.** ALTs compress accounts into indices to
   fit more per tx; any full RPC resolves `addressTableLookups` back to the real
   addresses. We do not treat ALTs as an obfuscation layer.

## 2. Adversaries (who we defend against, and what each sees)

| Adversary | Channel | Capability | In scope? |
|---|---|---|---|
| **Copy-trader bot** | Yellowstone gRPC / RPC `transactionSubscribe`, real-time | Filters by known program IDs (Jupiter, Raydium, pump.fun), parses direction/mint/size from inner instructions & balance deltas, follows CPI chain to the final effect | **Primary** |
| **Post-hoc forensic analyst** | `getTransaction`/block history, unlimited time | Clusters wallets, correlates timing, builds funding graphs, applies heuristics over the full permanent record | **Primary** |
| **Pre-inclusion searcher** | Jito/private orderflow, ShredStream | Sees some txs shortly before finalization; can front/back-run | **Secondary** (minimize) |
| **Global passive adversary w/ unlimited compute** | everything | statistical + ML over all chain history | **Explicitly out of scope** — see §6 |

## 3. Security goal and metric (falsifiable)

**Goal — instruction-level k-anonymity.** A bundle contains one *real* leg and
`K−1` *decoy* legs (`K` = anonymity set size for that bundle). For any adversary
`A` that outputs a guess of which leg is real:

```
advantage(A) = P[A identifies the real leg] − 1/K
```

The tool succeeds for a bundle class when `advantage(A) ≈ 0` for the realistic
adversary classifiers we implement in the proof harness (Phase 5). An `advantage`
significantly above 0 means the decoys leak — the generation strategy is broken and
must be fixed before we claim anything.

This gives us a **number to report**, not an adjective. The proof harness runs real
classifiers (§4 attack surface) against real confirmed devnet transactions and reports
measured `advantage` versus the `1/K` baseline. That measured number, with tx hashes,
is the core of `PROOF.md`.

## 4. Distinguishability attack surface → countermeasures

Each row is a way an adversary separates real from decoy. The design must answer every
one, and the harness must *test* each one.

1. **Economic-effect filter (the hard one).** A decoy that moves no real value is
   discarded in one step by looking at balance deltas. → **Countermeasure:** every
   decoy leg is *economically real* — it moves the user's own funds. This is
   non-negotiable and it is why the tool costs fees + slippage to run.
2. **Structural shape.** If real and decoy legs have different account layouts or
   instruction-data schemas, filtering is trivial. → **Countermeasure:** a *normalized
   leg format* — real and decoy legs are structurally identical at the program
   interface.
3. **Value signature.** Round numbers, or decoys with suspiciously uniform sizes,
   stand out. → **Countermeasure:** decoy amounts drawn from a distribution that
   matches plausible real activity (justified, not `rand()`), no round-number tells.
4. **Timing / cadence.** Deterministic scheduling is a fingerprint. → **Countermeasure:**
   randomized, human-plausible cadence at the SDK layer (bundles are atomic on-chain,
   but *when* a user casts them is a signal).
5. **Counterparty / venue.** If the real leg hits a "meaningful" venue and decoys hit
   random ones, the venue set leaks intent. → **Countermeasure:** decoy venues drawn
   from the *same* realistic set the real leg could plausibly use.
6. **The consolidation tell (honest open weakness).** The Phase 2 draft used
   within-bundle round-trips (A→B→A), which are themselves a signature ("economically
   pointless round-trip"). Phase 3 removes that: legs are one-way transfers and funds
   *stay* at the destination, so a single bundle looks like a fan-out of real payments,
   not a round-trip. **But the tell moves rather than disappearing:** the user
   eventually wants their decoy funds back, and a naive *consolidation* — sweeping the
   K−1 decoy destinations back to one wallet right after — retroactively re-links them
   ("these K−1 addresses all forwarded to the same wallet ⇒ they were decoys ⇒ the
   remaining leg was real"). This is the same information leak, one hop later.
   → **Countermeasure options, evaluated in Phase 5, not assumed solved:**
   - **delay + disperse** consolidation across time and across many later bundles, so
     the sweep is not co-timed with the original fan-out;
   - **keep decoys live** — let decoy destinations themselves originate future bundles
     (this is precisely what a companion `account-cooker` provides), so they never
     collapse back to a single wallet in an obvious sweep;
   - **partial / never consolidate** — treat the decoy spend as a real cost (the user
     genuinely holds funds across several of their own wallets), trading money for the
     strongest anonymity.

   We do **not** claim to have solved consolidation. Phase 5's harness includes a
   consolidation-linkage detector, and the measured `advantage` is reported for both the
   single-bundle view (strong) and the naive-consolidation view (weak) — separately and
   honestly. Documenting this gap precisely, and showing the delay/disperse mitigation
   moving the number, is part of the deliverable.

## 5. Invariants the on-chain program MUST enforce (testable)

- **I1 — Non-custodial.** The program never holds user funds. There is no pool, no
  escrow, no shared or withdrawable balance. Each leg moves lamports directly from the
  signer to its destination within one atomic transaction; the program is a neutral
  executor that owns nothing.
- **I2 — No third-party leakage from decoys; the program never custodies.** This is the
  invariant that keeps the tool out of money-transmitter / mixer territory (§7), and it
  is enforced at **two distinct layers**, stated honestly:
  - **On-chain (program):** the program custodies nothing and transmits nothing on
    anyone's behalf (I1). It moves only the *signer's own* lamports, only to
    destinations the signer supplied, only within the signer's own atomic transaction.
    It never pools or forwards a third party's funds. This is what the mixer analysis
    in §7 actually turns on, and it is structural.
  - **Off-chain (SDK) — deliberately not on-chain:** *decoy* destinations are
    user-controlled and recoverable; only the *real* leg pays the user's genuinely
    intended counterparty. The program does **not** verify "is this destination the
    user's own?" on purpose — any such on-chain check would brand decoys and leak the
    very thing we hide. So decoy recoverability is a property of how the SDK builds the
    bundle, not an on-chain constraint. We do not overclaim it as program-enforced.

  Note the earlier draft framed I2 as "every leg moves only the signer's own funds,
  enforced on-chain." That is incompatible with a *useful* real leg: the real leg is by
  definition the user's own intended action, which may legitimately pay a third party.
  The program cannot both let the real leg be useful and prove on-chain that no leg pays
  a third party — and trying to would leak. The reformulation above resolves that.
- **I3 — Atomicity.** The whole bundle succeeds or the whole bundle reverts. A partial
  execution that lands some legs but not others — exposing the real leg without its full
  decoy set — must be impossible.
- **I4 — No fund leakage on malformed input.** A malformed or adversarial leg (zero
  amount, self-send, leg/destination count mismatch, insufficient funds) must fail
  closed — revert the entire bundle — never move funds partially.

These four are written as program tests (`programs/supersonic-tx/tests/invariants.rs`)
and re-checked from scratch by `auditor-zero`.

## 6. Explicit non-goals and limitations (what this does NOT do)

An auditor reads this section first. We are deliberately honest about the edges.

- **Does not hide that a transaction occurred, or who signed it.** Impossible on
  Solana; not attempted.
- **Does not provide fund anonymity or break the link between a user's own inputs and
  outputs.** It is not a mixer. Your funds visibly remain yours (that is invariant I2).
- **Does not defend against an adversary who already knows your identity out-of-band**
  (e.g., you posted your wallet publicly, or KYC ties it).
- **Does not defend against a global adversary correlating across many of your bundles
  over time with unlimited compute** — repeated use leaks a behavioral prior. We can
  raise the cost, not reduce it to zero. The metric is per-bundle ambiguity, and we
  state the multi-bundle limitation plainly.
- **Does not protect against key compromise, RPC-level logging correlation, or a
  malicious wallet.** We recommend private/self-hosted RPC for the send path and note
  this as an operational assumption.
- **Costs money to use.** Real decoys mean real fees + slippage. This is inherent to
  defeating the balance-delta filter, not an implementation flaw.

## 7. Legal posture (why this is not a mixer)

The design deliberately sits on the safe side of the line that made Tornado Cash a
money-transmission case:

- A mixer **receives third-party funds, breaks provenance, and lets funds be withdrawn
  by someone** — it transmits value on behalf of others.
- `supersonic-tx` **never custodies or transmits third-party funds** (I1, I2). A user
  only ever moves their *own* money, and it stays theirs. The tool manufactures
  *behavioral ambiguity*, not *fund anonymity*.

This is a design constraint, not a disclaimer: if a proposed feature would require
pooling or transmitting other users' funds to work, it is rejected at design time. Any
change that pressures I1/I2 is escalated to the human owner before implementation,
because it changes the legal posture of a tool published under a real name.

## 8. Assumptions

- User's signing key is secure and not otherwise deanonymized.
- The send-path RPC is trusted or private (a logging RPC that timestamps submissions
  is a side channel we flag but do not fully close).
- Devnet is the validation environment; economic realism of decoys is validated with
  program-controlled venues where devnet liquidity is unreliable.
