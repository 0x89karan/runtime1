//! Tamper-evident action receipt writer (p7.5).
//!
//! Appends Ed25519-signed `ActionReceipt` lines to a JSONL file.
//! Each receipt includes a SHA-256 hash of the prior line, forming a hash chain.
//! Removing, reordering, or forging any line breaks the chain.
//!
//! Honest limits: for native agents the signing key lives in-process alongside the
//! agent. A logic exploit cannot forge a signature; a memory-corruption exploit could.

use std::{
    io::{BufWriter, Read, Seek, SeekFrom, Write},
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

/// Bytes read from the END of the evidence file to recover chain state at boot (ux.6a).
/// A receipt line is ~330 B, so this holds ~200 lines where only the last is needed.
const RESUME_TAIL_WINDOW: u64 = 64 * 1024;

/// Ceiling on the resume window. Beyond this the file is not a plausible receipt log, and
/// we fall back to the legacy full scan rather than growing the read without bound.
const RESUME_TAIL_MAX: u64 = 1024 * 1024;

/// Size at which `evidence.jsonl` is rotated to a numbered segment (ux.6a, audit86-P2-4).
/// ~330 B per receipt ⇒ ~100 000 receipts ⇒ months of normal operation per segment.
const MAX_EVIDENCE_BYTES: u64 = 32 * 1024 * 1024;

/// How many rotated segments are retained (`evidence.jsonl.1` … `.3`). Bounds total evidence
/// on disk at `(EVIDENCE_SEGMENTS_KEPT + 1) * MAX_EVIDENCE_BYTES` = 128 MiB.
///
/// The oldest segment is DELETED when the shift runs. Be precise about the record of that,
/// because an earlier version of this comment claimed it "is logged rather than silent" and
/// that overstated things: the eviction is announced on `tracing` only. `EvidenceWriter` holds
/// no `FlightRecorder`, so nothing durable records which segment went — filed as a P3, and a
/// retention ceiling expressed as a constant is a policy decision, not an implementation
/// detail. Operators who need longer retention must archive segments externally.
const EVIDENCE_SEGMENTS_KEPT: usize = 3;

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

/// Decode a lowercase-hex string to bytes.
///
/// Byte-oriented on purpose. This previously sliced the `&str` as `&s[2*i..2*i+2]`, which
/// **panics** when a multi-byte character straddles an even offset — `"a\u{e9}a"` is 4 bytes,
/// passes the length guard, and aborts the process on the first slice. That was survivable
/// while the only caller was the `agentctl verify` CLI, but ux.6a made it reachable from
/// `EvidenceWriter::open` (via the tail signature check) i.e. on the BOOT path of a process
/// that is PID 1 in the distro, with `panic = "abort"` set workspace-wide. A panic is not an
/// `Err`, so `resume_chain`'s fallback could not catch it either.
/// Invariant restored: "the loop never panics on bad input".
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    anyhow::ensure!(bytes.len().is_multiple_of(2), "odd-length hex string");
    bytes
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char)
                .to_digit(16)
                .with_context(|| format!("invalid hex digit {:?}", pair[0] as char))?;
            let lo = (pair[1] as char)
                .to_digit(16)
                .with_context(|| format!("invalid hex digit {:?}", pair[1] as char))?;
            Ok(((hi << 4) | lo) as u8)
        })
        .collect()
}

struct Inner {
    seq: u64,
    chain_prev_hash: String,
    writer: BufWriter<std::fs::File>,
    /// Bytes in the CURRENT segment; seeded from file length at open, bumped per receipt,
    /// reset on rotation. Drives the rotation threshold (ux.6a).
    bytes: u64,
    /// Latched when a rotation renamed the live file but could not create its replacement.
    /// Stops the shift from running again and cascade-destroying retained segments.
    rotation_broken: bool,
}

/// Thread-safe writer for action receipts. One per `agentd` process.
pub struct EvidenceWriter {
    keypair: Ed25519KeyPair,
    inner: Mutex<Inner>,
    pub_key_path: PathBuf,
    /// Diagnostic from chain resume at `open` time; `None` on a healthy boot (ux.6a).
    resume_note: Option<String>,
    /// Needed to rename segments at rotation (ux.6a).
    evidence_path: PathBuf,
    /// Segment size at which the current file is rotated.
    cap: u64,
}

impl EvidenceWriter {
    /// Open (or create) the evidence file. Loads an existing Ed25519 key from
    /// `key_path`, or generates a new one and persists it on first run.
    pub fn open(evidence_path: &Path, key_path: &Path) -> Result<Self> {
        Self::open_with_cap(evidence_path, key_path, MAX_EVIDENCE_BYTES)
    }

    /// `open` with an explicit rotation cap. Private so tests can drive rotation without
    /// writing 32 MiB (same pattern as `start_http_proxy_impl` in `egress.rs`).
    fn open_with_cap(evidence_path: &Path, key_path: &Path, cap: u64) -> Result<Self> {
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

        // Rotate BEFORE resuming, so an already-oversized chain resumes from a fresh segment
        // at genesis rather than being re-read and appended to.
        let mut rotate_note = None;
        if evidence_path.exists() {
            let len = std::fs::metadata(evidence_path)
                .with_context(|| format!("stat evidence file {}", evidence_path.display()))?
                .len();
            if len >= cap {
                rotate_note = rotate_segments(evidence_path, cap).err().map(|e| {
                    format!("evidence rotation at open failed ({e:#}); appending anyway")
                });
            }
        }

        // Bounded tail read + torn-tail repair + tail signature check (ux.6a). The keypair is
        // already loaded above, so the public half is available for the check.
        let resumed = resume_chain(evidence_path, Some(keypair.public_key().as_ref()))?;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(evidence_path)
            .with_context(|| format!("opening evidence file {}", evidence_path.display()))?;
        let bytes = file
            .metadata()
            .with_context(|| format!("stat {} after open", evidence_path.display()))?
            .len();

        let note = match (rotate_note, resumed.note) {
            (Some(a), Some(b)) => Some(format!("{a}; {b}")),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };

        Ok(Self {
            keypair,
            inner: Mutex::new(Inner {
                seq: resumed.seq,
                chain_prev_hash: resumed.chain_prev_hash,
                writer: BufWriter::new(file),
                bytes,
                rotation_broken: false,
            }),
            pub_key_path,
            resume_note: note,
            evidence_path: evidence_path.to_path_buf(),
            cap,
        })
    }

    /// Whether rotation has been latched off after a failure. Test-only: it exists so a
    /// rotation-failure test can PROVE it entered `rotate_locked`, which is precisely what the
    /// original (vacuous) version of that test failed to do.
    #[cfg(test)]
    fn rotation_is_broken(&self) -> bool {
        self.inner.lock().map(|i| i.rotation_broken).unwrap_or(false)
    }

    /// A human-readable note if the chain tail had to be repaired, failed its signature
    /// check, or could not be read from the bounded window at open time.
    ///
    /// `None` on every healthy boot. Recorded by `main.rs` as a flight event — it is
    /// diagnostic, never fatal.
    pub fn resume_note(&self) -> Option<&str> {
        self.resume_note.as_deref()
    }

    /// Record an allowed action. Returns the receipt sequence number.
    pub fn record_allowed(&self, action: &str, target: &str, principal: &str) -> Result<u64> {
        self.write_receipt(action, target, principal, "allowed")
    }

    /// Record a denied action. Returns the receipt sequence number.
    pub fn record_denied(&self, action: &str, target: &str, principal: &str) -> Result<u64> {
        self.write_receipt(action, target, principal, "denied")
    }

