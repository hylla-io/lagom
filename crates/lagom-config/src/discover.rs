//! Layered config discovery by precedence (`SPEC.md` §6.3) and multi-profile
//! resolution (`SPEC.md` §6.4).
//!
//! Discovery precedence, highest first: explicit `--config` → `./lagom.toml` →
//! project root `lagom.toml` → `$XDG_CONFIG_HOME/lagom/lagom.toml`. The explicit
//! path is supplied by the caller ([`super::load_discovered`]); this module
//! discovers the rest. The composed result is fed through the narrow-only merge.
//!
//! Multi-profile: a single `lagom.toml` is the default; a `.lagom/` directory may
//! hold named profiles (`.lagom/<name>.toml`) for apps that ship many.

use std::path::{Path, PathBuf};

/// The branded config filename.
const CONFIG_FILENAME: &str = "lagom.toml";

/// The directory holding named profiles (`SPEC.md` §6.4).
const PROFILE_DIR: &str = ".lagom";

/// Discover candidate `lagom.toml` paths starting from `start`, ordered by
/// precedence (highest first).
///
/// The layers, highest precedence first:
///
/// 1. `<start>/lagom.toml` (the working-directory config);
/// 2. `<project-root>/lagom.toml`, where the project root is the nearest ancestor
///    of `start` containing a `.git` entry (skipped if it coincides with layer 1);
/// 3. `$XDG_CONFIG_HOME/lagom/lagom.toml` (the user config), falling back to
///    `$HOME/.config/lagom/lagom.toml` when `XDG_CONFIG_HOME` is unset.
///
/// Only paths that exist on disk are returned, de-duplicated while preserving
/// order. An empty vec means no config was found — [`search_paths`] then says
/// *where* lagom looked. The explicit `--config` path is *not* handled here — the
/// caller prepends it (see [`super::load_discovered`]).
pub fn discover(start: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut found = search_paths(start);
    found.retain(|path| path.exists());
    found
}

/// The candidate paths [`discover`] inspects, highest precedence first —
/// including ones that do **not** exist.
///
/// Exists because an empty [`discover`] result is silent about *where* lagom
/// looked, so "no config here on purpose" and "my config never landed on a
/// searched path" are indistinguishable — the misdiagnosis this list lets a
/// caller rule out when it reports an unconfigured (passthrough) resolution.
///
/// Same layer order and de-duplication as [`discover`], minus the existence
/// filter, so the two can never disagree about what was searched. Layer 2 is
/// absent when no ancestor carries `.git`, layer 3 when neither
/// `XDG_CONFIG_HOME` nor `HOME` is set.
pub fn search_paths(start: impl AsRef<Path>) -> Vec<PathBuf> {
    let start = start.as_ref();
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Layer 1: working-directory config.
    candidates.push(start.join(CONFIG_FILENAME));

    // Layer 2: project-root config (nearest ancestor with a `.git`).
    if let Some(root) = project_root(start) {
        candidates.push(root.join(CONFIG_FILENAME));
    }

    // Layer 3: user config under XDG.
    if let Some(dir) = xdg_config_dir() {
        candidates.push(dir.join(CONFIG_FILENAME));
    }

    dedup(candidates)
}

/// List the named profiles available under `<dir>/.lagom/` (`SPEC.md` §6.4).
///
/// Returns the profile names (the `*.toml` stems, sorted) found in the `.lagom/`
/// directory beside `dir`. An absent or empty `.lagom/` directory yields an empty
/// vec. Non-`.toml` entries are ignored.
pub fn profiles(dir: impl AsRef<Path>) -> Vec<String> {
    let profile_dir = dir.as_ref().join(PROFILE_DIR);
    let Ok(entries) = std::fs::read_dir(&profile_dir) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                return None;
            }
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    names.sort();
    names
}

/// Resolve a named profile to its `lagom.toml` path under `<dir>/.lagom/`.
///
/// `<dir>/.lagom/<name>.toml`. Returns `None` if the file does not exist; the
/// caller decides whether that is an error.
pub fn resolve_profile(dir: impl AsRef<Path>, name: &str) -> Option<PathBuf> {
    let path = dir.as_ref().join(PROFILE_DIR).join(format!("{name}.toml"));
    path.exists().then_some(path)
}

/// Find the nearest ancestor of `start` (inclusive) that contains a `.git`
/// entry, treated as the project root.
fn project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// The user config directory: `$XDG_CONFIG_HOME/lagom`, or `$HOME/.config/lagom`.
fn xdg_config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("lagom"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("lagom"))
}

