// Shared auth helpers: secrets file path + atomic 0600 write.

use anyhow::{Context, Result};
use std::{fs, io::Write, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::{Path, PathBuf}};

pub fn secrets_file_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".agentos-secrets")
        .join("google.json"))
}

/// Write `client_id`/`client_secret`/`refresh_token` as JSON to `path` with mode 0600.
/// Atomic: writes to a temp file then renames.
pub fn write_secrets_file(
    path: &Path,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<()> {
    let json = serde_json::json!({
        "client_id":     client_id,
        "client_secret": client_secret,
        "refresh_token": refresh_token,
    });
    let content = serde_json::to_string_pretty(&json).unwrap();

    let dir = path.parent().context("Invalid secrets file path")?;
    if !dir.exists() {
        eprintln!("Creating {} ...", dir.display());
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("Failed to set permissions on {}", dir.display()))?;
    }
    let tmp_path = dir.join(format!(".google.json.tmp.{}", std::process::id()));

    // Create at mode 0600 from the first syscall — no world-readable race window.
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| format!("Open failed: {}", tmp_path.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("Write failed: {}", tmp_path.display()))?;
    }
    fs::rename(&tmp_path, path)
        .with_context(|| format!("Rename failed: {} -> {}", tmp_path.display(), path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn write_secrets_file_creates_and_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets").join("google.json");
        write_secrets_file(&path, "cid", "csec", "rtoken").unwrap();

        assert!(path.exists());
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "file mode must be 0600");

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["client_id"], "cid");
        assert_eq!(content["refresh_token"], "rtoken");
    }

    #[test]
    fn write_secrets_file_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("google.json");
        write_secrets_file(&path, "c1", "s1", "r1").unwrap();
        write_secrets_file(&path, "c2", "s2", "r2").unwrap();
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["client_id"], "c2");
        assert_eq!(content["refresh_token"], "r2");
    }
}