    /// Rotate the current segment while the `Inner` lock is held.
    ///
    /// Bounded growth is best-effort: a rotation failure logs and keeps appending rather than
    /// failing the receipt, because losing a receipt is strictly worse than an oversized file
    /// (same philosophy as `FlightRecorder`, which falls through on `set_len` failure).
    fn rotate_locked(&self, inner: &mut Inner) {
        // Flush first so buffered bytes land in the OLD segment, not the new one.
        if let Err(e) = inner.writer.flush() {
            // Latch here too: `inner.bytes` is not reset, so without this every subsequent
            // write re-enters rotation and re-attempts the same failing flush forever.
            tracing::warn!(
                "evidence flush before rotation failed: {e:#}; rotation DISABLED for this process"
            );
            inner.rotation_broken = true;
            return;
        }
        if let Err(e) = rotate_segments(&self.evidence_path, self.cap) {
            // Latch OFF rather than retrying on every receipt. With the delete-last ordering a
            // retry is no longer destructive, but it would still attempt a rename per receipt
            // forever; and a rotation that fails once (read-only dir, a name occupied by a
            // directory) fails the same way next time.
            tracing::warn!(
                "evidence rotation failed: {e:#}; rotation DISABLED for this process, \
                 continuing to append to the current segment"
            );
            inner.rotation_broken = true;
            return;
        }
        // The old fd still points at the RENAMED inode, so it must be replaced — reusing it
        // would keep growing the rotated segment and leave the new file empty forever.
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.evidence_path)
        {
            Ok(file) => {
                inner.writer = BufWriter::new(file);
                inner.seq = 0;
                inner.chain_prev_hash = GENESIS_HASH.to_string();
                inner.bytes = 0;
            }
            Err(e) => {
                // The rename already happened, so the live path is gone while `inner.writer`
                // still points at the RENAMED inode. Leaving it at that is actively
                // destructive: `inner.bytes` stays >= cap, so EVERY later write re-enters this
                // function, and `rotate_segments` unlinks `.3` then shifts `.2`->`.3` and
                // `.1`->`.2` BEFORE failing on the missing live path. Within a few writes the
                // inode actually being appended to is itself shifted into `.3` and unlinked,
                // while `write_receipt` keeps returning Ok. (An earlier comment here claimed
                // "a failure here is reported by the next write's error" — it is not.)
                // Latch rotation OFF: keep appending to the renamed inode, because durable
                // receipts matter more than bounded growth, and never shift again.
                tracing::error!(
                    "evidence rotation renamed the segment but could not create a new file \
                     ({e:#}); rotation DISABLED for this process to avoid cascading segment \
                     loss — receipts continue appending to the rotated segment"
                );
                inner.rotation_broken = true;
            }
        }
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

