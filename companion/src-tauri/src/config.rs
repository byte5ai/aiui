use crate::fsutil::atomic_write_with_mode;
use rand::RngCore;
use std::fs;
use std::io;
use std::path::PathBuf;

pub struct AppConfig {
    pub token: String,
    pub config_dir: PathBuf,
    pub token_path: PathBuf,
    pub http_port: u16,
}

/// Returns the OS-appropriate aiui config directory.
///
/// - macOS / Linux: `~/.config/aiui` (XDG-style — kept on macOS deliberately
///   so existing v0.4.x installs keep their token without migration).
/// - Windows: `%APPDATA%\aiui` (Roaming) — resolved via `dirs::config_dir()`.
pub fn config_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = dirs::config_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no %APPDATA%"))?;
        Ok(base.join("aiui"))
    }
    #[cfg(not(windows))]
    {
        let home = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home dir"))?;
        Ok(home.join(".config").join("aiui"))
    }
}

/// A well-formed aiui API token: exactly 64 lowercase hex chars (32 random
/// bytes). Anything else — empty, whitespace, truncated by a failed copy,
/// mangled by an editor — is treated as "no token" and regenerated, because
/// an empty or partial token silently weakens every `/render` auth check.
fn is_well_formed_token(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

impl AppConfig {
    pub fn load_or_init() -> io::Result<Self> {
        let config_dir = config_dir()?;
        // Issue #185: create the directory itself 0700 on Unix, so neither
        // the token nor the GUI lock is exposed by a 0755 parent.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            if !config_dir.exists() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&config_dir)?;
            } else {
                // Tighten an existing 0755 dir from an older install.
                use std::os::unix::fs::PermissionsExt;
                if let Ok(md) = fs::metadata(&config_dir) {
                    if md.permissions().mode() & 0o077 != 0 {
                        let _ = fs::set_permissions(
                            &config_dir,
                            fs::Permissions::from_mode(0o700),
                        );
                    }
                }
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&config_dir)?;

        let token_path = config_dir.join("token");
        let existing = fs::read_to_string(&token_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| {
                if is_well_formed_token(s) {
                    true
                } else {
                    crate::logging::trace(&format!(
                        "[aiui] token at {} is not 64 hex chars ({} bytes) — regenerating",
                        token_path.display(),
                        s.len()
                    ));
                    false
                }
            });

        let token = match existing {
            Some(t) => {
                // Re-assert 0600 on every launch: a token restored from a
                // backup, or written by an older build that chmod'ed after
                // the rename, would otherwise stay world-readable forever.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(md) = fs::metadata(&token_path) {
                        if md.permissions().mode() & 0o177 != 0 {
                            let _ = fs::set_permissions(
                                &token_path,
                                fs::Permissions::from_mode(0o600),
                            );
                        }
                    }
                }
                t
            }
            None => {
                let mut bytes = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut bytes);
                let t = hex::encode(bytes);
                // 0600 is applied to the temp handle *before* the rename, so
                // the token is never briefly world-readable under its final
                // name — the window the old chmod-after-rename left open.
                atomic_write_with_mode(&token_path, t.as_bytes(), Some(0o600))?;
                t
            }
        };

        let cfg = AppConfig {
            token,
            config_dir,
            token_path,
            http_port: 7777,
        };
        // Review C-01: best-effort — a missing proof never lets a remote gain
        // a local write; `/render` then refuses target-bearing specs with an
        // actionable error. Trace the cause so that error can be explained.
        if let Err(e) = cfg.ensure_local_proof() {
            crate::logging::trace(&format!(
                "[aiui] could not create the locality proof at {}: {e}",
                cfg.local_proof_path().display()
            ));
        }
        Ok(cfg)
    }

    /// Where the locality proof lives: next to the token, but — unlike the
    /// token — never copied to a remote (`setup::push_token_to_remote` copies
    /// `token` by name).
    pub fn local_proof_path(&self) -> PathBuf {
        self.config_dir.join("local-proof")
    }

    /// The locality proof, if one exists and is well-formed. Read fresh on
    /// every use rather than cached: the GUI and its `--mcp-stdio` children
    /// are separate processes, and the file is the one value they share.
    pub fn read_local_proof(&self) -> Option<String> {
        fs::read_to_string(self.local_proof_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| is_well_formed_token(s))
    }

    /// Create the locality proof (0600) if it does not exist yet.
    ///
    /// Review C-01: every caller of `127.0.0.1:7777` — the local bridge and
    /// every tunnelled remote — presents the same bearer token, so the token
    /// cannot say who is local. The companion used to infer "local writer"
    /// from an ABSENT `session_origin` field, which any remote could simply
    /// omit to have its `target` write performed on this machine. The local
    /// Rust bridge now presents this file's content as `x-aiui-local-proof`;
    /// a render without it is treated as bridge-served.
    pub fn ensure_local_proof(&self) -> io::Result<String> {
        if let Some(p) = self.read_local_proof() {
            return Ok(p);
        }
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let p = hex::encode(bytes);
        atomic_write_with_mode(&self.local_proof_path(), p.as_bytes(), Some(0o600))?;
        // Two processes racing on first start each write a value; the file is
        // the authority, so return what it holds now.
        Ok(self.read_local_proof().unwrap_or(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_proof_is_created_once_and_is_not_the_token() {
        let dir = std::env::temp_dir().join(format!("aiui-proof-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = AppConfig {
            token: "t".repeat(64),
            config_dir: dir.clone(),
            token_path: dir.join("token"),
            http_port: 7777,
        };
        assert_eq!(cfg.read_local_proof(), None);
        let a = cfg.ensure_local_proof().unwrap();
        let b = cfg.ensure_local_proof().unwrap();
        assert_eq!(a, b, "created once, then reused");
        assert!(is_well_formed_token(&a));
        assert_ne!(a, cfg.token);
        assert_ne!(cfg.local_proof_path().file_name().unwrap(), "token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(cfg.local_proof_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn token_shape_is_validated() {
        assert!(is_well_formed_token(&"a".repeat(64)));
        assert!(is_well_formed_token(&"0123456789abcdef".repeat(4)));
        assert!(!is_well_formed_token(""));
        assert!(!is_well_formed_token("   "));
        assert!(!is_well_formed_token(&"a".repeat(63)), "truncated");
        assert!(!is_well_formed_token(&"a".repeat(65)), "too long");
        assert!(!is_well_formed_token(&"z".repeat(64)), "non-hex");
    }
}
