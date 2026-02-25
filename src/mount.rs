//! Unified mount parsing for `ww run` positional arguments.
//!
//! Every positional arg is `source[:target]`:
//! - `images/my-app`           → mount at `/` (image layer)
//! - `~/.ww/identity:/etc/identity` → mount file at `/etc/identity`
//! - `/ipfs/QmHash:/boot`      → mount IPFS content at `/boot`
//!
//! The split happens on the first `:` — if the right side starts with `/`,
//! it's a targeted mount. Otherwise the whole string is a source mounted
//! at `/`.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use anyhow::{bail, Result};

/// A mount: bind a source (host path or IPFS path) into the guest FHS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Host path or IPFS path (raw string, resolved later).
    pub source: String,
    /// Absolute path in the guest FHS. Defaults to `/`.
    pub target: PathBuf,
}

impl Mount {
    /// Returns `true` if this mount targets the FHS root.
    pub fn is_root(&self) -> bool {
        self.target == std::path::Path::new("/")
    }
}

/// Parse positional args into mounts.
///
/// Each arg is split on the first `:`. If the right side starts with `/`,
/// it's a targeted mount (`source:target`). Otherwise the whole string
/// is treated as a source mounted at `/`.
pub fn parse_args(args: &[String]) -> Result<Vec<Mount>> {
    args.iter().map(|arg| parse_one(arg)).collect()
}

fn parse_one(arg: &str) -> Result<Mount> {
    if arg.is_empty() {
        bail!("empty mount argument");
    }

    // Split on the first ':'.
    if let Some(colon_pos) = arg.find(':') {
        let (left, right) = arg.split_at(colon_pos);
        let right = &right[1..]; // skip the ':'

        // If the right side starts with '/', it's a targeted mount.
        if right.starts_with('/') {
            if left.is_empty() {
                bail!("mount has empty source: {arg}");
            }
            return Ok(Mount {
                source: left.to_string(),
                target: PathBuf::from(right),
            });
        }
    }

    // No colon, or right side doesn't start with '/' → root mount.
    Ok(Mount {
        source: arg.to_string(),
        target: PathBuf::from("/"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_mount_plain_path() {
        let m = parse_one("images/my-app").unwrap();
        assert_eq!(m.source, "images/my-app");
        assert_eq!(m.target, PathBuf::from("/"));
        assert!(m.is_root());
    }

    #[test]
    fn root_mount_dot() {
        let m = parse_one(".").unwrap();
        assert_eq!(m.source, ".");
        assert!(m.is_root());
    }

    #[test]
    fn root_mount_ipfs_path() {
        let m = parse_one("/ipfs/QmFoo").unwrap();
        assert_eq!(m.source, "/ipfs/QmFoo");
        assert!(m.is_root());
    }

    #[test]
    fn targeted_mount_file() {
        let m = parse_one("~/.ww/identity:/etc/identity").unwrap();
        assert_eq!(m.source, "~/.ww/identity");
        assert_eq!(m.target, PathBuf::from("/etc/identity"));
        assert!(!m.is_root());
    }

    #[test]
    fn targeted_mount_directory() {
        let m = parse_one("~/data:/var/data").unwrap();
        assert_eq!(m.source, "~/data");
        assert_eq!(m.target, PathBuf::from("/var/data"));
    }

    #[test]
    fn targeted_mount_ipfs_source() {
        let m = parse_one("/ipfs/QmHash:/boot").unwrap();
        assert_eq!(m.source, "/ipfs/QmHash");
        assert_eq!(m.target, PathBuf::from("/boot"));
    }

    #[test]
    fn colon_without_slash_is_root_mount() {
        // "foo:bar" where bar doesn't start with '/' → treat whole thing as source
        let m = parse_one("foo:bar").unwrap();
        assert_eq!(m.source, "foo:bar");
        assert!(m.is_root());
    }

    #[test]
    fn parse_args_mixed() {
        let args: Vec<String> = vec![
            "images/my-app".into(),
            "~/.ww/identity:/etc/identity".into(),
            "/ipfs/QmHash:/boot".into(),
        ];
        let mounts = parse_args(&args).unwrap();
        assert_eq!(mounts.len(), 3);
        assert!(mounts[0].is_root());
        assert_eq!(mounts[1].target, PathBuf::from("/etc/identity"));
        assert_eq!(mounts[2].target, PathBuf::from("/boot"));
    }

    #[test]
    fn empty_arg_errors() {
        assert!(parse_one("").is_err());
    }

    #[test]
    fn empty_source_errors() {
        assert!(parse_one(":/etc/identity").is_err());
    }
}