        // Rotate BEFORE building the body: the receipt must carry the post-rotation
        // `seq`/`chain_prev_hash`, or the new segment would not start at genesis.
        if inner.bytes >= self.cap && !inner.rotation_broken {
            self.rotate_locked(&mut inner);
        }

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
        // +1 for the newline. Drives the rotation threshold (ux.6a).
        inner.bytes = inner.bytes.saturating_add(line.len() as u64 + 1);
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

/// Rotate `evidence.jsonl` to `evidence.jsonl.1`, shifting existing segments down and
/// dropping the oldest (ux.6a, closes the rotation half of audit86-P2-4).
///
/// **Why rename and not truncate-in-place.** `FlightRecorder` rotates with `set_len(0)` to
/// preserve its inode for the otel `tail.rs` sentinel, which DISCARDS the old content — fine
/// for a best-effort log, unacceptable for an audit record. Nothing tails `evidence.jsonl`
/// (the only readers are `agentctl verify` and the operator), so rename is safe here.
///
/// **Why this needs no format or verifier change.** Each segment is a COMPLETE
/// genesis-anchored chain: the fresh file restarts at `(seq 0, GENESIS_HASH)`, so
/// `agentctl verify <segment> <pubkey>` passes on every segment with the shipped verifier.
/// Genesis anchoring only ever blocked in-place truncation, never rename.
///
/// Honest limit: the SEAM between segments is unprovable — a whole segment can be removed
/// undetectably. That is not new (deleting `evidence.jsonl` entirely already restarts at 0
/// and still verifies); it is documented in `THREAT_MODEL.md` §8.7.
fn rotate_segments(path: &Path, cap: u64) -> Result<()> {
    // ORDER MATTERS: every irreversible step goes LAST. An earlier version deleted the oldest
    // segment FIRST, so any later failure in this function (most reachably the post-rename
    // reopen failing with ENOSPC — precisely the full-disk condition that makes a log rotate)
    // had already destroyed a segment to accomplish nothing, and the caller retried on the
    // next receipt, walking the live inode down the slots and finally unlinking it.
    //
    // So: shift the retained segments UP first, using a temporary name for the one that falls
    // off the end, then rename the live file, and only unlink the displaced segment once every
    // fallible step has succeeded.
    let displaced = segment_path(path, EVIDENCE_SEGMENTS_KEPT);
    let parked = segment_path(path, EVIDENCE_SEGMENTS_KEPT).with_extension("evicting");
    let had_displaced = displaced.exists();
    if had_displaced {
        std::fs::rename(&displaced, &parked)
            .with_context(|| format!("parking {} before eviction", displaced.display()))?;
    }
    for n in (1..EVIDENCE_SEGMENTS_KEPT).rev() {
        let from = segment_path(path, n);
        if from.exists() {
            let to = segment_path(path, n + 1);
            std::fs::rename(&from, &to)
                .with_context(|| format!("shifting {} -> {}", from.display(), to.display()))?;
        }
    }
    let first = segment_path(path, 1);
    std::fs::rename(path, &first)
        .with_context(|| format!("rotating {} -> {}", path.display(), first.display()))?;
    // Last, and only now irreversible — and deliberately BEST-EFFORT.
    //
    // Everything durability-relevant has already succeeded: the live file is renamed and the
    // caller is about to create its replacement. If this unlink failed and we returned `Err`,
    // `rotate_locked` would treat the whole rotation as not-having-happened, skip the reopen,
    // and leave `evidence.jsonl` MISSING while `inner.writer` still points at the renamed
    // inode — `write_receipt` would keep returning Ok into `.1` and `agentctl verify
    // evidence.jsonl` would fail ENOENT. That is the same shape as the cascade this reorder
    // exists to remove, reached by a new route (found by the fix-review red team), and it is
    // reachable through the very case the caller's comment anticipates: if `.3` is a directory
    // the park, shift and live rename all succeed and only this unlink fails with EISDIR.
    // A stranded `.evicting` file is a bounded, reported nuisance; a missing live file is not.
    if had_displaced {
        tracing::warn!(
            segment = %displaced.display(),
            "evidence segment retention reached; the oldest segment of signed receipts is being \
             DELETED — archive segments externally if you need longer retention"
        );
        if let Err(e) = std::fs::remove_file(&parked) {
            tracing::error!(
                parked = %parked.display(),
                "rotation completed but the evicted segment could not be unlinked ({e:#}); it is \
                 stranded under that name and will be reclaimed by the next rotation"
            );
        }
    }
    tracing::info!(
        cap_bytes = cap,
        segment = %first.display(),
        "evidence.jsonl reached its cap — rotated to a new segment (each segment verifies independently)"
    );
    Ok(())
}

fn segment_path(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// What `resume_chain` recovered from an existing chain, plus any repair it performed.
struct Resume {
    seq: u64,
    chain_prev_hash: String,
    /// Set when the tail had to be repaired, failed its signature check, or could not be
    /// read from the bounded window. Recorded as a flight event by `main.rs`; NEVER fatal.
    note: Option<String>,
}

impl Resume {
    fn genesis() -> Self {
        Self { seq: 0, chain_prev_hash: GENESIS_HASH.to_string(), note: None }
    }
}

/// Hash one chain line exactly as the pre-ux.6a full scan did.
///
/// That scan used `str::lines()`, which excludes the `\n` delimiter and strips ONE trailing
/// `\r`. Any divergence here changes `chain_prev_hash` and so breaks EVERY existing
/// `evidence.jsonl` at its next append. `resume_tail_matches_legacy_full_scan` is the guard.
///
/// Precise invariant: equality with `str::lines()` holds for every NEWLINE-TERMINATED line.
/// It does not hold for a final line that ends in `\r` with no `\n` — std strips `\r` only
/// after stripping a `\n`, so the legacy scan hashed the `\r` and this does not. That case is
/// harmless in practice, but only because of a side effect elsewhere: such a line takes the
/// unterminated-but-complete branch, which appends the missing `\n` BEFORE anything is
/// chained onto it, after which legacy, `verify_chain` and this function all agree. Do not
/// widen the claim beyond newline-terminated lines.
fn line_hash(line: &[u8]) -> String {
    let stripped = line.strip_suffix(b"\r").unwrap_or(line);
    hex_encode(digest::digest(&digest::SHA256, stripped).as_ref())
}

/// Read the last `window` bytes of `path`. Returns the buffer and the offset it starts at.
fn read_tail(path: &Path, len: u64, window: u64) -> Result<(Vec<u8>, u64)> {
    let start = len.saturating_sub(window);
    let take = len - start;
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("opening evidence file {}", path.display()))?;
    f.seek(SeekFrom::Start(start))
        .with_context(|| format!("seeking to {start} in {}", path.display()))?;
    let mut buf = Vec::with_capacity(take as usize);
    f.take(take)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading tail of {}", path.display()))?;
    Ok((buf, start))
}

/// The pre-ux.6a algorithm: read the whole file, hash every line, count lines for `seq`.
/// Retained as the fallback path so the bounded reader can never introduce a NEW boot
/// failure — `main.rs` treats an error here as fail-closed.
fn resume_chain_full_scan(path: &Path) -> Result<(u64, String)> {
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

/// Recover `(seq, chain_prev_hash)` by reading only a bounded tail of the evidence file,
/// repairing a torn final write, and signature-checking the tail receipt (ux.6a).
///
/// Closes `audit86-P2-4` (the pre-ux.6a version re-hashed the WHOLE file at every boot,
/// O(file) forever, on a path `main.rs` treats as fail-closed) and `audit-S5` (the tail was
/// trusted on resume with no signature check).
///
/// **This function must never return `Err` for any input the old one accepted.** Anything the
/// bounded path cannot handle degrades to `resume_chain_full_scan` with a note, because a new
/// error here is a new unbootable `agentd`.
fn resume_chain(path: &Path, pub_key: Option<&[u8]>) -> Result<Resume> {
    if !path.exists() {
        return Ok(Resume::genesis());
    }
    let len = std::fs::metadata(path)
        .with_context(|| format!("stat evidence file {}", path.display()))?
        .len();
    if len == 0 {
        return Ok(Resume::genesis());
    }

    match resume_from_tail(path, len, pub_key) {
        Ok(r) => Ok(r),
        Err(e) => {
            let (seq, chain_prev_hash) = resume_chain_full_scan(path)?;
            Ok(Resume {
                seq,
                chain_prev_hash,
                note: Some(format!(
                    "bounded tail resume failed ({e:#}); fell back to the full scan"
                )),
            })
        }
    }
}

/// Upper bound on a plausible resumed `seq`, given the file length.
///
/// `seq` now comes from the tail receipt rather than a line count, and that receipt's
/// signature is deliberately NOT enforced (see `finish_tail`), so the value is UNTRUSTED. One
/// appended line claiming `"seq":18446744073709551615` would otherwise reach `write_receipt`'s
/// increment and either panic inside the mutex (debug, poisoning it for every later caller) or
/// wrap to 0 in release, making every subsequent GENUINE receipt fail `verify_chain` with a
/// sequence gap — permanent chain poisoning from one appended line. A chain of N lines has
/// `seq <= N-1` and every line is at least one byte, so the file length is a hard ceiling.
/// Out-of-range means "not a chain we can resume": fall back to the full scan, which derives
/// `seq` from the actual line count.
fn seq_is_plausible(seq: u64, len: u64) -> bool {
    seq < len
}

fn resume_from_tail(path: &Path, len: u64, pub_key: Option<&[u8]>) -> Result<Resume> {
    let mut window = RESUME_TAIL_WINDOW;
    let (mut buf, mut start) = read_tail(path, len, window)?;

    // The last COMPLETE line must lie entirely inside the window. When the file ends with a
    // delimiter that needs two `\n` (one closing the line, one opening it); reading from
    // offset 0 makes the start of the file the opening boundary instead.
    while start > 0 && buf.iter().filter(|b| **b == b'\n').count() < 2 {
        anyhow::ensure!(
            window < RESUME_TAIL_MAX,
            "no complete receipt line in the last {window} bytes"
        );
        window = (window * 2).min(RESUME_TAIL_MAX);
        let grown = read_tail(path, len, window)?;
        buf = grown.0;
        start = grown.1;
    }

    let mut note: Option<String> = None;
    let last_nl = buf.iter().rposition(|b| *b == b'\n');

    // Bytes after the final `\n` (or the whole buffer when the file has no `\n` at all and we
    // read from offset 0) are an unterminated trailing fragment.
    let frag_len = match last_nl {
        Some(i) => buf.len() - (i + 1),
        None => buf.len(),
    };

    if frag_len > 0 {
        let frag = &buf[buf.len() - frag_len..];
        if let Ok(receipt) = serde_json::from_slice::<ActionReceipt>(frag) {
            // A COMPLETE receipt whose trailing newline is missing. Restore the delimiter so
            // the next `writeln!` cannot fuse two records into one unparseable line. Nothing
            // is discarded.
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .with_context(|| format!("reopening {} to terminate tail", path.display()))?;
            f.write_all(b"\n")
                .with_context(|| format!("appending newline to {}", path.display()))?;
            f.sync_data().context("fsync after terminating tail")?;
            let hash = line_hash(frag);
            return finish_tail(
                receipt,
                hash,
                pub_key,
                Some(
                    "final receipt was missing its newline; delimiter restored (no data lost)"
                        .to_string(),
                ),
                len,
            );
        }

        // A torn write. These bytes were never `sync_data()`'d, so no durable receipt is
        // lost. Discard back to the last record boundary; otherwise the next append fuses
        // onto the fragment and the chain becomes PERMANENTLY unverifiable while agentd
        // keeps booting happily (the pre-ux.6a behaviour).
        // NEVER truncate to 0. With no `\n` anywhere this is not a chain with a torn tail —
        // it is something else (an operator's archival note, a copied fragment), and
        // `set_len(0)` would destroy it while reporting "discarded N trailing byte(s) from a
        // torn final write". Hand it to the full-scan fallback instead.
        let keep = match last_nl {
            Some(i) => start + i as u64 + 1,
            None => anyhow::bail!(
                "no record boundary anywhere in {}; refusing to truncate the whole file",
                path.display()
            ),
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("reopening {} to repair tail", path.display()))?
            .set_len(keep)
            .with_context(|| format!("truncating {} to {keep}", path.display()))?;
        note = Some(format!(
            "discarded {frag_len} trailing byte(s) from a torn final write (never fsynced)"
        ));
        if keep == 0 {
            return Ok(Resume { seq: 0, chain_prev_hash: GENESIS_HASH.to_string(), note });
        }
        buf.truncate(last_nl.expect("keep > 0 implies a newline was found") + 1);
    }

    // `buf` now ends with `\n`. The last complete line sits between the previous delimiter
    // (or the buffer start) and that final one.
    //
    // Guard the subtraction: `buf` being empty here needs stat-size != 0 while the read yields
    // zero bytes (a concurrent truncation, or a path whose size disagrees with readable
    // bytes). Rare, but `panic = "abort"` is set workspace-wide and a panic is not an `Err`, so
    // an underflow here would abort PID 1 past `resume_chain`'s fallback. Same class as the
    // `hex_decode` char-boundary panic fixed above.
    anyhow::ensure!(!buf.is_empty(), "tail read returned no bytes");
    let body = &buf[..buf.len() - 1];
    let line_start = body.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let line = &body[line_start..];

    let receipt: ActionReceipt =
        serde_json::from_slice(line).context("parsing tail receipt")?;
    finish_tail(receipt, line_hash(line), pub_key, note, len)
}

/// Signature-check the tail receipt and assemble the `Resume`.
///
/// A bad signature WARNS and boots (`audit-S5`). It must not refuse: `resume_chain` verified
/// nothing before ux.6a, so failing closed here would brick every operator who ever archived,
/// copied, or hand-edited `evidence.jsonl`. Full verification stays `agentctl verify`'s job.
fn finish_tail(
    receipt: ActionReceipt,
    hash: String,
    pub_key: Option<&[u8]>,
    note: Option<String>,
    len: u64,
) -> Result<Resume> {
    anyhow::ensure!(
        seq_is_plausible(receipt.seq, len),
        "tail receipt claims seq={} in a {}-byte file — implausible, refusing to resume from it",
        receipt.seq,
        len
    );
    let mut note = note;
    if let Some(pk) = pub_key {
        if !tail_signature_ok(&receipt, pk) {
            let warning = format!(
                "tail receipt seq={} failed its Ed25519 signature check; the chain is \
                 already broken — run `agentctl verify` (appending anyway)",
                receipt.seq
            );
            tracing::warn!("{warning}");
            note = Some(match note {
                Some(prev) => format!("{prev}; {warning}"),
                None => warning,
            });
        }
    }
    Ok(Resume { seq: receipt.seq.saturating_add(1), chain_prev_hash: hash, note })
}

fn tail_signature_ok(receipt: &ActionReceipt, pub_key: &[u8]) -> bool {
    let body = ReceiptBody {
        seq: receipt.seq,
        action: receipt.action.clone(),
        target: receipt.target.clone(),
        principal: receipt.principal.clone(),
        verdict: receipt.verdict.clone(),
        ts: receipt.ts.clone(),
        chain_prev_hash: receipt.chain_prev_hash.clone(),
    };
    let Ok(canonical) = serde_json::to_string(&body) else {
        return false;
    };
    let Ok(sig) = hex_decode(&receipt.signature) else {
        return false;
    };
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pub_key)
        .verify(canonical.as_bytes(), &sig)
        .is_ok()
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
    use std::os::unix::fs::PermissionsExt;
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

    // ---------------------------------------------------------------------------------
    // ux.6a Step 1 — bounded tail resume (closes audit86-P2-4 + audit-S5)
    // ---------------------------------------------------------------------------------

    /// The pre-ux.6a algorithm, copied VERBATIM as an independent reference. Deliberately
    /// not delegating to `resume_chain_full_scan`: if that helper is ever edited, this copy
    /// keeps the differential test honest.
    fn legacy_resume(path: &Path) -> (u64, String) {
        if !path.exists() {
            return (0, GENESIS_HASH.to_string());
        }
        let content = std::fs::read_to_string(path).unwrap();
        let mut seq = 0u64;
        let mut last_hash = GENESIS_HASH.to_string();
        for line in content.lines() {
            last_hash = hex_encode(digest::digest(&digest::SHA256, line.as_bytes()).as_ref());
            seq += 1;
        }
        (seq, last_hash)
    }

    /// Put a test dir back to rwx so `TempDir`'s cleanup can remove it.
    fn restore_dir_perms(p: &Path) {
        if let Ok(md) = std::fs::metadata(p) {
            let mut perms = md.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(p, perms);
        }
    }

    /// Restores directory permissions on the UNWIND path too.
    ///
    /// Not a nicety. A test that chmods its `TempDir` to `0o500` and restores as its last
    /// statement skips the restore on any assertion failure; `TempDir::drop` then cannot unlink
    /// the directory, and it leaks — holding `egress.pkcs8`, an Ed25519 PRIVATE SIGNING KEY,
    /// in an undeletable `dr-x------` directory. Observed for real during this increment's
    /// review, once per failing run. Bind this immediately before `set_permissions`.
    struct RestorePerms(PathBuf);
    impl Drop for RestorePerms {
        fn drop(&mut self) {
            restore_dir_perms(&self.0);
        }
    }

    fn write_n_receipts(ev: &Path, key: &Path, n: usize) {
        let w = EvidenceWriter::open(ev, key).unwrap();
        for i in 0..n {
            w.record_allowed("inference", "claude-sonnet-4-6", &format!("agent_{i}"))
                .unwrap();
        }
    }

    /// THE guard for this step. The bounded reader must agree with the shipped algorithm
    /// byte-for-byte on every well-formed chain — `chain_prev_hash` feeds the next append, so
    /// any divergence silently breaks every `evidence.jsonl` in the field at its next write.
    #[test]
    fn resume_tail_matches_legacy_full_scan() {
        for n in [0usize, 1, 2, 17] {
            let dir = TempDir::new().unwrap();
            let ev = dir.path().join("evidence.jsonl");
            let key = dir.path().join("egress.pkcs8");
            write_n_receipts(&ev, &key, n);

            let (legacy_seq, legacy_hash) = legacy_resume(&ev);
            let got = resume_chain(&ev, None).unwrap();

            assert_eq!(got.seq, legacy_seq, "seq diverged at n={n}");
            assert_eq!(got.chain_prev_hash, legacy_hash, "chain hash diverged at n={n}");
            assert!(got.note.is_none(), "healthy chain must produce no note at n={n}");
        }
    }

    /// `seq` now comes from the tail receipt instead of a line count, so resume reads the
    /// same field `verify_chain` checks and the two can no longer disagree.
    #[test]
    fn resume_reads_seq_from_last_receipt_not_line_count() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 5);

