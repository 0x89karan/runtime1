//! Tamper-evident action receipt writer (p7.5).
//!
//! Appends Ed25519-signed `ActionReceipt` lines to a JSONL file.
//! Each receipt includes a SHA-256 hash of the prior line, forming a hash chain.
//! Removing, reordering, or forging any line breaks the chain.
//!
//! Honest limits: for native agents the signing key lives in-process alongside the
//! agent. A logic exploit cannot forge a signature; a memory-corruption exploit could.

use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use ring::{
    digest,
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};

/// SHA-256 hash used as `chain_prev_hash` for the very first receipt.
/// Constant so verifiers have a deterministic anchor.
pub const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Serialisation fields covered by the Ed25519 signature (no `signature` field).
#[derive(Serialize, Deserialize, Debug, Clone)]
struct ReceiptBody {
    seq: u64,
    action: String,
    target: String,
    principal: String,
    verdict: String,
    ts: String,
    chain_prev_hash: String,
}

/// A fully-signed action receipt appended as one JSON line in `evidence.jsonl`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ActionReceipt {
    pub seq: u64,
    pub action: String,
    pub target: String,
    pub principal: String,
    pub verdict: String,
    pub ts: String,
    pub chain_prev_hash: String,
    /// Ed25519 signature over the canonical `ReceiptBody` JSON (lowercase hex).
    pub signature: String,
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as FmtWrite;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(s.len().is_multiple_of(2), "odd-length hex string");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(anyhow::Error::from))
        .collect()
}

struct Inner {
    seq: u64,
    chain_prev_hash: String,
    writer: BufWriter<std::fs::File>,
}

/// Thread-safe writer for action receipts. One per `agentd` process.
pub struct EvidenceWriter {
    keypair: Ed25519KeyPair,
    inner: Mutex<Inner>,
    pub_key_path: PathBuf,
}

impl EvidenceWriter {
    /// Open (or create) the evidence file. Loads an existing Ed25519 key from
    /// `key_path`, or generates a new one and persists it on first run.
    pub fn open(evidence_path: &Path, key_path: &Path) -> Result<Self> {
        let rng = SystemRandom::new();
        let keypair = if key_path.exists() {
            let pkcs8 = std::fs::read(key_path)
                .with_context(|| format!("reading egress key {}", key_path.display()))?;
            Ed25519KeyPair::from_pkcs8(&pkcs8)
                .map_err(|e| anyhow::anyhow!("invalid PKCS8 egress key: {e:?}"))?
        } else {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
                .map_err(|e| anyhow::anyhow!("key generation failed: {e:?}"))?;
            write_private_key(key_path, pkcs8.as_ref())?;
            Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
                .map_err(|e| anyhow::anyhow!("invalid generated PKCS8: {e:?}"))?
        };

        let pub_key_path = key_path.with_extension("pub");
        // Always overwrite pubkey so it stays in sync with the private key.
        write_public_key(&pub_key_path, keypair.public_key().as_ref())?;

        let (seq, chain_prev_hash) = resume_chain(evidence_path)?;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(evidence_path)
            .with_context(|| format!("opening evidence file {}", evidence_path.display()))?;

        Ok(Self {
            keypair,
            inner: Mutex::new(Inner {
                seq,
                chain_prev_hash,
                writer: BufWriter::new(file),
            }),
            pub_key_path,
        })
    }

    /// Record an allowed action. Returns the receipt sequence number.
    pub fn record_allowed(&self, action: &str, target: &str, principal: &str) -> Result<u64> {
        self.write_receipt(action, target, principal, "allowed")
    }

    /// Record a denied action. Returns the receipt sequence number.
    pub fn record_denied(&self, action: &str, target: &str, principal: &str) -> Result<u64> {
        self.write_receipt(action, target, principal, "denied")
    }

    fn write_receipt(
        &self,
        action: &str,
        target: &str,
        principal: &str,
        verdict: &str,
    ) -> Result<u64> {
        let ts = chrono::Utc::now().to_rfc3339();
        let mut inner = self.inner.lock().unwrap();

        let body = ReceiptBody {
            seq: inner.seq,
            action: action.to_string(),
            target: target.to_string(),
            principal: principal.to_string(),
            verdict: verdict.to_string(),
            ts,
            chain_prev_hash: inner.chain_prev_hash.clone(),
        };
        let canonical = serde_json::to_string(&body).context("serializing receipt body")?;
        let sig = self.keypair.sign(canonical.as_bytes());
        let receipt = ActionReceipt {
            seq: body.seq,
            action: body.action,
            target: body.target,
            principal: body.principal,
            verdict: body.verdict,
            ts: body.ts,
            chain_prev_hash: body.chain_prev_hash,
            signature: hex_encode(sig.as_ref()),
        };
        let line = serde_json::to_string(&receipt).context("serializing receipt")?;
        inner.chain_prev_hash =
            hex_encode(digest::digest(&digest::SHA256, line.as_bytes()).as_ref());
        let seq = inner.seq;
        inner.seq += 1;
        writeln!(inner.writer, "{}", line).context("writing receipt")?;
        inner.writer.flush().context("flushing evidence file")?;
        // fdatasync: durability guarantee required for the tamper-evidence claim.
        // A receipt is "written" only when it is on stable storage; BufWriter::flush
        // only flushes user-space buffers.
        inner.writer.get_ref().sync_data().context("fsyncing evidence file")?;
        Ok(seq)
    }

    pub fn public_key_path(&self) -> &Path {
        &self.pub_key_path
    }
}

