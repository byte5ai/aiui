use crate::fsutil::atomic_write;
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

impl AppConfig {
    pub fn load_or_init() -> io::Result<Self> {
        let config_dir = config_dir()?;
        fs::create_dir_all(&config_dir)?;

        let token_path = config_dir.join("token");
        let token = if token_path.exists() {
            fs::read_to_string(&token_path)?.trim().to_string()
        } else {
            let mut bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            let t = hex::encode(bytes);
            atomic_write(&token_path, t.as_bytes())?;
            // chmod 600
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&token_path)?.permissions();
                perms.set_mode(0o600);
                fs::set_permissions(&token_path, perms)?;
            }
            t
        };

        Ok(AppConfig {
            token,
            config_dir,
            token_path,
            http_port: 7777,
        })
    }
}
