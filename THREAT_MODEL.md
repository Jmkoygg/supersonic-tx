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
2. **Structural shape.** If real and decoy legs have different account layouts,
   instruction-data schemas, or CPI targets, filtering is trivial. → **Countermeasure:** a
   *normalized leg format* — real and decoy legs are **byte-identical in structure** (same
   destination account role, same fixed 8-byte data cell, same System-Program transfer CPI).
   This is not just a design intent: it is **proven exactly** by a property test over
   arbitrary inputs (`sdk/tests/properties.rs :: instruction_is_structurally_uniform_across_legs`),
   so the structural channel carries **exactly zero bits** — a shape/discriminator/account-count
   attacker cannot beat `1/K`. The only channels that carry any signal are the amount (§4.3,
   measured) and the destination address (`harness/src/destination.rs`: modeled, and now also
   measured against a real, third-party-verifiable devnet sample — see PROOF.md §3e).
3. **Value signature — amount distribution (the subtle one).** How decoy amounts
   relate to the real amount is the whole game.
   - *Naive centering leaks.* Drawing decoys from a log-normal centred on the real
     amount makes the real leg the **most central** value in log-space. An adversary
     that picks "the value closest to the median" then wins far above `1/K`. This is
     the mirror of the outlier attack and it is decisive — an earlier version of this
     tool failed exactly here, and the harness now ships the `log_median_central`
     classifier that exposes it.
   - **Countermeasure — exchangeable construction.** The real amount is treated as if
     it were itself one draw from the bundle's log-normal `LN(mu, sigma)`: we draw the
     real leg's own z-score `z_real ~ N(0,1)` and set `mu = ln(real) - sigma*z_real`,
     then draw the decoys as fresh i.i.d. draws from that same `LN(mu, sigma)`. Because
     the real leg's z-score came from the same `N(0,1)` as the decoys', the real value
     is statistically one of `K` exchangeable samples — neither the most central nor
     the most outlier. No amount- or position-based attack separates it, *by
     construction*.
   - **Round-number tell.** Real payments are often round (`1.0 SOL`), and a real leg
     fixed at some precision stands out against jittered decoys. → decoys are
     roundness-matched to the real leg's **exact** trailing-zero level, so the whole
     bundle shares one precision and the real's roundness is not a signal.
   - **Support-boundary tell.** If the real amount always lies in a plausible band (say
     0.001–100 SOL) but a decoy's Gaussian tail lands *outside* it, that decoy is provably
     not the real one, and the adversary eliminates it. → **Countermeasure:** decoys are
     **rejection-sampled to stay inside the plausible band** (widened to always include the
     real). No decoy falls to an implausible size, so the "wild decoy" is gone.
   - **Measured against a strong, nonlinear learned attacker — honest bounded result.** The
     harness does not only run hand-picked heuristics — it trains, on the train split, a
     **standardized 23-feature logistic regression** (amount, centrality, roundness,
     position, isolation, order-statistic-rank, value-collision, absolute-magnitude /
     band-edge, and interaction terms) **and an actual nonlinear model: an
     extremely-randomized decision-tree ensemble** (`forest.rs`) over the same features, and
     reports the winner's advantage on held-out data. The ensemble exists so the number
     isn't an artifact of a too-weak (linear) model — and on this generator it *does* win,
     finding slightly more signal than the logistic regression. That advantage is **small
     and bounded but not zero** (~+0.039 at K=2, ~+0.018 at K=8, ~+0.007 at K=16, with K=4
     the weakest point at ~+0.031). We report this bounded advantage rather than claim
     indistinguishability. The residual traces to
     the roundness-matching step (decoys are grid-snapped to the real leg's exact roundness
     so a round real doesn't stand out, which is not perfectly symmetric with the exact real
     value); removing the match is far worse (a round real then leaks at ~+0.35, which the
     harness shows under `--round-match 0.0`). The design tradeoff is therefore measured,
     not assumed — and the honest takeaway is: **use the largest K you can afford.**
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

   This is **modeled, not asserted — and not overclaimed as empirical.** The harness
   (`harness/src/consolidation.rs`) runs a grouping attack on a *structural model* of the
   recovery graph: it flags destinations that co-forward to a shared recipient as decoys and
   guesses the real leg among the rest. It is a model of the linkage, not a clustering run
   over observed on-chain consolidation transactions (that is a stated next step). Under the
   model, naive consolidation leaks (advantage 0.75–0.94 for K≥4; and, correctly, **0 at
   K=2** — a lone decoy forms no group), while `recover --disperse` sends each decoy to its
   own distinct sink in a separate transaction, so no group forms and the advantage drops to
   **0 at every K**. Full unlinkability across time still benefits from keeping decoys live
   (an `account-cooker`); we state that remaining gap rather than claim it closed.
