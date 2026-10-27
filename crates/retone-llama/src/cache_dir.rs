//! Where models live: `~/.cache/retone`,
//! or `RETONE_CACHE_ROOT` when set, in which case model dirs left in the home
//! cache move there once (renamed, or copied through a staging dir across
//! volumes), never overwriting what the new root already has. A failed move
//! rolls back and keeps the home cache for this launch.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const CACHE_ROOT_ENV: &str = "RETONE_CACHE_ROOT";

/// Only these subdirs move to an overridden root. `models` holds the local
/// LLMs; the others are caches older releases wrote.
const RELOCATED_SUBDIRS: [&str; 4] = [
    "whisper-models",
    "parakeet-models",
    "diarization-models",
    "models",
];

/// The `~/.cache/retone` default.
pub fn home_cache(home: &Path) -> PathBuf {
    home.join(".cache").join("retone")
}

/// Filesystem moves, behind a seam so failure paths are testable.
pub trait MoveOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn copy_dir(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
}

pub struct RealFs;

fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

impl MoveOps for RealFs {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    fn copy_dir(&self, from: &Path, to: &Path) -> io::Result<()> {
        copy_recursive(from, to)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        match fs::remove_dir_all(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

struct Move {
    from: PathBuf,
    to: PathBuf,
    copied: bool,
}

fn rollback(ops: &dyn MoveOps, moves: Vec<Move>) {
    for m in moves.into_iter().rev() {
        if !m.to.exists() {
            continue;
        }
        if m.copied {
            if ops.copy_dir(&m.to, &m.from).is_ok() {
                let _ = ops.remove_dir(&m.to);
            }
        } else if !m.from.exists() {
            let _ = ops.rename(&m.to, &m.from);
        }
    }
}

fn migrate(ops: &dyn MoveOps, legacy: &Path, target: &Path) -> bool {
    let mut moves: Vec<Move> = Vec::new();
    let mut staging: Option<PathBuf> = None;
    let result = (|| -> io::Result<()> {
        fs::create_dir_all(target)?;
        for subdir in RELOCATED_SUBDIRS {
            let from = legacy.join(subdir);
            let to = target.join(subdir);
            if !from.exists() || to.exists() {
                continue;
            }
            if ops.rename(&from, &to).is_ok() {
                moves.push(Move {
                    from,
                    to,
                    copied: false,
                });
                continue;
            }
            // Cross-volume: copy into a staging dir first so an interrupted
            // copy can never pass for a complete model dir.
            let stage = PathBuf::from(format!("{}.migrating", to.display()));
            staging = Some(stage.clone());
            ops.remove_dir(&stage)?;
            ops.copy_dir(&from, &stage)?;
            ops.rename(&stage, &to)?;
            staging = None;
            moves.push(Move {
                from,
                to,
                copied: true,
            });
        }
        for m in &moves {
            if m.copied {
                ops.remove_dir(&m.from)?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(target: "models", %error, "Model cache migration failed; keeping the home cache");
            if let Some(stage) = staging {
                let _ = ops.remove_dir(&stage);
            }
            rollback(ops, moves);
            false
        }
    }
}

/// Resolves the cache root once per (legacy, target) pair and remembers a
/// completed migration, like the JS module-level memo.
pub struct CacheRoot {
    migrated: Mutex<Option<(PathBuf, PathBuf)>>,
}

impl Default for CacheRoot {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheRoot {
    pub const fn new() -> Self {
        Self {
            migrated: Mutex::new(None),
        }
    }

    /// The cache root for `home`, honouring `override_root` (the value of
    /// `RETONE_CACHE_ROOT`, when set and non-empty).
    pub fn resolve(&self, home: &Path, override_root: Option<&str>, ops: &dyn MoveOps) -> PathBuf {
        let home_cache = home_cache(home);
        let target = match override_root.filter(|s| !s.is_empty()) {
            Some(root) => PathBuf::from(root),
            None => return home_cache,
        };
        if target == home_cache {
            return home_cache;
        }
        let pair = (home_cache.clone(), target.clone());
        let mut migrated = self.migrated.lock().unwrap_or_else(|e| e.into_inner());
        if migrated.as_ref() == Some(&pair) {
            return target;
        }
        if migrate(ops, &home_cache, &target) {
            *migrated = Some(pair);
            target
        } else {
            home_cache
        }
    }
}

/// The process-wide resolver. `override_root` is the `RETONE_CACHE_ROOT`
/// value from wherever the app reads settings (`userData/.env` or the
/// environment).
pub fn cache_root(override_root: Option<&str>) -> PathBuf {
    static ROOT: CacheRoot = CacheRoot::new();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    ROOT.resolve(&home, override_root, &RealFs)
}

/// `<cache root>/models`.
pub fn models_dir(override_root: Option<&str>) -> PathBuf {
    cache_root(override_root).join("models")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn preserves_the_macos_home_cache_default() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("Users/stan");
        let root = CacheRoot::new().resolve(&home, None, &RealFs);
        assert_eq!(root, home.join(".cache/retone"));
        assert_eq!(
            CacheRoot::new().resolve(&home, Some(""), &RealFs),
            home.join(".cache/retone")
        );
    }

    #[test]
    fn honours_retone_cache_root() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("Users/stan");
        let custom = temp.path().join("custom-cache");
        let root = CacheRoot::new().resolve(&home, custom.to_str(), &RealFs);
        assert_eq!(root, custom);
        assert!(custom.exists());
    }

    #[test]
    fn migrates_legacy_model_dirs_into_the_override_root() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let legacy = home.join(".cache/retone");
        write(&legacy.join("models"), "qwen.gguf", "llm");
        fs::create_dir_all(legacy.join("qdrant-data")).unwrap();
        let custom = temp.path().join("custom");

        let root = CacheRoot::new().resolve(&home, custom.to_str(), &RealFs);

        assert_eq!(root, custom);
        assert_eq!(
            fs::read_to_string(custom.join("models/qwen.gguf")).unwrap(),
            "llm"
        );
        assert!(!legacy.join("models").exists());
        assert!(legacy.join("qdrant-data").exists(), "only model dirs move");
    }

    #[test]
    fn never_overwrites_models_already_at_the_override_root() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let legacy = home.join(".cache/retone/models");
        write(&legacy, "a.gguf", "old");
        let custom = temp.path().join("custom");
        write(&custom.join("models"), "a.gguf", "new");

        CacheRoot::new().resolve(&home, custom.to_str(), &RealFs);

        assert_eq!(
            fs::read_to_string(custom.join("models/a.gguf")).unwrap(),
            "new"
        );
        assert_eq!(fs::read_to_string(legacy.join("a.gguf")).unwrap(), "old");
    }

    /// Renames of the legacy dir fail (cross-volume); copies can be blocked.
    struct CrossVolume<'a> {
        legacy_models: &'a Path,
        block_copy: RefCell<bool>,
    }

