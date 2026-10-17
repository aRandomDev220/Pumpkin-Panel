use std::{net::SocketAddr, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // metrics/session fields wired in Phase 6/2
pub struct Settings {
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_backup_dir")]
    pub backup_dir: PathBuf,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_metrics_interval")]
    pub metrics_interval_seconds: u64,
    #[serde(default = "default_session_ttl")]
    pub session_ttl_minutes: u64,
    #[serde(default = "default_db_file")]
    pub database_url: String,
}

fn default_http_port() -> u16 {
    8080
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("/data/servers")
}
fn default_backup_dir() -> PathBuf {
    PathBuf::from("/data/backups")
}
fn default_log_level() -> String {
    "info".into()
}
fn default_metrics_interval() -> u64 {
    30
}
fn default_session_ttl() -> u64 {
    720
}
fn default_db_file() -> String {
    "sqlite:/data/pumpkin-manager.db?mode=rwc".into()
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let builder = config::Config::builder()
            .set_default("http_port", 8080)?
            .set_default("data_dir", "/data/servers")?
            .set_default("backup_dir", "/data/backups")?
            .set_default("log_level", "info")?
            .set_default("metrics_interval_seconds", 30)?
            .set_default("session_ttl_minutes", 720)?
            .set_default("database_url", "sqlite:/data/pumpkin-manager.db?mode=rwc")?
            .add_source(config::Environment::with_prefix("PUMPKIN_MANAGER").try_parsing(true))
            .build()?;
        let mut settings: Self = builder.try_deserialize()?;
        settings.data_dir = resolve_path(settings.data_dir)?;
        settings.backup_dir = resolve_path(settings.backup_dir)?;
        Ok(settings)
    }

    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], self.http_port))
    }

    /// Session lifetime in seconds (cookie `Max-Age` + session expiry).
    /// Floors at one minute so a misconfigured `0` cannot create
    /// immediately-dead sessions.
    pub fn session_ttl_secs(&self) -> i64 {
        (self.session_ttl_minutes.max(1) as i64) * 60
    }
}

fn resolve_path(path: PathBuf) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()?.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_are_sane() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("PUMPKIN_MANAGER_HTTP_PORT");
        std::env::remove_var("PUMPKIN_MANAGER_DATABASE_URL");
        let s = Settings::load().expect("settings load");
        assert_eq!(s.http_port, 8080);
        assert!(s.database_url.starts_with("sqlite:"));
    }

    #[test]
    fn env_prefix_parses_port() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("PUMPKIN_MANAGER_HTTP_PORT", "9090");
        let s = Settings::load().expect("settings load");
        std::env::remove_var("PUMPKIN_MANAGER_HTTP_PORT");
        assert_eq!(s.http_port, 9090);
    }

    #[test]
    fn relative_storage_paths_resolve_from_working_directory() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("PUMPKIN_MANAGER_DATA_DIR", "./dev-data/servers");
        std::env::set_var("PUMPKIN_MANAGER_BACKUP_DIR", "./dev-data/backups");
        let s = Settings::load().expect("settings load");
        std::env::remove_var("PUMPKIN_MANAGER_DATA_DIR");
        std::env::remove_var("PUMPKIN_MANAGER_BACKUP_DIR");
        assert!(s.data_dir.is_absolute());
        assert!(s.backup_dir.is_absolute());
        assert!(s.data_dir.ends_with("dev-data/servers"));
        assert!(s.backup_dir.ends_with("dev-data/backups"));
    }
}
