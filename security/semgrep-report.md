# Semgrep static-analysis report

Third-party static analysis, run and versioned in this repo to complement Sec3 X-Ray
(Solana-specific, program-only) and `cargo audit` (dependency advisories, see
`SECURITY.md`/`README.md`) with a general-purpose Rust/security scanner.

## What was run

```
semgrep 1.171.0
semgrep --config=p/rust programs sdk cli harness bench/pinocchio-router \
  --x-ignore-semgrepignore-files --sarif --output security/semgrep-report.sarif
```

- Scope: this project's own Rust code only — `programs/`, `sdk/`, `cli/`, `harness/`,
  `bench/pinocchio-router/`. `target/` and third-party dependencies are excluded (not
  scanned, not part of this repo's source).
- Ruleset: `p/rust` — the Semgrep Registry's general Rust ruleset (11 rules). This was
  the most productive of the rulesets tried against this codebase:
  - `p/rust` (11 rules) — 4 findings (below).
  - `p/security-audit` (225 rules registered, but only 3 apply to Rust files without a
    Semgrep login) — 0 findings.
  - `--config=auto` (registry default, unauthenticated) — only 4 Rust-specific rules
    resolve without login — 0 findings.
  - No `semgrep login`/Semgrep AppSec Platform token was used, so some registry rules
    that require authentication were not available; `p/rust` does not require one and
    gave the most complete unauthenticated Rust coverage.
- `--x-ignore-semgrepignore-files` was added because Semgrep's bundled default
  `.semgrepignore` silently skips anything under a `tests/` directory (5 files:
  `cli/tests/warm_sweep_fix_verification.rs`, `harness/tests/mollusk_cu_bench.rs`,
  `harness/tests/pinocchio_invariants.rs`, `programs/supersonic-tx/tests/invariants.rs`,
  `sdk/tests/properties.rs`). This project's test files include real invariant/property
  tests worth scanning, so the flag forces them in. Findings were identical with and
  without the flag (the flag only adds coverage, it does not change results in the
  non-test files).
- Full run: 28 targets (26 `.rs` + 2 `.json` fixtures), 11 rules, **4 findings, 0 blocking
  after review** (see below). Full machine-readable output: `security/semgrep-report.sarif`.

## Findings — all 4 reviewed, all false positives for this codebase

### 1. `rust.lang.security.temp-dir.temp-dir` — `cli/src/main.rs:1091`

```rust
let dir = std::env::temp_dir().join(format!(
    "supersonic-cli-test-{}-{}",
    std::process::id(),
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
));
```

Rule warns that `std::env::temp_dir()` combined with a predictable name is an insecure
temp-file pattern (symlink/pre-creation attacks in a shared temp directory).

**Why it's a false positive here:** this is inside `#[cfg(test)] mod tests` — it is
never compiled into the shipped CLI binary, only into the test harness run by
`cargo test` on the developer's own machine. It exists purely to point the `HOME` env var
at an empty scratch directory so tests don't touch a real user's `~/.supersonic/*`
data; nothing security-sensitive (secrets, keys, permissions) is written to this
directory, and there is no cross-user/cross-privilege trust boundary being crossed —
it's local test isolation, not a production temp-file security decision. No change made.

### 2–4. `rust.lang.security.args.args` — `harness/src/main.rs:42`, `harness/src/bin/collect_devnet_history.rs:74`, `harness/src/bin/collect_mainnet_profiles.rs:117`

```rust
let args: Vec<String> = std::env::args().collect();
```

Rule warns against relying on `std::env::args()` — specifically `args()[0]`, the
program's own invocation path — for a *security* decision, since that value is
attacker-controllable and not a trustworthy identity check.

**Why it's a false positive here:** in all three call sites `args` is used purely for
ordinary CLI flag parsing (`--n`, `--seed`, `--aged-count`, `--rpc`, `--out`, etc.) in
internal harness/data-collection binaries — none of them read `args[0]` or use any
element of `args` to make a trust, auth, or privilege decision. This is the generic
"any manual argv-parsing Rust CLI trips this rule" false-positive pattern the rule is
known for; there's no security operation being gated on argv content here. No change
made.

## Summary

0 real findings requiring a code change. 4 findings, all reviewed individually and
confirmed to be rule false-positives against this specific code (test-only temp-dir
usage, and ordinary CLI arg parsing misclassified as a security-sensitive `args[0]`
check) — documented above rather than suppressed silently. Re-run anytime with the
command at the top of this file; SARIF output is versioned at
`security/semgrep-report.sarif` for tooling/diffing.
