use std::path::PathBuf;

use clap::Parser;

/// Verify the tamper-evident evidence chain produced by the egress mediator.
#[derive(Parser)]
pub struct Args {
    /// Path to the evidence file (e.g. evidence.jsonl).
    pub evidence: PathBuf,
    /// Path to the Ed25519 public key file (e.g. egress-key.pub).
    pub pubkey: PathBuf,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let n = agentd::evidence::verify_chain(&args.evidence, &args.pubkey)?;
    println!("chain ok: {n} receipts verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentd::evidence::EvidenceWriter;
    use tempfile::TempDir;

    #[test]
    fn verify_valid_chain_succeeds() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let writer = EvidenceWriter::open(&ev, &key).unwrap();
        writer.record_allowed("inference", "claude-sonnet-4-6", "agent_0").unwrap();
        writer.record_allowed("inference", "claude-sonnet-4-6", "agent_0").unwrap();
        drop(writer);

        let pubkey = key.with_extension("pub");
        let result = run(Args { evidence: ev, pubkey });
        assert!(result.is_ok(), "valid chain should verify: {result:?}");
    }

    #[test]
    fn verify_missing_pubkey_returns_err() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let pubkey = dir.path().join("nonexistent.pub");
        std::fs::write(&ev, "").unwrap();
        let result = run(Args { evidence: ev, pubkey });
        assert!(result.is_err(), "missing pubkey should return an error");
    }

    #[test]
    fn verify_tampered_chain_returns_err() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        {
            let writer = EvidenceWriter::open(&ev, &key).unwrap();
            for _ in 0..3 {
                writer.record_allowed("inference", "m", "a").unwrap();
            }
        }
        let content = std::fs::read_to_string(&ev).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        let tampered = format!("{}\n{}\n", lines[0], lines[2]);
        std::fs::write(&ev, tampered).unwrap();

        let pubkey = key.with_extension("pub");
        let result = run(Args { evidence: ev, pubkey });
        assert!(result.is_err(), "tampered chain should fail verification");
    }
}