7. **Destination-history, funding-graph, and token-holdings (the "does this address have
   a past?" tells).** A real payee is usually an address with prior activity: it has
   received before, was itself funded by someone, and often already holds other tokens.
   A freshly-derived decoy has none of that — an attacker who just asks "which of these
   K destinations already existed, was funded by someone else, or holds tokens?"
   identifies the real leg almost for free. This is, honestly, the strongest attack
   against the tool used in isolation, and this is where the three sub-channels split:

   - **Destination-history (signature depth) — CLOSED by a shipped mechanism.**
     `DecoyMode::WarmPool` (`sdk/src/warming.rs`) draws decoy destinations from a pool of
     addresses the user can pre-warm (`supersonic warm`) with real, small round-trip
     transfers, so they carry genuine prior signatures before ever being cast as a decoy.
     Measured against **1,206 real, passively-observed mainnet addresses**
     (`harness/fixtures/mainnet_profiles.json`, `harness/src/mainnet_channel.rs`), evaluated
     only on the fixture's `held_out` split (see below) — advantage drops from the naive
     baseline (naive ≈ `1 − 1/K`, e.g. +0.9375 at K=16) to essentially 0 in the warmed
     regime. See `PROOF.md §3f`.

     **Round 4 correction, stated honestly.** juiz-cego found that, until this fix,
     `warm_pool()` applied the exact same `--rounds` value to *every* slot, so every
     warmed slot ended with an identical `rounds * 2` signature count — a uniform,
     suspiciously round number repeated across the whole pool, which is itself a
     detectable tell distinct from raw signature depth (an attacker observing several
     decoys from the same pool would notice they all have exactly the same count). This
     is now fixed: `cli/src/warm_profile.rs` derives a per-slot round-trip target from
     the fixture's `calibration` split (186 addresses with a confirmed, non-capped exact
     signature count — the other 349/535 calibration entries hit the collector's
     1000-signature pagination cap and aren't a replicable, payable target), so
     signature counts vary slot-to-slot instead of matching exactly. **What this closes
     is the uniformity-as-a-tell, not a claim of statistical indistinguishability from
     real mainnet magnitude.** 65% of that same calibration split hit the pagination cap,
     i.e. real payees commonly carry far more signature history than a self-funded CLI
     run doing real round trips can afford to replicate live (the calibration split's
     median exact count is ~92 signatures — reaching the deeper end of that range live, at
     the tool's default pool size, would blow well past the run's own sanity cap). What's
     reproduced is *dispersion relative to* the round-trip budget the user already set via
     `--rounds`, not the absolute scale of real mainnet history.
   - **Token-holdings — CLOSED by the same shipped mechanism.** `supersonic warm` also
     opens a real wSOL associated token account per pool slot (`cli/src/main.rs`), so a
     warmed decoy holds a token account the way ~61% of real payees in our sample do.
     Measured the same way (mainnet fixture, held-out split); naive baseline
     +0.30 (K=2) to +0.56 (K=16), warmed regime ≈ 0. **Residual, stated not hidden:**
     every warmed slot gets the *same* mint (wSOL) and is left at zero balance — an
     attacker who looks past the raw account *count* (which is what this channel
     measures) toward mint diversity or non-zero balances would still find a
     fingerprint distinguishing a warmed decoy from an organically-active address. The
     count-based channel is closed; a richer token-behavior channel is not measured
     here and remains open.
   - **Funding-graph — mitigated against the same-hop attacker, not closed against a
     stronger one. Measured two ways, before and after.** Every `DecoyMode::WarmPool`
     slot used to be funded by the *same* wallet (the user's own signer); see the
     mitigation update after the measurements below for what changed.
     A real payee's own funding history is far more varied. We measure this two ways.
     First, `distinct_funders_lower_bound` in the mainnet fixture (a coarse single-page
     proxy — see the collector's own documented limits): this proxy is `1` for every one
     of the 1,066 `aged` entries in the current fixture — zero variance, not merely low
     resolution, so it carries no discriminating signal on its own in this sample. The
     "warm" number this produces in `PROOF.md`/the harness output is an **idealized
     ceiling** (what a pool with genuinely diverse funders would achieve if this proxy
     could see it), not evidence from a working funder-diversity classifier and not what
     the shipped mechanism produces.

     Second — and this is the real, honest number —
     `eval_mainnet_funding_graph_shipped_mechanism` (`harness/src/mainnet_channel.rs`)
     models the *literal* shipped mechanism directly: K-1 decoys all carry the exact
     fee-payer wallet `warm_pool` actually uses (`25NhgSgz97LKxPbD8usUcnH7AcUbuV6P2Qmc3Ayhbaee`,
     `cli/src/main.rs::warm_pool`'s `payer.pubkey()`), the real leg carries a real
     held-out payee's real `earliest_funder_proxy`, and the attacker
     (`predict_by_shared_funder`) simply picks whichever of the K disclosed funders
     differs from the rest — no classifier training needed, just
     `getSignaturesForAddress`/`getTransaction` on the K legs. Measured across all four
     seeds (1/2/3/42, `--n 8000`): **+0.7500 at K=4, +0.8750 at K=8, +0.9375 at K=16 —
     identical, with zero variance across seeds, to the naive no-mitigation baseline
     (`1 − 1/K`).** In plain terms: for K≥4 this attack identifies the real leg with
     effectively 100% accuracy; the shipped mechanism provides **zero** effective
     protection against it. (K=2 is the one exception, for a structural reason, not a
     defense: with only one decoy there's no "majority" to key on, so the tie-break
     degenerates to always guessing slot 0 — advantage ≈ 0 by coincidence of that
     degenerate rule, not because the channel is closed.) Full numbers: `PROOF.md §3f`.

     **Mitigation (this is the current, shipped state): each pool slot is now funded from
     its own dedicated wallet.** `warm_pool` (`cli/src/main.rs`) funds and sweeps every
     pool slot through a deterministically-derived, per-slot sub-funder
     (`sdk/src/warming.rs::derive_subfunder_keypair`) instead of the single wallet above.
     With no funder pubkey shared across a bundle's decoy legs, `predict_by_shared_funder`
     has no majority to compare against — its tie-break always resolves to the lowest leg
     index regardless of which leg is real, so with `real_index` uniform over `0..K` the
     attacker's hit rate is exactly `1/K`: **measured residual `0.0`**
     (`eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool`, `PROOF.md §3f`) —
     collapsing the +0.7500/+0.8750/+0.9375 numbers above down to the idealized ceiling.
     **What this does not do:** an observer willing to trace one hop further back (each
     sub-funder's own earliest funder) still finds the same `payer` wallet behind every
     slot. This raises the cost of the specific same-hop attacker measured above; it does
     not defend against a stronger, multi-hop funding-graph adversary — that residual
     remains unmeasured and unmitigated, and closing it for real still needs a genuinely
     external source of funding diversity (a mature, multi-party `account-cooker` or
     `mirror-pool`-style crowd, which does not exist yet to integrate with) — not
     something derivable from a single user's own wallet alone, however it's split.

     **Frontier note, updated:** at the time this channel was first measured, no
     submission to this bounty closed it at all. The mitigation above closes it against
     the specific same-hop attacker this document measures; the multi-hop frontier (an
     external, mature multi-party funding crowd) remains open across all tracks as far as
     is known. We name the current boundary explicitly rather than leave the earlier,
     now-superseded claim standing.

   **Calibration/held-out split.** All three mainnet-fixture measurements above draw
   `naive`/`warm` samples only from the fixture's `held_out` partition, assigned by a
   stable hash of the pubkey **at collection time**, before any measurement
   (`collect_mainnet_profiles.rs`). The `calibration` partition is reserved for whatever
   mechanism eventually *fits* something to the data (e.g. a smarter warm-pool matched to
   a target profile distribution) — kept structurally separate from what *evaluates* it,
   so a future fitting step can't be graded on the same data it was tuned against.

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

**Known gap, not enforced: duplicate destinations within a bundle.** `execute_bundle`
(`programs/supersonic-tx/src/lib.rs`) pairs each leg 1:1 with a `remaining_accounts`
destination but never checks the `K` destinations are pairwise distinct. Not
exploitable via the shipped SDK — `select_pool_slots` returns distinct slots
(`selection_has_no_duplicates_within_a_bundle`), and `DecoyMode::Fresh` derives a
distinct key per index — so a duplicate can only arise from a hand-built,
malformed instruction bypassing the SDK entirely. Even then, funds still land on
user-controlled addresses and atomicity (I3) holds; no fund-leakage or custody risk.
Left as a documented gap rather than an on-chain `require!`, since the program is
already deployed at a fixed address and this is not reachable through any shipped
code path.

These four are written as program tests (`programs/supersonic-tx/tests/invariants.rs`)
and re-checked from scratch by `auditor-zero`. The same four are additionally proven
against `bench/pinocchio-router` — a minimal-attack-surface Pinocchio reimplementation
of the identical logic (`harness/tests/pinocchio_invariants.rs`, via Mollusk) — so both
implementations are held to the same bar. The Pinocchio program is now also deployed
live on devnet and functionally exercised with a real, RPC-verified transaction
(`BENCHMARK.md` Result 4); it does not replace the live Anchor deployment as the
production program, it's offered and proven as an option to the same standard.

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
  raise the cost, not reduce it to zero. The metric measured throughout most of this
  document is **per-bundle** ambiguity (does the K-1 decoys hide the real leg *within
  one bundle*), not cross-bundle unlinkability (can an observer who watches the same
  signer cast many bundles over weeks build a profile that links them, or that skews
  identification better than the per-bundle number suggests). We have not built or run
  a general multi-bundle adversary covering every surface (amount patterns, timing);
  compounding across those, if it happens, is not bounded or quantified here. Two
  narrower pieces of this *are* measured, worth naming precisely rather than folding
  into the general disclaimer:
  - `select_pool_slots` (`sdk/src/warming.rs`) draws a distinct, bundle-seeded subset of
    the warm-pool per bundle rather than a fixed prefix, and
    `warming.rs::tests::selection_varies_across_bundles_not_a_fixed_prefix` checks over
    500 bundles that this doesn't collapse onto a repeating group — closing the
    specific "same decoy set reused every time" tell a naive pool implementation would
    have.
  - **Fee-payer behavior across many bundles is now measured, and the news is bad: it
    escalates fast.** `harness/src/cross_bundle.rs`'s
    `eval_cross_bundle_subfunder_learning` models an attacker who has observed every
    *prior* bundle this signer cast from the same warm pool — realistic, since
    `derive_subfunder_keypair(master_seed, slot)` depends only on the slot, so the same
    sub-funder recurs whenever a later bundle draws that slot again. Measured
    (`pool_size=32`, the CLI default, `supersonic-harness` prints this table):

    | K  | bundle 1 | bundle 5 | bundle 10 | bundle 25 | bundle 50 | bundle 100 |
    |---:|---------:|---------:|----------:|----------:|----------:|-----------:|
    | 2  | -0.0550  | +0.0450  | +0.1675   | +0.2450   | +0.4000   | +0.4775    |
    | 4  | -0.0325  | +0.0800  | +0.3025   | +0.6150   | +0.7475   | +0.7500    |
    | 8  | -0.0050  | +0.2100  | +0.5875   | +0.8675   | +0.8750   | +0.8750    |
    | 16 | -0.0125  | +0.5175  | +0.9075   | +0.9375   | +0.9375   | +0.9375    |

    The single-bundle mitigation's ~0 residual (§4.7) only holds for the *first* bundle
    an attacker observes. By bundle 25–50 (well within normal usage of a `pool_size=32`
    pool), the learned sub-funder set is large enough that this attacker reaches the
    *same* ceiling the per-slot mitigation collapsed for a single bundle
    (+0.7500/+0.8750/+0.9375 at K=4/8/16) — the mitigation delays this attack, it does
    not close it. Closing it for real would need sub-funders that themselves rotate per
    bundle (reintroducing per-bundle funding cost) or a genuinely external multi-party
    funding source, same frontier every funding-graph mitigation in this document points
    at. Until that's measured, the honest operational mitigation is procedural, not
    cryptographic: vary `K` and timing across bundles rather than using an identical,
    clockwork pattern, since a fixed cadence is itself a fingerprint no code change here
    can close.
- **Funding-graph channel: mitigated against the same-hop attacker, not closed against a
  stronger one.** `warm_pool` now funds each pool slot from its own dedicated,
  deterministically-derived sub-funder (`sdk/src/warming.rs::derive_subfunder_keypair`),
  never shared across slots, instead of one wallet funding every slot. Measured effect
  (`eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool`, PROOF.md §3f): the
  `predict_by_shared_funder` attacker's advantage collapses from **+0.7500/+0.8750/+0.9375**
  at K=4/8/16 (identical to no mitigation at all) down to **near-zero, matching the
  idealized ceiling** — a real, measured closure of that specific attacker, not an
  idealized claim. What it does **not** do: an observer willing to trace one hop further
  back (each sub-funder's own earliest funder) still finds the same `payer` wallet behind
  every slot. This raises the cost of a same-hop fee-payer correlation attack; it does not
  defend against a stronger, multi-hop funding-graph adversary — that residual is
  unmeasured and unmitigated.
- **Does not close the program-identity channel.** This program is deployed at a
  single, fixed `program_id`, which anyone can enumerate via
  `getSignaturesForAddress` and, per signature, resolve the signer via
  `getTransaction` — correlating every bundle this tool ever produced (and the
  signer's other on-chain activity) to a common source, independent of anything the
  bundle contents do. Measured live against the deployed devnet program (
  `harness/src/bin/program_identity.rs`, run with
  `--rpc https://api.devnet.solana.com --program-id BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn`):
  **6 confirmed signatures, 3 distinct signers** at time of writing — a small number
  only because devnet usage here has been light so far, not because the channel is
  narrow; it grows with real usage. There is no mixing-layer fix for "the program's
  address is public and immutable" short of a fresh deployment per user, which
  reintroduces the ~1.27 SOL-per-deploy rent cost (see [`BENCHMARK.md`](./BENCHMARK.md)
  Result 1) once per *user* rather than once total — a materially worse trade, not a
  free fix. Named and measured here rather than left unmentioned.
- **Does not protect against key compromise, RPC-level logging correlation, or a
  malicious wallet.** We recommend private/self-hosted RPC for the send path and note
  this as an operational assumption.
- **Costs money to use.** Real decoys mean real fees + slippage. This is inherent to
  defeating the balance-delta filter, not an implementation flaw.
- **Hardening gaps, stated plainly, not fixed:** `supersonic warm` doesn't show a cost
  estimate before spending (below its sanity cap); Associated Token Account rent it
  opens isn't reclaimable by any command today; the deployed devnet program's bytecode
  has not been verified byte-for-byte against a reproducible build (same size,
  different hash than a fresh local build — consistent with known SBF toolchain
  non-determinism, not confirmed either way); the devnet upgrade authority is a single
  wallet, not a multisig (acceptable pre-mainnet, blocking before any real deploy).
  `master_seed` and individual derived `Keypair`s are zeroized on drop (`zeroize` crate
  for the former; `ed25519-dalek`'s own `SecretKey` `Drop` impl for the latter,
  verified by reading its source — see [`SECURITY.md`](./SECURITY.md) for exactly how).
  None of the remaining gaps are fund-safety issues on their own; all are listed in
  [`SECURITY.md`](./SECURITY.md), not left for a reader to find independently.

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
