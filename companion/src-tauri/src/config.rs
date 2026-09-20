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

        Ok(AppConfig {
            token,
            config_dir,
            token_path,
            http_port: 7777,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