        // Excise a line from the middle: the chain is now broken, but the tail still says 4.
        let content = std::fs::read_to_string(&ev).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        let without_second: String =
            lines.iter().enumerate().filter(|(i, _)| *i != 1).fold(String::new(), |mut s, (_, l)| {
                s.push_str(l);
                s.push('\n');
                s
            });
        std::fs::write(&ev, without_second).unwrap();

        assert_eq!(legacy_resume(&ev).0, 4, "the old algorithm counts lines");
        assert_eq!(
            resume_chain(&ev, None).unwrap().seq,
            5,
            "the new one reads seq from the tail receipt"
        );
    }

    /// Behavioural proof of boundedness: the answer is one the counting algorithm cannot
    /// produce. The prefix is deliberately malformed — this probes the read window, not
    /// correctness on a real chain (that is `resume_tail_matches_legacy_full_scan`).
    #[test]
    fn resume_ignores_body_and_reads_only_the_tail() {
        let dir = TempDir::new().unwrap();
        let key = dir.path().join("egress.pkcs8");
        let seed = dir.path().join("seed.jsonl");
        write_n_receipts(&seed, &key, 1);
        let line = std::fs::read_to_string(&seed).unwrap().lines().next().unwrap().to_owned();

        // 8 MiB sparse prefix, newline-terminated so a complete line is findable, then one
        // real receipt.
        let ev = dir.path().join("evidence.jsonl");
        let f = std::fs::File::create(&ev).unwrap();
        f.set_len(8 * 1024 * 1024).unwrap();
        drop(f);
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(f, "\n{line}").unwrap();
        drop(f);

        assert_eq!(
            legacy_resume(&ev).0,
            2,
            "the full scan sees the 8 MiB NUL run as a line and counts 2"
        );
        let got = resume_chain(&ev, None).unwrap();
        assert_eq!(got.seq, 1, "the bounded reader only ever saw the tail receipt (seq 0)");
        assert_eq!(got.chain_prev_hash, line_hash(line.as_bytes()));
    }

    /// A torn final write (process killed mid-`writeln!`) must be discarded, not chained
    /// onto. Pre-ux.6a, the fragment was hashed and the next append fused onto it, leaving a
    /// permanently unverifiable chain while `agentd` booted happily.
    #[test]
    fn resume_repairs_torn_tail_fragment() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 3);

        let content = std::fs::read_to_string(&ev).unwrap();
        let third_start = content.match_indices('\n').nth(1).unwrap().0 + 1;
        // Keep the first two records plus half of the third.
        std::fs::write(&ev, &content.as_bytes()[..third_start + 20]).unwrap();

        let w = EvidenceWriter::open(&ev, &key).unwrap();
        assert!(w.resume_note().is_some(), "the repair must be reported");
        assert!(w.resume_note().unwrap().contains("torn"), "note names the cause");
        let after = std::fs::read_to_string(&ev).unwrap();
        assert_eq!(after.lines().count(), 2, "truncated back to the record boundary");
        assert!(after.ends_with('\n'));

        // The chain is intact and appendable.
        w.record_allowed("inference", "m", "a").unwrap();
        drop(w);
        assert_eq!(verify_chain(&ev, &key.with_extension("pub")).unwrap(), 3);
    }

    /// A complete receipt whose trailing newline is missing must gain the delimiter, losing
    /// nothing — otherwise the next append fuses two records into one unparseable line.
    #[test]
    fn resume_appends_newline_when_tail_complete_but_unterminated() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 3);

        let content = std::fs::read_to_string(&ev).unwrap();
        std::fs::write(&ev, content.trim_end_matches('\n')).unwrap();

        let w = EvidenceWriter::open(&ev, &key).unwrap();
        assert!(w.resume_note().unwrap().contains("newline"));
        w.record_allowed("inference", "m", "a").unwrap();
        drop(w);
        // All THREE originals survived and the fourth chained cleanly.
        assert_eq!(verify_chain(&ev, &key.with_extension("pub")).unwrap(), 4);
    }

    /// audit-S5: the tail signature is now checked on resume. It must WARN and boot — never
    /// refuse. `resume_chain` verified nothing before ux.6a, so failing closed here would
    /// brick any operator who archived, copied, or hand-edited `evidence.jsonl`.
    #[test]
    fn resume_warns_but_boots_on_unsigned_tail() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 2);

        let content = std::fs::read_to_string(&ev).unwrap();
        let last: ActionReceipt =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        let forged = ActionReceipt {
            seq: 2,
            action: "inference".into(),
            target: "m".into(),
            principal: "attacker".into(),
            verdict: "allowed".into(),
            ts: last.ts.clone(),
            chain_prev_hash: line_hash(content.lines().last().unwrap().as_bytes()),
            signature: "00".repeat(64),
        };
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(f, "{}", serde_json::to_string(&forged).unwrap()).unwrap();
        drop(f);

        // Boots. This is the whole point.
        let w = EvidenceWriter::open(&ev, &key).unwrap();
        let note = w.resume_note().expect("bad tail signature must be reported");
        assert!(note.contains("signature"), "note names the cause: {note}");
        assert!(note.contains("agentctl verify"), "note points at the real verifier: {note}");
        drop(w);

        // And the offline verifier still catches it, which is where detection belongs.
        assert!(verify_chain(&ev, &key.with_extension("pub")).is_err());
    }

    /// The window-growth loop, which nothing else covers. When the last COMPLETE line does
    /// not fit in the initial 64 KiB window, `resume_from_tail` must double the window until
    /// two delimiters are visible — and still agree with the legacy full scan.
    #[test]
    fn resume_grows_the_window_when_the_tail_line_exceeds_it() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 1);

        // Append a receipt whose `principal` alone is larger than RESUME_TAIL_WINDOW, so the
        // last 64 KiB of the file sits entirely INSIDE that final line and contains only one
        // newline. Without growth the reader would mistake a mid-line fragment for a record.
        let first = std::fs::read_to_string(&ev).unwrap();
        let big = ActionReceipt {
            seq: 1,
            action: "inference".into(),
            target: "m".into(),
            principal: "x".repeat((RESUME_TAIL_WINDOW as usize) + 4096),
            verdict: "allowed".into(),
            ts: "2026-07-29T00:00:00Z".into(),
            chain_prev_hash: line_hash(first.lines().next().unwrap().as_bytes()),
            signature: "00".repeat(64),
        };
        let big_line = serde_json::to_string(&big).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(f, "{big_line}").unwrap();
        drop(f);
        assert!(
            std::fs::metadata(&ev).unwrap().len() > RESUME_TAIL_WINDOW,
            "precondition: the file must exceed the initial window"
        );

        let (legacy_seq, legacy_hash) = legacy_resume(&ev);
        let got = resume_chain(&ev, None).unwrap();
        assert_eq!(got.seq, legacy_seq, "growth path must agree with the full scan on seq");
        assert_eq!(got.chain_prev_hash, legacy_hash, "…and on the chain hash");
        assert_eq!(got.chain_prev_hash, line_hash(big_line.as_bytes()));
        assert!(got.note.is_none(), "growing the window is not a defect — no note");
    }

    /// THE invariant of this increment: the bounded reader may be slower or noisier than the
    /// legacy scan, but it must NEVER fail where the legacy scan succeeded. `main.rs` treats
    /// an `EvidenceWriter::open` error as fail-closed, so a new `Err` here is a new
    /// unbootable `agentd`. Anything the bounded path cannot handle degrades to the full scan.
    #[test]
    fn resume_never_errors_where_the_legacy_scan_succeeded() {
        let dir = TempDir::new().unwrap();
        let key = dir.path().join("egress.pkcs8");
        let seed = dir.path().join("seed.jsonl");
        write_n_receipts(&seed, &key, 2);
        let seeded = std::fs::read_to_string(&seed).unwrap();
        let one = seeded.lines().next().unwrap();

        // Each case is a file body that the LEGACY algorithm accepts (it only ever fails on
        // an I/O error or invalid UTF-8), so the bounded reader must accept it too.
        let cases: Vec<(&str, String)> = vec![
            ("empty", String::new()),
            ("single newline", "\n".to_string()),
            ("blank lines only", "\n\n\n".to_string()),
            ("not json at all", "hello world\n".to_string()),
            ("json but not a receipt", "{\"a\":1}\n".to_string()),
            ("truncated json", "{\"seq\":0,\"acti".to_string()),
            ("valid then garbage line", format!("{one}\ngarbage\n")),
            ("valid then torn fragment", format!("{one}\n{{\"seq\":1,\"ac")),
            ("trailing blank after valid", format!("{one}\n\n")),
            ("crlf line endings", format!("{one}\r\n")),
            ("no trailing newline", one.to_string()),
            ("only a huge unterminated line", "y".repeat(200_000)),
        ];

        for (name, body) in cases {
            let ev = dir.path().join(format!("case-{}.jsonl", name.replace(' ', "-")));
            std::fs::write(&ev, &body).unwrap();
            // Legacy accepts every one of these.
            let legacy = std::panic::catch_unwind(|| legacy_resume(&ev));
            assert!(legacy.is_ok(), "precondition: legacy handles {name:?}");
            // So the bounded reader must not turn it into a boot failure.
            let got = resume_chain(&ev, None);
            assert!(
                got.is_ok(),
                "case {name:?} became a NEW boot failure: {:?}",
                got.err()
            );
        }
    }

    /// A healthy chain must never produce a note — otherwise the diagnostic is noise.
    #[test]
    fn resume_note_is_none_on_a_healthy_chain() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 4);
        let w = EvidenceWriter::open(&ev, &key).unwrap();
        assert!(w.resume_note().is_none());
    }

    // ---------------------------------------------------------------------------------
    // ux.6a Step 2 — rename-based segment rotation (closes the rest of audit86-P2-4)
    // ---------------------------------------------------------------------------------

    /// The property that makes rotation free: every segment is a COMPLETE genesis-anchored
    /// chain, so the SHIPPED verifier passes on each one with no format or verifier change.
    /// Genesis anchoring only ever blocked in-place truncation, never rename.
    #[test]
    fn rotation_renames_at_cap_and_both_segments_verify_independently() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let pubk = key.with_extension("pub");

        // Cap small enough that the 3rd receipt triggers rotation.
        let w = EvidenceWriter::open_with_cap(&ev, &key, 600).unwrap();
        for i in 0..6 {
            w.record_allowed("inference", "m", &format!("agent_{i}")).unwrap();
        }
        drop(w);

        let seg1 = dir.path().join("evidence.jsonl.1");
        assert!(seg1.exists(), "rotation must produce evidence.jsonl.1");

        // Each segment verifies ON ITS OWN, with the unchanged verifier — and together they
        // still account for all 6 receipts. A 600-byte cap rotates more than once, so sum
        // across every retained segment rather than assuming a single rotation.
        let mut total = verify_chain(&ev, &pubk).expect("live segment must verify");
        for n in 1..=EVIDENCE_SEGMENTS_KEPT {
            let seg = segment_path(&ev, n);
            if seg.exists() {
                total += verify_chain(&seg, &pubk)
                    .unwrap_or_else(|e| panic!("rotated segment .{n} must verify: {e:#}"));
            }
        }
        assert_eq!(total, 6, "no receipt was lost across rotation");

        // The new segment starts at genesis — that is what makes it independently verifiable.
        let first: ActionReceipt =
            serde_json::from_str(std::fs::read_to_string(&ev).unwrap().lines().next().unwrap())
                .unwrap();
        assert_eq!(first.seq, 0);
        assert_eq!(first.chain_prev_hash, GENESIS_HASH);
    }

    /// The fd-follows-rename trap: after `rename` the old descriptor still points at the
    /// RENAMED inode. Reusing it would keep growing the rotated segment and leave the new
    /// file empty forever — with every test above still green.
    #[test]
    fn rotation_writes_to_the_new_inode_not_the_renamed_one() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");

        let w = EvidenceWriter::open_with_cap(&ev, &key, 600).unwrap();
        for _ in 0..3 {
            w.record_allowed("inference", "m", "a").unwrap();
        }
        let seg1 = dir.path().join("evidence.jsonl.1");
        assert!(seg1.exists(), "precondition: rotation happened");
        let live_before = std::fs::metadata(&ev).unwrap().len();
        let rotated_before = std::fs::metadata(&seg1).unwrap().len();

        w.record_allowed("inference", "m", "post-rotation").unwrap();
        drop(w);

        assert!(
            std::fs::metadata(&ev).unwrap().len() > live_before,
            "the post-rotation receipt must land in the NEW file"
        );
        assert_eq!(
            std::fs::metadata(&seg1).unwrap().len(),
            rotated_before,
            "the rotated segment must be frozen — a stale fd would still be growing it"
        );
    }

    /// run.1-ar-01's lesson applied here: the threshold is seeded from file metadata at open,
    /// so an already-oversized chain rotates on startup instead of being re-read forever.
    /// Sparse file so no 32 MiB is actually written.
    #[test]
    fn rotation_at_open_when_preexisting_file_over_cap() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 2);
        // Inflate past the cap without writing the bytes.
        let f = std::fs::OpenOptions::new().write(true).open(&ev).unwrap();
        f.set_len(4096).unwrap();
        drop(f);

        let w = EvidenceWriter::open_with_cap(&ev, &key, 2048).unwrap();
        assert!(
            dir.path().join("evidence.jsonl.1").exists(),
            "an oversized chain must rotate at open, not be resumed"
        );
        w.record_allowed("inference", "m", "fresh").unwrap();
        drop(w);
        // The fresh segment is a valid one-receipt chain from genesis.
        assert_eq!(verify_chain(&ev, &key.with_extension("pub")).unwrap(), 1);
    }

    /// Segments shift down and retention is bounded. The oldest is dropped only when the
    /// shift runs, and `rotate_segments` logs it rather than dropping silently.
    #[test]
    fn rotation_shifts_segments_and_bounds_retention() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");

        // Force many rotations.
        let w = EvidenceWriter::open_with_cap(&ev, &key, 400).unwrap();
        for i in 0..20 {
            w.record_allowed("inference", "m", &format!("agent_{i}")).unwrap();
        }
        drop(w);

        let existing: Vec<usize> = (1..=EVIDENCE_SEGMENTS_KEPT + 2)
            .filter(|n| segment_path(&ev, *n).exists())
            .collect();
        assert_eq!(
            existing,
            (1..=EVIDENCE_SEGMENTS_KEPT).collect::<Vec<_>>(),
            "exactly the retained segments exist, contiguously, with no .{} \
             beyond the bound",
            EVIDENCE_SEGMENTS_KEPT + 1
        );
        // Every retained segment is still independently verifiable.
        let pubk = key.with_extension("pub");
        for n in 1..=EVIDENCE_SEGMENTS_KEPT {
            verify_chain(&segment_path(&ev, n), &pubk)
                .unwrap_or_else(|e| panic!("segment .{n} must verify: {e:#}"));
        }

        // The eviction must actually UNLINK the parked segment, not orphan it. Without this the
        // documented (KEPT + 1) * MAX bound silently becomes (KEPT + 2) * MAX and a stray file
        // of agent ids / egress targets sits in the evidence dir forever — and every test still
        // passed with the final `remove_file` stubbed out.
        let parked = segment_path(&ev, EVIDENCE_SEGMENTS_KEPT).with_extension("evicting");
        assert!(!parked.exists(), "the parked-for-eviction segment must be unlinked");
        // Bound the whole directory, not just the numbered slots.
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("evidence.jsonl"))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "evidence.jsonl",
                "evidence.jsonl.1",
                "evidence.jsonl.2",
                "evidence.jsonl.3"
            ],
            "retention must bound the whole directory"
        );
    }

    /// Rotation must never cost a receipt, and a FAILED rotation must never cost a segment.
    ///
    /// The version of this test that shipped in the build commit was vacuous twice over, and
    /// that is what let the segment-cascade defect through: it used `cap = 400` while a receipt
    /// line is ~370 B, so the second write never crossed the threshold and `rotate_locked` was
    /// never entered at all; and its sabotage (one directory at `.1`) could not fail the
    /// rename, because the shift renames `.1` -> `.2` first and renaming a directory onto a
    /// free name succeeds on Unix, freeing `.1` for the live file. Both are fixed here.
    #[test]
    fn rotation_failure_costs_neither_a_receipt_nor_a_segment() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");

        // Cap BELOW one receipt line, so the second write is guaranteed to cross it.
        let w = EvidenceWriter::open_with_cap(&ev, &key, 300).unwrap();
        w.record_allowed("inference", "m", "a").unwrap();
        let after_first = std::fs::metadata(&ev).unwrap().len();
        assert!(after_first >= 300, "precondition: the next write must trigger rotation");

        // Sabotage that PROVABLY fails. Occupying `.1`/`.2` with directories does NOT work:
        // the shift runs `.2 -> .3` before `.1 -> .2`, so `.2` is already vacated and renaming
        // a directory onto a free name succeeds — that mistake is exactly why the original
        // version of this test was green while rotation was broken. Instead make the DIRECTORY
        // read-only (r-x): every rename/unlink inside it fails with EACCES, while the receipt
        // still lands because `write_receipt` appends through an already-open fd.
        for n in [1, 2] {
            std::fs::write(dir.path().join(format!("evidence.jsonl.{n}")), b"segment\n").unwrap();
        }
        let _restore = RestorePerms(dir.path().to_path_buf());
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        // The receipt still lands.
        let seq = w
            .record_allowed("inference", "m", "b")
            .expect("a rotation failure must never fail the receipt");
        assert_eq!(seq, 1, "the chain continued in place");
        // NON-VACUITY: prove rotation was actually attempted and failed. Without this the test
        // passes just as happily when the cap is never crossed — the original bug.
        assert!(
            w.rotation_is_broken(),
            "rotation was never attempted — this test would be vacuous"
        );

        // And nothing was destroyed: the live file is still the live file, and both existing
        // segments survive byte-for-byte.
        assert!(ev.exists(), "the live evidence file must still exist");
        for n in [1, 2] {
            let seg = dir.path().join(format!("evidence.jsonl.{n}"));
            assert_eq!(
                std::fs::read(&seg).unwrap(),
                b"segment\n",
                "segment .{n} was disturbed by a failed rotation"
            );
        }
        drop(w);
        assert_eq!(verify_chain(&ev, &key.with_extension("pub")).unwrap(), 2);
    }

    /// The cascade guard. A rotation that keeps failing must be attempted ONCE and then latched
    /// off — otherwise every subsequent receipt re-enters `rotate_segments`, and the old
    /// delete-first ordering walked the live inode down the slots and finally unlinked it while
    /// `write_receipt` kept returning Ok.
    #[test]
    fn a_failing_rotation_is_latched_off_and_never_cascades() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let w = EvidenceWriter::open_with_cap(&ev, &key, 300).unwrap();
        w.record_allowed("inference", "m", "seed").unwrap();
        for n in [1, 2] {
            std::fs::write(dir.path().join(format!("evidence.jsonl.{n}")), b"segment\n").unwrap();
        }
        let _restore = RestorePerms(dir.path().to_path_buf());
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        // Many writes, all past the cap.
        for i in 0..10 {
            w.record_allowed("inference", "m", &format!("a{i}"))
                .expect("every receipt must survive a broken rotation");
        }
        assert!(w.rotation_is_broken(), "rotation must have been attempted and latched off");
        drop(w);

        // Nothing shifted, nothing deleted, no stray parked segment, and every receipt is in
        // one still-valid chain.
        for n in [1, 2] {
            let seg = dir.path().join(format!("evidence.jsonl.{n}"));
            assert_eq!(std::fs::read(&seg).unwrap(), b"segment\n", "segment .{n} was disturbed");
        }
        assert!(
            !dir.path().join("evidence.jsonl.3").exists(),
            "a latched-off rotation must not create further segments"
        );
        assert!(
            !dir.path().join("evidence.jsonl.evicting").exists(),
            "the parked-for-eviction temp name must never be left behind"
        );
        assert_eq!(verify_chain(&ev, &key.with_extension("pub")).unwrap(), 11);
    }

    /// RT-1 regression guard. The delete-last reorder put a FALLIBLE unlink after the live
    /// rename, and `rotate_locked` treated its `Err` as "rotation did not happen" — skipping the
    /// reopen and leaving `evidence.jsonl` MISSING while writes kept succeeding into the renamed
    /// segment. Reachable exactly as here: with `.3` a directory, the park, shift and live
    /// rename all succeed and only the unlink fails with EISDIR.
    #[test]
    fn eviction_failure_still_leaves_a_live_evidence_file() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        // Occupy every slot so a rotation must evict, and make `.3` a DIRECTORY so the final
        // unlink fails while every rename succeeds.
        for n in [1, 2] {
            std::fs::write(dir.path().join(format!("evidence.jsonl.{n}")), b"seg\n").unwrap();
        }
        std::fs::create_dir(dir.path().join("evidence.jsonl.3")).unwrap();

        let w = EvidenceWriter::open_with_cap(&ev, &key, 300).unwrap();
        w.record_allowed("inference", "m", "a").unwrap();
        // Crosses the cap → rotation runs, evicts, and the unlink fails.
        w.record_allowed("inference", "m", "b").unwrap();

        assert!(
            ev.exists(),
            "a failed EVICTION must not leave the live evidence file missing"
        );
        assert!(!w.rotation_is_broken(), "eviction failure is not rotation failure");
        // The new segment is a fresh genesis-anchored chain that keeps accepting receipts.
        w.record_allowed("inference", "m", "c").unwrap();
        drop(w);
        let n = verify_chain(&ev, &key.with_extension("pub")).unwrap();
        assert!(n >= 1, "the live segment must verify, got {n}");
    }

    /// CRITICAL guard: a non-ASCII signature must not abort the process.
    ///
    /// `hex_decode` used to byte-slice a `&str`, which panics when a multi-byte char straddles
    /// an even offset. ux.6a made that reachable from `open` via the tail signature check — the
    /// BOOT path, with `panic = "abort"` workspace-wide, where a panic bypasses every fallback.
    #[test]
    fn non_ascii_signature_does_not_panic_at_open() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 1);

        let prev = std::fs::read_to_string(&ev).unwrap();
        let bad = ActionReceipt {
            seq: 1,
            action: "inference".into(),
            target: "m".into(),
            principal: "a".into(),
            verdict: "allowed".into(),
            ts: "2026-07-29T00:00:00Z".into(),
            chain_prev_hash: line_hash(prev.lines().next().unwrap().as_bytes()),
            // 4 bytes, so it passes the even-length guard, and 'é' straddles offset 2.
            signature: "a\u{e9}a".to_string(),
        };
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(f, "{}", serde_json::to_string(&bad).unwrap()).unwrap();
        drop(f);

        // Must boot, and must report the bad signature rather than aborting.
        let w = EvidenceWriter::open(&ev, &key).expect("must not panic or refuse");
        assert!(w.resume_note().is_some(), "the unverifiable tail is reported");
        // And hex_decode itself now errors instead of panicking.
        assert!(hex_decode("a\u{e9}a").is_err());
    }

    /// CRITICAL guard: `seq` comes from an UNVERIFIED tail receipt, so an absurd value must not
    /// reach the writer. Unbounded, it either panics inside the mutex (debug) or wraps to 0 in
    /// release and makes every later genuine receipt fail `verify_chain` with a sequence gap.
    #[test]
    fn implausible_tail_seq_falls_back_instead_of_poisoning_the_chain() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 1);

        let prev = std::fs::read_to_string(&ev).unwrap();
        let absurd = ActionReceipt {
            seq: u64::MAX,
            action: "inference".into(),
            target: "m".into(),
            principal: "a".into(),
            verdict: "allowed".into(),
            ts: "2026-07-29T00:00:00Z".into(),
            chain_prev_hash: line_hash(prev.lines().next().unwrap().as_bytes()),
            signature: "00".repeat(64),
        };
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(f, "{}", serde_json::to_string(&absurd).unwrap()).unwrap();
        drop(f);

        let got = resume_chain(&ev, None).unwrap();
        assert!(
            got.seq < 1_000,
            "an implausible tail seq must not propagate (got {})",
            got.seq
        );
        assert!(got.note.is_some(), "the fallback must be reported");
        // The writer must remain usable — no overflow, no poisoned mutex.
        let w = EvidenceWriter::open(&ev, &key).unwrap();
        w.record_allowed("inference", "m", "after").unwrap();
        assert!(w.resume_note().is_some());
    }

    /// T-1: the `\r`-strip in `line_hash` is the byte-for-byte compatibility hinge with
    /// `str::lines()`, and it had NO coverage — removing it left all 24 tests green, because
    /// every other test writes through `EvidenceWriter`, which only ever emits `\n`. A CRLF
    /// `evidence.jsonl` (archived, copied through a Windows host, hand-edited) would have had
    /// its `chain_prev_hash` silently changed and broken at its next append.
    #[test]
    fn resume_matches_legacy_on_crlf_line_endings() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 3);
        let crlf = std::fs::read_to_string(&ev).unwrap().replace('\n', "\r\n");
        std::fs::write(&ev, crlf).unwrap();

        let (legacy_seq, legacy_hash) = legacy_resume(&ev);
        let got = resume_chain(&ev, None).unwrap();
        assert_eq!(got.seq, legacy_seq, "seq diverged on CRLF");
        assert_eq!(
            got.chain_prev_hash, legacy_hash,
            "chain hash diverged on CRLF — the \\r strip must match str::lines()"
        );
        // Essential: without this the assertion above is satisfiable by the FALLBACK, which
        // uses `str::lines()` and therefore matches legacy by construction. The bounded reader
        // must be the thing that agreed.
        assert!(
            got.note.is_none(),
            "the BOUNDED reader must handle CRLF — a fallback would match legacy trivially: {:?}",
            got.note
        );
    }

    /// T-5: the committed fixture only ever exercised `verify_chain`, which this increment did
    /// NOT touch. Drive the one artifact known to predate the rewrite through `resume_chain`,
    /// the function that WAS rewritten — that is the actual risk the fixture exists to cover.
    /// Copied first, because resume is allowed to mutate its input.
    #[test]
    fn golden_fixture_resumes_identically_under_the_bounded_reader() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let src = golden_fixture().join("evidence.jsonl");
        std::fs::copy(&src, &ev).unwrap();
        let pk = std::fs::read(golden_fixture().join("egress-key.pub")).unwrap();

        let (legacy_seq, legacy_hash) = legacy_resume(&ev);
        let got = resume_chain(&ev, Some(&pk)).unwrap();

        assert_eq!(got.seq, 3, "the fixture is a 3-receipt chain");
        assert_eq!(got.seq, legacy_seq);
        assert_eq!(got.chain_prev_hash, legacy_hash);
        assert!(got.note.is_none(), "a healthy pre-ux.6a chain resumes clean: {:?}", got.note);
        assert_eq!(
            std::fs::read(&ev).unwrap(),
            std::fs::read(&src).unwrap(),
            "resume must not modify a healthy chain"
        );
    }

    /// T-7: the window-growth CEILING, i.e. the loop's termination guard. The check sits BEFORE
    /// the doubling, so an off-by-one there is an infinite loop that hangs boot — on a path
    /// `main.rs` treats as fail-closed.
    #[test]
    fn resume_falls_back_when_the_tail_line_exceeds_the_window_ceiling() {
        let dir = TempDir::new().unwrap();
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        write_n_receipts(&ev, &key, 1);

        let first = std::fs::read_to_string(&ev).unwrap();
        let huge = ActionReceipt {
            seq: 1,
            action: "inference".into(),
            target: "m".into(),
            principal: "x".repeat((RESUME_TAIL_MAX as usize) + 4096),
            verdict: "allowed".into(),
            ts: "2026-07-29T00:00:00Z".into(),
            chain_prev_hash: line_hash(first.lines().next().unwrap().as_bytes()),
            signature: "00".repeat(64),
        };
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(f, "{}", serde_json::to_string(&huge).unwrap()).unwrap();
        drop(f);

        let (legacy_seq, legacy_hash) = legacy_resume(&ev);
        let got = resume_chain(&ev, None).unwrap();
        assert_eq!(got.seq, legacy_seq, "the fallback must reproduce the legacy answer");
        assert_eq!(got.chain_prev_hash, legacy_hash);
        assert!(
            got.note.unwrap().contains("fell back to the full scan"),
            "exceeding the ceiling must be reported, not silent"
        );
    }

    /// Path to the committed golden fixture (a 3-receipt chain written by the p7.5 format).
    fn golden_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/evidence")
    }

    /// ux.6a Step 0 — the backward-compatibility and canonicalization guard.
    ///
    /// The Ed25519 signature covers `serde_json::to_string(&ReceiptBody)`, and `verify_chain`
    /// reconstructs that struct field-by-field and re-serializes it (see `verify_chain`). So the
    /// signed bytes are whatever serde emits for `ReceiptBody`'s CURRENT shape and field ORDER —
    /// there is no `deny_unknown_fields` and no format version to warn anyone. Adding, removing,
    /// or reordering a field silently invalidates every receipt ever written.
    ///
    /// This fixture was generated by the pre-ux.6a binary and is verified with the SHIPPED
    /// verifier. If this test goes red, an on-disk format change has been introduced and every
    /// `evidence.jsonl` in the field just became unverifiable. Do not "fix" it by regenerating
    /// the fixture.
    #[test]
    fn evidence_golden_fixture_still_verifies() {
        let dir = golden_fixture();
        let n = verify_chain(&dir.join("evidence.jsonl"), &dir.join("egress-key.pub"))
            .expect("the committed p7.5-format chain must still verify");
        assert_eq!(n, 3, "fixture is a 3-receipt chain");
    }

    /// The fixture must never carry the private key — only the public half is committable.
    #[test]
    fn golden_fixture_ships_no_private_key() {
        for entry in std::fs::read_dir(golden_fixture()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.ends_with(".pkcs8"),
                "a private key leaked into the committed fixture: {name}"
            );
        }
    }

    /// Regenerates the golden fixture. `#[ignore]`d: it is documentation of how the fixture was
    /// made and an escape hatch, NOT part of the suite. Running it mints a NEW key and new
    /// signatures, which defeats the point of the guard above.
    ///
    /// `cargo test -p agentd --lib evidence::tests::regenerate_golden_fixture -- --ignored`
    #[test]
    #[ignore = "fixture generator: rewrites a committed test fixture"]
    fn regenerate_golden_fixture() {
        let tmp = TempDir::new().unwrap();
        let ev = tmp.path().join("evidence.jsonl");
        let key = tmp.path().join("egress-key.pkcs8");
        {
            let w = EvidenceWriter::open(&ev, &key).unwrap();
            w.record_allowed("inference", "claude-sonnet-4-6", "agent_0")
                .unwrap();
            w.record_allowed("inference", "claude-sonnet-4-6", "agent_1")
                .unwrap();
            w.record_denied("egress", "https://blocked.example.com", "agent_0")
                .unwrap();
        }
        let out = golden_fixture();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(&ev, out.join("evidence.jsonl")).unwrap();
        // Public half ONLY — the pkcs8 stays in the temp dir and dies with it.
        std::fs::copy(key.with_extension("pub"), out.join("egress-key.pub")).unwrap();
        assert_eq!(
            verify_chain(&out.join("evidence.jsonl"), &out.join("egress-key.pub")).unwrap(),
            3
        );
    }
}
