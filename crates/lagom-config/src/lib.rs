//! # lagom-config
//!
//! The `lagom.toml` face: load the flat human-authored TOML surface into a
//! [`lagom_core::Policy`], discover config files by precedence, and compose an
//! integrator base with an end-user overlay via the narrow-only
//! [`lagom_core::merge`].
//!
//! This crate carries *only* the flat human surface (which tools, which args
//! pinned/constrained, description overrides) per `SPEC.md` §6.2; rich
//! transforms remain the builder's job. The CLI face and end-user narrowing
//! both go through here.
//!
//! These are compiling seams: the public signatures are final, the bodies are
//! `todo!()` until the loader/discovery/builder slices land.

use std::path::{Path, PathBuf};

use lagom_core::{Policy, merge};
use thiserror::Error;

mod discover;
mod schema;
mod toml_model;

pub use discover::{discover, profiles, resolve_profile};
pub use schema::json_schema;
pub use toml_model::{LowerError, TomlArgPolicy, TomlPolicy, TomlToolPolicy};

/// Anything that can go wrong turning config on disk into a [`Policy`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config `{path}`: {source}")]
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file was not valid TOML, or did not match the `lagom.toml` schema.
    #[error("parsing config `{path}`: {source}")]
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },
    /// The TOML parsed but described a policy lagom cannot represent — e.g. an
    /// unknown `presence` value, or more than one mutually-exclusive arg
    /// transform on a single argument.
    #[error("invalid policy in `{path}`: {message}")]
    Lower {
        /// The path whose contents were malformed.
        path: PathBuf,
        /// A human-readable description of the problem.
        message: String,
    },
    /// Composing an overlay onto a base attempted to *widen* the sealed bounds
    /// (`SPEC.md` §5.2). Wraps the core [`lagom_core::MergeError`].
    #[error("overlay widens sealed bounds: {0}")]
    Merge(#[from] lagom_core::MergeError),
}

/// Load a single `lagom.toml` from `path` into a [`Policy`].
///
/// Reads the file, deserializes it into [`TomlPolicy`], then lowers it into the
/// canonical core [`Policy`]. No layering is performed here — see [`load_layered`]
/// and [`load_discovered`] for base + overlay composition.
pub fn load(path: impl AsRef<Path>) -> Result<Policy, ConfigError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let toml_policy: TomlPolicy = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    toml_policy
        .into_policy()
        .map_err(|LowerError(message)| ConfigError::Lower {
            path: path.to_path_buf(),
            message,
        })
}

/// Load and compose a layered config: an integrator `base` narrowed by an
/// end-user `overlay`, via the narrow-only [`merge`].
///
/// The overlay may only narrow the base (`SPEC.md` §5.2); any widening surfaces
/// as [`ConfigError::Merge`].
pub fn load_layered(
    base: impl AsRef<Path>,
    overlay: impl AsRef<Path>,
) -> Result<Policy, ConfigError> {
    let base = load(base)?;
    let overlay = load(overlay)?;
    Ok(merge(&base, &overlay)?)
}