/// Drop repeated paths (cwd may *be* the project root) while preserving order,
/// so a reported search list never claims lagom looked at one path twice.
fn dedup(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for path in candidates {
        if !seen.contains(&path) {
            seen.push(path);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A throwaway temp directory rooted under the system temp dir, cleaned on
    /// drop. Avoids a dev-dependency just for fixtures.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            let unique = format!(
                "lagom-config-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            path.push(unique);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn discover_finds_working_dir_config() {
        let tmp = TempDir::new("cwd");
        fs::write(tmp.path.join("lagom.toml"), "").unwrap();
        let found = discover(&tmp.path);
        assert_eq!(found, vec![tmp.path.join("lagom.toml")]);
    }

    #[test]
    fn discover_empty_when_no_config() {
        let tmp = TempDir::new("empty");
        // Isolate from any real $HOME/XDG config by pointing XDG at the temp dir.
        temp_env_xdg(&tmp.path, || {
            assert!(discover(&tmp.path).is_empty());
        });
    }

    #[test]
    fn search_paths_lists_absent_candidates_in_precedence_order() {
        // The reporting contract: with nothing on disk, `discover` is empty but
        // `search_paths` still names every layer it looked at, cwd first, XDG
        // last. Without this an operator cannot tell a missed config from an
        // intentionally unconfigured run.
        let tmp = TempDir::new("searched");
        let xdg = tmp.path.join("xdg");
        fs::create_dir_all(&xdg).unwrap();
        temp_env_xdg(&xdg, || {
            let searched = search_paths(&tmp.path);
            assert!(
                discover(&tmp.path).is_empty(),
                "fixture must have no config on disk"
            );
            assert_eq!(
                searched,
                vec![
                    tmp.path.join("lagom.toml"),
                    xdg.join("lagom").join("lagom.toml"),
                ],
                "absent candidates are still reported, highest precedence first"
            );
        });
    }

    #[test]
    fn search_paths_does_not_repeat_cwd_as_project_root() {
        // cwd *is* the project root here; the searched list must not double-report
        // the same path, or the diagnostic misleads about where lagom looked.
        let tmp = TempDir::new("searched-root");
        fs::create_dir_all(tmp.path.join(".git")).unwrap();
        let xdg = tmp.path.join("xdg");
        fs::create_dir_all(&xdg).unwrap();
        temp_env_xdg(&xdg, || {
            assert_eq!(
                search_paths(&tmp.path),
                vec![
                    tmp.path.join("lagom.toml"),
                    xdg.join("lagom").join("lagom.toml"),
                ]
            );
        });
    }

    #[test]
    fn discover_orders_cwd_before_project_root() {
        let tmp = TempDir::new("root");
        // tmp is the project root (has .git); nested/ is the working dir.
        fs::create_dir_all(tmp.path.join(".git")).unwrap();
        fs::write(tmp.path.join("lagom.toml"), "").unwrap();
        let nested = tmp.path.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("lagom.toml"), "").unwrap();

        let found = discover(&nested);
        assert_eq!(
            found,
            vec![nested.join("lagom.toml"), tmp.path.join("lagom.toml")],
            "cwd config must precede project-root config"
        );
    }

    #[test]
    fn profiles_lists_named_profiles_sorted() {
        let tmp = TempDir::new("profiles");
        let profile_dir = tmp.path.join(".lagom");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(profile_dir.join("read-only.toml"), "").unwrap();
        fs::write(profile_dir.join("admin.toml"), "").unwrap();
        fs::write(profile_dir.join("notes.txt"), "").unwrap();

        assert_eq!(
            profiles(&tmp.path),
            vec!["admin".to_string(), "read-only".to_string()]
        );
    }

    #[test]
    fn profiles_empty_when_no_dir() {
        let tmp = TempDir::new("noprofiles");
        assert!(profiles(&tmp.path).is_empty());
    }

    #[test]
    fn resolve_profile_finds_existing_only() {
        let tmp = TempDir::new("resolve");
        let profile_dir = tmp.path.join(".lagom");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(profile_dir.join("read-only.toml"), "").unwrap();

        assert_eq!(
            resolve_profile(&tmp.path, "read-only"),
            Some(profile_dir.join("read-only.toml"))
        );
        assert_eq!(resolve_profile(&tmp.path, "missing"), None);
    }

    /// Run `f` with `XDG_CONFIG_HOME`/`HOME` pointed at `dir` so discovery cannot
    /// see the developer's real user config. Restores the environment after.
    ///
    /// Env mutation is process-global; this test crate is small and these tests
    /// do not run truly in parallel against the same vars, but we still guard
    /// with a mutex to be safe under `cargo test`'s thread pool.
    fn temp_env_xdg(dir: &Path, f: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: single-threaded within the lock for the duration of `f`.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir);
            std::env::set_var("HOME", dir);
        }

        f();

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
}
