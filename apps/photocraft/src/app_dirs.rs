//! Where the desktop app keeps its per-user data: preferences, brush presets, crash-recovery
//! autosaves and the GPU startup marker all live under the one directory [`config_dir`] returns.
//!
//! The rules (`PHOTOCRAFT_CONFIG_DIR`, portable mode, the platform convention) live in
//! [`photocraft_automation::app_dirs`], so `photocraft-cli` resolves the same folder; this module
//! resolves it once for the app and logs the outcome.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use photocraft_automation::app_dirs::{DataDir, resolve};

/// The data directory for this process, resolved once (the portable check probes the disk).
pub fn current() -> &'static DataDir {
    static DIR: OnceLock<DataDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        let d = resolve(|k| std::env::var_os(k), exe.as_deref().and_then(Path::parent));
        if let Some(w) = &d.warning {
            log::warn!("{w}");
            eprintln!("photocraft: {w}");
        }
        log::info!("settings directory ({:?}): {:?}", d.mode, d.dir);
        d
    })
}

/// The per-user settings directory (see the module docs).
pub fn config_dir() -> Option<PathBuf> {
    current().dir.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_automation::app_dirs::{Mode, PORTABLE_DATA_DIR, PORTABLE_MARKERS, platform_dir};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("photocraft-app-dirs-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn env_of(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let m: HashMap<String, OsString> = pairs.iter().map(|(k, v)| (k.to_string(), v.as_os_str().to_owned())).collect();
        move |k| m.get(k).cloned()
    }

    /// A fake user folder that every platform's convention resolves under.
    fn fake_home(root: &Path) -> impl Fn(&str) -> Option<OsString> + use<> {
        let home = root.join("home");
        let appdata = root.join("appdata");
        env_of(&[("HOME", &home), ("APPDATA", &appdata)])
    }

    #[test]
    fn no_marker_uses_the_platform_dir() {
        let root = temp("platform");
        let exe = root.join("bin");
        std::fs::create_dir_all(&exe).unwrap();
        let env = fake_home(&root);
        let d = resolve(&env, Some(&exe));
        assert_eq!(d.mode, Mode::Platform);
        assert!(d.warning.is_none());
        let dir = d.dir.unwrap();
        assert_eq!(Some(dir.clone()), platform_dir(&env));
        assert!(!dir.starts_with(&exe));
        assert!(!exe.join(PORTABLE_DATA_DIR).exists(), "no portable folder without a marker");
        // The convention per platform.
        if cfg!(target_os = "macos") {
            assert_eq!(dir, root.join("home/Library/Application Support/Photocraft"));
        } else if cfg!(windows) {
            assert_eq!(dir, root.join("appdata").join("Photocraft"));
        } else {
            assert_eq!(dir, root.join("home/.config/photocraft"));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn either_marker_keeps_everything_beside_the_exe() {
        for marker in PORTABLE_MARKERS {
            let root = temp("portable");
            let exe = root.join("PhotoCraft");
            std::fs::create_dir_all(&exe).unwrap();
            std::fs::write(exe.join(marker), b"").unwrap();
            let d = resolve(fake_home(&root), Some(&exe));
            assert_eq!(d.mode, Mode::Portable, "{marker}");
            assert!(d.warning.is_none());
            let dir = d.dir.unwrap();
            assert_eq!(dir, exe.join(PORTABLE_DATA_DIR));
            assert!(dir.is_dir(), "the data folder is created");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the write probe is removed");
            // Every derived path sits under the exe dir.
            for sub in ["preferences.json", "Presets", "Recovery", crate::gpu_startup::MARKER_FILE] {
                assert!(dir.join(sub).starts_with(&exe));
            }
            assert!(!root.join("home").exists() && !root.join("appdata").exists(), "nothing written to the user folder");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_marker_folder_is_not_a_marker() {
        let root = temp("dir-marker");
        let exe = root.join("bin");
        std::fs::create_dir_all(exe.join("portable.txt")).unwrap();
        assert_eq!(resolve(fake_home(&root), Some(&exe)).mode, Mode::Platform);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unwritable_portable_folder_falls_back_with_a_warning() {
        let root = temp("unwritable");
        let exe = root.join("bin");
        std::fs::create_dir_all(&exe).unwrap();
        std::fs::write(exe.join("portable.txt"), b"").unwrap();
        // A file where the data folder should go: it can't be created, on every platform.
        std::fs::write(exe.join(PORTABLE_DATA_DIR), b"in the way").unwrap();
        let env = fake_home(&root);
        let d = resolve(&env, Some(&exe));
        assert_eq!(d.mode, Mode::Platform);
        assert_eq!(d.dir, platform_dir(&env));
        let w = d.warning.unwrap();
        assert!(w.contains("portable") && w.contains("isn't writable"), "{w}");
        // No user folder at all: still no panic, just no directory.
        let d = resolve(|_| None, Some(&exe));
        assert!(d.dir.is_none() && d.warning.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn override_wins_over_portable() {
        let root = temp("override");
        let exe = root.join("bin");
        std::fs::create_dir_all(&exe).unwrap();
        std::fs::write(exe.join("portable.txt"), b"").unwrap();
        let custom = root.join("custom");
        let d = resolve(env_of(&[("PHOTOCRAFT_CONFIG_DIR", &custom)]), Some(&exe));
        assert_eq!((d.mode, d.dir), (Mode::Override, Some(custom)));
        assert!(!exe.join(PORTABLE_DATA_DIR).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_exe_dir_or_env_never_panics() {
        let d = resolve(|_| None, None);
        assert_eq!((d.mode, d.dir), (Mode::Platform, None));
        let empty = PathBuf::new();
        let d = resolve(env_of(&[("PHOTOCRAFT_CONFIG_DIR", &empty), ("HOME", &empty), ("APPDATA", &empty)]), None);
        assert_eq!(d.dir, None, "empty variables are ignored");
    }
}
