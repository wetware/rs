//! Daemon configuration loader.
//!
//! Reads a Glia data literal from `~/.ww/config.glia` and extracts
//! the fields needed by `ww daemon install` / the running daemon.
//!
//! Example config:
//! ```glia
//! {:port     2025
//!  :identity "~/.ww/key"
//!  :images   ["images/my-app" "images/shell"]}
//! ```
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use glia::Val;

/// Daemon configuration extracted from a `.glia` file.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub port: u16,
    pub identity: Option<PathBuf>,
    pub images: Vec<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            port: 2025,
            identity: None,
            images: Vec::new(),
        }
    }
}

impl DaemonConfig {
    /// Serialize this config as a Glia data literal.
    pub fn to_glia(&self) -> String {
        let mut parts = vec![format!(":port {}", self.port)];

        if let Some(ref identity) = self.identity {
            parts.push(format!(":identity \"{}\"", identity.display()));
        }

        if !self.images.is_empty() {
            let imgs: Vec<String> = self
                .images
                .iter()
                .map(|p| format!("\"{}\"", p.display()))
                .collect();
            parts.push(format!(":images [{}]", imgs.join(" ")));
        }

        format!("{{{}}}", parts.join(" "))
    }

    /// Write this config to a `.glia` file, creating parent directories as needed.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config directory: {}", parent.display()))?;
        }
        std::fs::write(path, self.to_glia())
            .with_context(|| format!("write config: {}", path.display()))
    }
}

/// Default config file path: `~/.ww/config.glia`.
pub fn default_config_path() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".ww/config.glia")
}

/// Load daemon config from a `.glia` file.
///
/// If the file doesn't exist, returns the default config. Missing keys
/// use their defaults; unknown keys are silently ignored.
pub fn load(path: &std::path::Path) -> Result<DaemonConfig> {
    if !path.exists() {
        return Ok(DaemonConfig::default());
    }

    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read config: {}", path.display()))?;

    let val = glia::read(&contents)
        .map_err(|e| anyhow::anyhow!("parse config {}: {e}", path.display()))?;

    from_val(&val)
}

/// Extract a [`DaemonConfig`] from a top-level Glia map.
fn from_val(val: &Val) -> Result<DaemonConfig> {
    let pairs = match val {
        Val::Map(pairs) => pairs,
        other => bail!("config must be a map, got: {other}"),
    };

    let mut config = DaemonConfig::default();

    for (key, value) in pairs {
        let kw = match key {
            Val::Keyword(k) => k.as_str(),
            _ => continue, // skip non-keyword keys
        };

        match kw {
            "port" => {
                config.port = match value {
                    Val::Int(n) => u16::try_from(*n).context(":port must be 0..65535")?,
                    other => bail!(":port must be an integer, got: {other}"),
                };
            }
            "identity" => {
                config.identity = match value {
                    Val::Str(s) => Some(expand_tilde(s)),
                    other => bail!(":identity must be a string, got: {other}"),
                };
            }
            "images" => {
                config.images = match value {
                    Val::Vector(items) => items
                        .iter()
                        .map(|v| match v {
                            Val::Str(s) => Ok(PathBuf::from(s)),
                            other => bail!(":images elements must be strings, got: {other}"),
                        })
                        .collect::<Result<Vec<_>>>()?,
                    other => bail!(":images must be a vector, got: {other}"),
                };
            }
            _ => {} // ignore unknown keys
        }
    }

    Ok(config)
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .expect("cannot determine home directory")
            .join(rest)
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let input = r#"{:port 2025 :identity "~/.ww/key" :images ["img/a" "img/b"]}"#;
        let val = glia::read(input).unwrap();
        let config = from_val(&val).unwrap();
        assert_eq!(config.port, 2025);
        assert!(config.identity.is_some());
        assert_eq!(config.images.len(), 2);
    }

    #[test]
    fn parse_minimal_config() {
        let input = "{}";
        let val = glia::read(input).unwrap();
        let config = from_val(&val).unwrap();
        assert_eq!(config.port, 2025);
        assert!(config.identity.is_none());
        assert!(config.images.is_empty());
    }

    #[test]
    fn parse_partial_config() {
        let input = "{:port 3000}";
        let val = glia::read(input).unwrap();
        let config = from_val(&val).unwrap();
        assert_eq!(config.port, 3000);
        assert!(config.identity.is_none());
    }

    #[test]
    fn reject_non_map() {
        let input = "[1 2 3]";
        let val = glia::read(input).unwrap();
        assert!(from_val(&val).is_err());
    }

    #[test]
    fn reject_bad_port() {
        let input = r#"{:port "not-a-number"}"#;
        let val = glia::read(input).unwrap();
        assert!(from_val(&val).is_err());
    }

    #[test]
    fn reject_port_out_of_range() {
        let input = "{:port 99999}";
        let val = glia::read(input).unwrap();
        assert!(from_val(&val).is_err());
    }

    #[test]
    fn unknown_keys_ignored() {
        let input = "{:port 2025 :unknown-key true}";
        let val = glia::read(input).unwrap();
        let config = from_val(&val).unwrap();
        assert_eq!(config.port, 2025);
    }

    #[test]
    fn tilde_expansion() {
        let expanded = expand_tilde("~/foo/bar");
        assert!(expanded.to_str().unwrap().contains("foo/bar"));
        assert!(!expanded.to_str().unwrap().starts_with("~"));
    }

    #[test]
    fn no_tilde_passthrough() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn to_glia_minimal() {
        let config = DaemonConfig::default();
        let s = config.to_glia();
        assert_eq!(s, "{:port 2025}");
    }

    #[test]
    fn to_glia_full() {
        let config = DaemonConfig {
            port: 9000,
            identity: Some(PathBuf::from("/keys/my.key")),
            images: vec![PathBuf::from("img/a"), PathBuf::from("img/b")],
        };
        let s = config.to_glia();
        assert_eq!(
            s,
            r#"{:port 9000 :identity "/keys/my.key" :images ["img/a" "img/b"]}"#
        );
    }

    #[test]
    fn roundtrip_write_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.glia");
        let original = DaemonConfig {
            port: 3000,
            identity: Some(PathBuf::from("/tmp/key")),
            images: vec![PathBuf::from("images/app")],
        };
        original.write(&path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.port, 3000);
        assert_eq!(loaded.identity, Some(PathBuf::from("/tmp/key")));
        assert_eq!(loaded.images, vec![PathBuf::from("images/app")]);
    }
}