    impl MoveOps for CrossVolume<'_> {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            if from == self.legacy_models {
                return Err(io::Error::other("EXDEV"));
            }
            RealFs.rename(from, to)
        }
        fn copy_dir(&self, from: &Path, to: &Path) -> io::Result<()> {
            if from == self.legacy_models && *self.block_copy.borrow() {
                return Err(io::Error::other("copy blocked"));
            }
            RealFs.copy_dir(from, to)
        }
        fn remove_dir(&self, path: &Path) -> io::Result<()> {
            RealFs.remove_dir(path)
        }
    }

    #[test]
    fn stages_cross_volume_migrations_before_removing_the_legacy_dir() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let legacy_models = home.join(".cache/retone/models");
        write(&legacy_models, "m.gguf", "model");
        let custom = temp.path().join("custom");
        let ops = CrossVolume {
            legacy_models: &legacy_models,
            block_copy: RefCell::new(false),
        };

        let root = CacheRoot::new().resolve(&home, custom.to_str(), &ops);

        assert_eq!(root, custom);
        assert!(custom.join("models/m.gguf").exists());
        assert!(!legacy_models.exists());
        assert!(!custom.join("models.migrating").exists());
    }

    #[test]
    fn keeps_the_legacy_cache_after_a_failed_migration_and_retries_later() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let legacy_root = home.join(".cache/retone");
        let legacy_models = legacy_root.join("models");
        write(&legacy_models, "m.gguf", "model");
        let custom = temp.path().join("custom");
        let resolver = CacheRoot::new();
        let ops = CrossVolume {
            legacy_models: &legacy_models,
            block_copy: RefCell::new(true),
        };

        assert_eq!(resolver.resolve(&home, custom.to_str(), &ops), legacy_root);
        assert!(legacy_models.join("m.gguf").exists());
        assert!(!custom.join("models.migrating").exists());

        *ops.block_copy.borrow_mut() = false;
        assert_eq!(resolver.resolve(&home, custom.to_str(), &ops), custom);
        assert!(custom.join("models/m.gguf").exists());
        assert!(!legacy_models.exists());
    }

    #[test]
    fn rolls_back_earlier_moves_when_a_later_one_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let legacy_root = home.join(".cache/retone");
        write(&legacy_root.join("whisper-models"), "w.bin", "w");
        let legacy_models = legacy_root.join("models");
        write(&legacy_models, "m.gguf", "model");
        let custom = temp.path().join("custom");
        let ops = CrossVolume {
            legacy_models: &legacy_models,
            block_copy: RefCell::new(true),
        };

        assert_eq!(
            CacheRoot::new().resolve(&home, custom.to_str(), &ops),
            legacy_root
        );
        assert!(legacy_root.join("whisper-models/w.bin").exists());
        assert!(!custom.join("whisper-models").exists());
    }
}
