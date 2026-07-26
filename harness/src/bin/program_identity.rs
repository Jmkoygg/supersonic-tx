//! Program-identity channel — measures a vazamento (leakage) inherent to
//! *any* single, fixed on-chain program deployed at a static `program_id`:
//! anyone who knows the address can enumerate every transaction that ever
//! invoked it (`getSignaturesForAddress`) and, for each one, read off the fee
//! payer/signer. That is enough to correlate "these N bundles all came from
//! this tool" and, if the same wallet used it more than once, link separate
//! bundles to the same real identity — independent of anything the bundle
//! contents themselves do to mix funds.
//!
//! This does not attempt to fix that: there is no mixing-layer fix for "the
//! program's own address is public and immutable" short of a fresh
//! deployment per user (which reintroduces the ~1.27 SOL-per-deploy rent cost
//! Result 1 in BENCHMARK.md measures, at a much less favorable multiplier —
//! once per *user*, not once total). The point of this tool is the same one
//! `eval_mainnet_funding_graph_shipped_mechanism` already makes for the
//! funding-graph channel: measure and name a real, unmitigated residual
//! honestly, rather than let a channel go unmentioned because it isn't fixed.
//!
//! Usage:
//!   program-identity --program-id <PUBKEY> [--rpc URL] [--max-pages 50]
//!
//! Every number this prints is a live `getSignaturesForAddress`/
//! `getTransaction` result against the given cluster at run time —
//! independently reproducible with the same two RPC calls by anyone.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

struct Rpc {
    url: String,
    http: reqwest::blocking::Client,
}

impl Rpc {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("build http client"),
        }
    }

    /// Same retry/backoff shape as `collect_mainnet_profiles.rs`'s `Rpc::call` —
    /// kept as a separate copy rather than a shared module because this is a
    /// standalone, independently-auditable tool (the whole point is that
    /// anyone can read it top to bottom and reproduce the measurement).
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut backoff_ms: u64 = 400;
        for attempt in 0..8u32 {
            let resp = self.http.post(&self.url).json(&body).send();
            match resp {
                Ok(r) => {
                    if r.status().as_u16() == 429 {
                        std::thread::sleep(Duration::from_millis(backoff_ms));
                        backoff_ms = (backoff_ms * 2).min(20_000);
                        continue;
                    }
                    match r.json::<Value>() {
                        Ok(v) => {
                            if let Some(err) = v.get("error") {
                                eprintln!("  [rpc error on {method}, attempt {attempt}] {err}");
                                std::thread::sleep(Duration::from_millis(backoff_ms));
                                backoff_ms = (backoff_ms * 2).min(20_000);
                                continue;
                            }
                            return Ok(v["result"].clone());
                        }
                        Err(e) => {
                            eprintln!("  [decode error on {method}, attempt {attempt}] {e}");
                            std::thread::sleep(Duration::from_millis(backoff_ms));
                            backoff_ms = (backoff_ms * 2).min(20_000);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  [transport error on {method}, attempt {attempt}] {e}");
                    std::thread::sleep(Duration::from_millis(backoff_ms));
                    backoff_ms = (backoff_ms * 2).min(20_000);
                }
            }
        }
        Err(anyhow!("{method} failed after retries"))
    }
}

struct Args {
    rpc: String,
    program_id: String,
    max_pages: u32,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        rpc: "https://api.devnet.solana.com".to_string(),
        program_id: String::new(),
        max_pages: 50,
    };
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rpc" => {
                i += 1;
                a.rpc = args[i].clone();
            }
            "--program-id" => {
                i += 1;
                a.program_id = args[i].clone();
            }
            "--max-pages" => {
                i += 1;
                a.max_pages = args[i].parse()?;
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    if a.program_id.is_empty() {
        return Err(anyhow!("--program-id <PUBKEY> is required"));
    }
    Ok(a)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let rpc = Rpc::new(&args.rpc);

    let mut signatures: Vec<String> = Vec::new();
    let mut before: Option<String> = None;
    for page in 0..args.max_pages {
        let params = match &before {
            Some(b) => json!([args.program_id, {"limit": 1000, "before": b}]),
            None => json!([args.program_id, {"limit": 1000}]),
        };
        let res = rpc.call("getSignaturesForAddress", params)?;
        let arr = res.as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        for s in &arr {
            if let Some(sig) = s.get("signature").and_then(|v| v.as_str()) {
                signatures.push(sig.to_string());
            }
        }
        before = arr
            .last()
            .and_then(|s| s.get("signature"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if arr.len() < 1000 {
            break; // last page
        }
        println!(
            "  [page {}] {} signatures so far",
            page + 1,
            signatures.len()
        );
    }

    let mut distinct_signers: std::collections::HashSet<String> = std::collections::HashSet::new();
    for sig in &signatures {
        let tx = rpc.call(
            "getTransaction",
            json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}]),
        )?;
        if let Some(signer) = tx
            .get("transaction")
            .and_then(|t| t.get("message"))
            .and_then(|m| m.get("accountKeys"))
            .and_then(|k| k.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
        {
            distinct_signers.insert(signer.to_string());
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    println!(
        "{} confirmed signature(s), {} distinct signer(s) for program {} on {}",
        signatures.len(),
        distinct_signers.len(),
        args.program_id,
        args.rpc
    );
    println!(
        "\nThis is the full residual of the program-identity channel: any observer with \
        public RPC access can reproduce this exact count with the same two calls \
        (getSignaturesForAddress, getTransaction) — no privileged data used. A fixed, \
        reused program_id makes every invocation of this tool correlatable to every other, \
        and to the signer's other on-chain activity. Nothing in the bundle-mixing logic \
        (decoy legs, warm-pool, amount noise) touches this channel: it is orthogonal to \
        transaction contents and is not claimed to be mitigated here."
    );
    Ok(())
}
