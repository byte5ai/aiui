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

impl AppConfig {
    pub fn load_or_init() -> io::Result<Self> {
        let config_dir = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home dir"))?
            .join(".config")
            .join("aiui");
        fs::create_dir_all(&config_dir)?;

        let token_path = config_dir.join("token");
        let token = if token_path.exists() {
            fs::read_to_string(&token_path)?.trim().to_string()
        } else {
            let mut bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            let t = hex::encode(bytes);
            fs::write(&token_path, &t)?;
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