/// Load every discovered `lagom.toml` and compose them by precedence
/// (`SPEC.md` §6.3).
///
/// Discovery order (lowest precedence first, after reversing [`discover`]'s
/// highest-first list) is treated as base → overlay → overlay…: the
/// lowest-precedence config is the integrator base (the sealed ceiling) and each
/// higher-precedence config narrows it via the narrow-only [`merge`]. A widening
/// at any layer surfaces as [`ConfigError::Merge`].
///
/// `explicit` is the optional `--config` path (highest precedence). Returns a
/// passthrough [`Policy`] when nothing is discovered.
pub fn load_discovered(
    start: impl AsRef<Path>,
    explicit: Option<&Path>,
) -> Result<Policy, ConfigError> {
    let mut paths = discover(start);
    if let Some(p) = explicit {
        // Explicit `--config` is highest precedence: prepend, de-duplicated.
        paths.retain(|existing| existing != p);
        paths.insert(0, p.to_path_buf());
    }

    if paths.is_empty() {
        return Ok(Policy::passthrough());
    }

    // `discover` returns highest-precedence first; the base (sealed ceiling) is
    // the *lowest*-precedence layer, so fold from the back.
    let mut iter = paths.iter().rev();
    let base_path = iter.next().expect("non-empty checked above");
    let mut policy = load(base_path)?;
    for overlay_path in iter {
        let overlay = load(overlay_path)?;
        policy = merge(&policy, &overlay)?;
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lagom_core::Presence;
    use std::fs;

    /// Throwaway temp dir, cleaned on drop (see `discover::tests` for rationale).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "lagom-config-lib-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, name: &str, body: &str) -> PathBuf {
            let p = self.path.join(name);
            fs::write(&p, body).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn load_reads_and_lowers() {
        let tmp = TempDir::new("load");
        let p = tmp.write("lagom.toml", "default-presence = \"drop\"\n");
        let policy = load(&p).unwrap();
        assert_eq!(policy.default_presence, Presence::Drop);
    }

    #[test]
    fn load_missing_file_is_io_error() {
        let err = load("/no/such/lagom.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "{err:?}");
    }

    #[test]
    fn load_bad_toml_is_parse_error() {
        let tmp = TempDir::new("badtoml");
        let p = tmp.write("lagom.toml", "not = = valid");
        let err = load(&p).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn load_malformed_policy_is_lower_error_with_path() {
        let tmp = TempDir::new("lower");
        let p = tmp.write("lagom.toml", "default-presence = \"allow\"\n");
        let err = load(&p).unwrap_err();
        match err {
            ConfigError::Lower { path, message } => {
                assert_eq!(path, p);
                assert!(message.contains("unknown presence"), "{message}");
            }
            other => panic!("expected Lower, got {other:?}"),
        }
    }

    #[test]
    fn load_layered_narrows() {
        // Base keeps `search` open; overlay pins an arg — a legal narrowing.
        let tmp = TempDir::new("layered-ok");
        let base = tmp.write("base.toml", "[tools.search]\npresence = \"keep\"\n");
        let overlay = tmp.write(
            "overlay.toml",
            "[tools.search.args.artifact]\npin = \"hylla\"\n",
        );
        let policy = load_layered(&base, &overlay).unwrap();
        assert!(policy.tools["search"].args.contains_key("artifact"));
    }

    #[test]
    fn load_layered_rejects_widening_overlay() {
        // Base drops `search` (sealed); overlay tries to re-keep it — a widening,
        // which must be rejected (`SPEC.md` §5.2).
        let tmp = TempDir::new("layered-widen");
        let base = tmp.write("base.toml", "[tools.search]\npresence = \"drop\"\n");
        let overlay = tmp.write("overlay.toml", "[tools.search]\npresence = \"keep\"\n");
        let err = load_layered(&base, &overlay).unwrap_err();
        assert!(matches!(err, ConfigError::Merge(_)), "{err:?}");
    }

    #[test]
    fn load_discovered_composes_root_then_cwd() {
        // Project root (lower precedence) is the base; cwd (higher precedence) is
        // the overlay. Root keeps the tool; cwd narrows by dropping it.
        let tmp = TempDir::new("disc");
        fs::create_dir_all(tmp.path.join(".git")).unwrap();
        tmp.write("lagom.toml", "[tools.search]\npresence = \"keep\"\n");
        let nested = tmp.path.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("lagom.toml"),
            "[tools.search]\npresence = \"drop\"\n",
        )
        .unwrap();

        let policy = load_discovered(&nested, None).unwrap();
        assert_eq!(
            policy.tools["search"].presence,
            Some(Presence::Drop),
            "higher-precedence cwd overlay narrows the root base"
        );
    }

    #[test]
    fn load_discovered_empty_is_passthrough() {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = TempDir::new("disc-empty");
        // Isolate from the developer's real user config by pointing XDG/HOME at
        // an empty temp dir, so discovery genuinely finds nothing.
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: serialized by LOCK for the duration of this test.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp.path);
            std::env::set_var("HOME", &tmp.path);
        }

        let policy = load_discovered(&tmp.path, None).unwrap();
        assert_eq!(policy, Policy::passthrough());

        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn load_discovered_explicit_overlays_highest() {
        // Explicit --config is the highest-precedence overlay. Root base keeps;
        // explicit narrows by dropping.
        let tmp = TempDir::new("disc-explicit");
        fs::create_dir_all(tmp.path.join(".git")).unwrap();
        tmp.write("lagom.toml", "[tools.search]\npresence = \"keep\"\n");
        let explicit = tmp.write("explicit.toml", "[tools.search]\npresence = \"drop\"\n");

        let policy = load_discovered(&tmp.path, Some(&explicit)).unwrap();
        assert_eq!(policy.tools["search"].presence, Some(Presence::Drop));
    }
}