fn resume_chain(path: &Path) -> Result<(u64, String)> {
    if !path.exists() {
        return Ok((0, GENESIS_HASH.to_string()));
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading evidence file {}", path.display()))?;
    let mut seq = 0u64;
    let mut last_hash = GENESIS_HASH.to_string();
    for line in content.lines() {
        last_hash = hex_encode(digest::digest(&digest::SHA256, line.as_bytes()).as_ref());
        seq += 1;
    }
    Ok((seq, last_hash))
}

fn write_private_key(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating key file {}", path.display()))?;
        f.write_all(data)
            .with_context(|| format!("writing key file {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
            .with_context(|| format!("writing key file {}", path.display()))?;
    }
    Ok(())
}

fn write_public_key(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(path)
            .with_context(|| format!("creating public key file {}", path.display()))?;
        f.write_all(data)
            .with_context(|| format!("writing public key file {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
            .with_context(|| format!("writing public key file {}", path.display()))?;
    }
    Ok(())
}

/// Verify the receipt chain in `evidence_path` using the public key at `pub_key_path`.
/// Returns the number of receipts verified on success.
pub fn verify_chain(evidence_path: &Path, pub_key_path: &Path) -> Result<u64> {
    let pub_key_bytes = std::fs::read(pub_key_path)
        .with_context(|| format!("reading public key {}", pub_key_path.display()))?;
    let pub_key =
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pub_key_bytes);

    let content = std::fs::read_to_string(evidence_path)
        .with_context(|| format!("reading evidence file {}", evidence_path.display()))?;

    let mut prev_hash = GENESIS_HASH.to_string();
    let mut expected_seq = 0u64;

    for (i, line) in content.lines().enumerate() {
        let receipt: ActionReceipt = serde_json::from_str(line)
            .with_context(|| format!("parsing receipt at line {}", i + 1))?;

        if receipt.seq != expected_seq {
            anyhow::bail!(
                "seq={} expected={} at line {}: sequence gap",
                receipt.seq,
                expected_seq,
                i + 1
            );
        }
        if receipt.chain_prev_hash != prev_hash {
            anyhow::bail!(
                "seq={} at line {}: hash chain break (expected {}, got {})",
                expected_seq,
                i + 1,
                prev_hash,
                receipt.chain_prev_hash
            );
        }

        let body = ReceiptBody {
            seq: receipt.seq,
            action: receipt.action.clone(),
            target: receipt.target.clone(),
            principal: receipt.principal.clone(),
            verdict: receipt.verdict.clone(),
            ts: receipt.ts.clone(),
            chain_prev_hash: receipt.chain_prev_hash.clone(),
        };
        let canonical = serde_json::to_string(&body).context("serializing for verify")?;
        let sig_bytes = hex_decode(&receipt.signature)
            .with_context(|| format!("seq={}: invalid sig hex", expected_seq))?;
        pub_key
            .verify(canonical.as_bytes(), &sig_bytes)
            .map_err(|_| {
                anyhow::anyhow!(
                    "seq={} at line {}: Ed25519 signature invalid",
                    expected_seq,
                    i + 1
                )
            })?;

        prev_hash = hex_encode(digest::digest(&digest::SHA256, line.as_bytes()).as_ref());
        expected_seq += 1;
    }

    Ok(expected_seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn receipt_chain_round_trip() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let writer = EvidenceWriter::open(&ev, &key).unwrap();
        writer
            .record_allowed("inference", "claude-sonnet-4-6", "agent_0")
            .unwrap();
        writer
            .record_allowed("inference", "claude-sonnet-4-6", "agent_0")
            .unwrap();
        writer
            .record_denied("egress", "https://evil.example.com", "agent_0")
            .unwrap();
        let pub_path = key.with_extension("pub");
        let n = verify_chain(&ev, &pub_path).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn receipt_detects_tamper_removed_line() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let writer = EvidenceWriter::open(&ev, &key).unwrap();
        for _ in 0..3 {
            writer.record_allowed("inference", "m", "a").unwrap();
        }
        drop(writer);
        let content = std::fs::read_to_string(&ev).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        let tampered = format!("{}\n{}\n", lines[0], lines[2]);
        std::fs::write(&ev, tampered).unwrap();
        let pub_path = key.with_extension("pub");
        assert!(verify_chain(&ev, &pub_path).is_err());
    }

    #[test]
    fn receipt_genesis_anchor() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let writer = EvidenceWriter::open(&ev, &key).unwrap();
        writer.record_allowed("inference", "m", "a").unwrap();
        drop(writer);
        let content = std::fs::read_to_string(&ev).unwrap();
        let receipt: ActionReceipt =
            serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(receipt.chain_prev_hash, GENESIS_HASH);
        assert_eq!(receipt.seq, 0);
    }

    #[test]
    fn key_persists_across_open() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        {
            let w = EvidenceWriter::open(&ev, &key).unwrap();
            w.record_allowed("inference", "m", "a").unwrap();
            w.record_allowed("inference", "m", "a").unwrap();
        }
        {
            let w = EvidenceWriter::open(&ev, &key).unwrap();
            w.record_allowed("inference", "m", "a").unwrap();
        }
        let pub_path = key.with_extension("pub");
        assert_eq!(verify_chain(&ev, &pub_path).unwrap(), 3);
    }

    #[test]
    fn empty_chain_verifies_zero() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        std::fs::write(&ev, "").unwrap();
        let key = dir.path().join("egress.pkcs8");
        EvidenceWriter::open(&ev, &key).unwrap();
        let pub_path = key.with_extension("pub");
        assert_eq!(verify_chain(&ev, &pub_path).unwrap(), 0);
    }
}
