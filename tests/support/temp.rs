//! Atomically created owner-only temp dirs with RAII cleanup.

use std::collections::HashSet;
use std::fs::{self, DirBuilder, File};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const TEMP_DIR_MODE: u32 = 0o700;
pub const SNAPSHOT_FILE_MODE: u32 = 0o700;
pub const SNAPSHOT_READONLY_FILE_MODE: u32 = 0o500;
pub const DUMP_FILE_MODE: u32 = 0o600;

#[derive(Debug)]
pub struct OwnedTempDir {
    path: PathBuf,
}

impl OwnedTempDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for OwnedTempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for OwnedTempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

fn restore_owner_write_tree(path: &Path) {
    restore_owner_write_tree_inner(path, &mut HashSet::new());
}

fn restore_owner_write_tree_inner(path: &Path, visited: &mut HashSet<(u64, u64)>) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    let key = (meta.dev(), meta.ino());
    if !visited.insert(key) {
        return;
    }
    let mode = if meta.is_dir() {
        TEMP_DIR_MODE
    } else {
        SNAPSHOT_FILE_MODE
    };
    let _ = chmod_owner_only(path, mode);
    if !meta.is_dir() {
        return;
    }
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            restore_owner_write_tree_inner(&entry.path(), visited);
        }
    }
}

fn unlink_tree_nofollow(path: &Path, visited: &mut HashSet<(u64, u64)>) -> Result<(), String> {
    let meta =
        fs::symlink_metadata(path).map_err(|err| format!("lstat {}: {err}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        fs::remove_file(path).map_err(|err| format!("unlink {}: {err}", path.display()))?;
        return Ok(());
    }
    let key = (meta.dev(), meta.ino());
    if !visited.insert(key) {
        return Ok(());
    }
    let entries =
        fs::read_dir(path).map_err(|err| format!("read_dir {}: {err}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("read_dir {}: {err}", path.display()))?;
        unlink_tree_nofollow(&entry.path(), visited)?;
    }
    fs::remove_dir(path).map_err(|err| format!("rmdir {}: {err}", path.display()))
}

fn remove_owned_temp_dir(path: &Path) -> Result<(), String> {
    restore_owner_write_tree(path);
    if unlink_tree_nofollow(path, &mut HashSet::new()).is_ok() {
        return Ok(());
    }
    restore_owner_write_tree(path);
    unlink_tree_nofollow(path, &mut HashSet::new())
        .map_err(|err| format!("failed to remove task temp {}: {err}", path.display()))?;
    Ok(())
}

impl Drop for OwnedTempDir {
    fn drop(&mut self) {
        if remove_owned_temp_dir(&self.path).is_ok() {
            return;
        }
        if self.path.exists() && !std::thread::panicking() {
            panic!(
                "failed to remove task temp {}: leftover after restoring owner permissions",
                self.path.display()
            );
        }
    }
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn test_temp_root() -> PathBuf {
    if let Some(dir) = env_path("CARGO_TARGET_TMPDIR") {
        return dir;
    }
    if let Some(dir) = env_path("CARGO_TARGET_DIR") {
        return dir.join("ngx-rustscript-test-tmp");
    }
    std::env::temp_dir()
}

fn chmod_owner_only(path: &Path, mode: u32) -> Result<(), String> {
    let mut perms = fs::metadata(path)
        .map_err(|err| format!("stat {}: {err}", path.display()))?
        .permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).map_err(|err| format!("chmod {}: {err}", path.display()))
}

fn random_u64() -> u64 {
    let mut buf = [0u8; 8];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut buf))
        .unwrap_or_else(|err| panic!("read /dev/urandom: {err}"));
    u64::from_le_bytes(buf)
}

/// Create a unique owner-only directory. `mkdir` of the leaf is atomic.
pub fn unique_temp_dir(label: &str) -> OwnedTempDir {
    let parent = test_temp_root().join("ngx-rustscript-module-load");
    DirBuilder::new()
        .recursive(true)
        .mode(TEMP_DIR_MODE)
        .create(&parent)
        .unwrap_or_else(|err| panic!("create temp parent {}: {err}", parent.display()));
    chmod_owner_only(&parent, TEMP_DIR_MODE).unwrap_or_else(|err| panic!("{err}"));
    let safe_label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    for _ in 0..256 {
        let dir = parent.join(format!("{safe_label}-{:016x}", random_u64()));
        match DirBuilder::new().mode(TEMP_DIR_MODE).create(&dir) {
            Ok(()) => {
                chmod_owner_only(&dir, TEMP_DIR_MODE).unwrap_or_else(|err| panic!("{err}"));
                return OwnedTempDir { path: dir };
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => panic!("create unique temp {}: {err}", dir.display()),
        }
    }
    panic!("could not allocate a unique owner-only temp dir for {label}");
}

pub fn chmod_path(path: &Path, mode: u32) -> Result<(), String> {
    chmod_owner_only(path, mode)
}

pub fn freeze_snapshot(snapshot: &Path) -> Result<(), String> {
    let dir = snapshot
        .parent()
        .ok_or_else(|| format!("snapshot {} has no parent", snapshot.display()))?;
    let dump = dir.join("ngx_rustscript_module_identity.dump");
    let _ = fs::remove_file(&dump);
    chmod_owner_only(snapshot, SNAPSHOT_READONLY_FILE_MODE)?;
    chmod_owner_only(dir, TEMP_DIR_MODE)?;
    Ok(())
}

pub fn isolated_prefix(label: &str) -> OwnedTempDir {
    let prefix = unique_temp_dir(label);
    for child in ["logs", "conf", "html"] {
        DirBuilder::new()
            .recursive(true)
            .mode(TEMP_DIR_MODE)
            .create(prefix.join(child))
            .unwrap();
    }
    prefix
}

#[test]
fn unique_temp_dir_is_owner_only_and_removed_on_drop() {
    let path;
    {
        let dir = unique_temp_dir("raii");
        path = dir.path().to_path_buf();
        assert!(path.is_dir());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, TEMP_DIR_MODE);
    }
    assert!(
        !path.exists(),
        "temp dir must be removed on Drop: {}",
        path.display()
    );
}

#[test]
fn unique_temp_dir_is_removed_on_panic() {
    let holder = std::sync::Mutex::new(None::<PathBuf>);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dir = unique_temp_dir("panic-clean");
        *holder.lock().unwrap() = Some(dir.path().to_path_buf());
        panic!("force unwind for RAII");
    }));
    std::panic::set_hook(prev);
    assert!(panicked.is_err());
    let path = holder.lock().unwrap().clone().expect("captured path");
    assert!(
        !path.exists(),
        "temp dir must be removed on panic unwind: {}",
        path.display()
    );
}

#[test]
fn unique_temp_dirs_do_not_collide() {
    let a = unique_temp_dir("collide");
    let b = unique_temp_dir("collide");
    assert_ne!(a.path(), b.path());
    assert!(a.path().is_dir());
    assert!(b.path().is_dir());
}

fn write_snapshot_file(dir: &Path) -> PathBuf {
    let snapshot = dir.join("libngx_rustscript.so");
    fs::write(&snapshot, b"placeholder-snapshot").unwrap();
    chmod_owner_only(&snapshot, SNAPSHOT_FILE_MODE).unwrap();
    snapshot
}

#[test]
fn frozen_snapshot_directory_is_removed_on_drop() {
    let path;
    {
        let dir = unique_temp_dir("frozen-drop");
        let snapshot = write_snapshot_file(dir.path());
        freeze_snapshot(&snapshot).expect("freeze snapshot");
        let file_mode = fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, SNAPSHOT_READONLY_FILE_MODE);
        assert_eq!(
            dir_mode, TEMP_DIR_MODE,
            "frozen snapshot parent must stay owner-writable so Drop can remove it"
        );
        path = dir.path().to_path_buf();
        assert!(path.is_dir());
    }
    assert!(
        !path.exists(),
        "frozen snapshot directory must be removed on Drop: {}",
        path.display()
    );
}

#[test]
fn frozen_snapshot_directory_is_removed_on_panic_unwind() {
    let holder = std::sync::Mutex::new(None::<PathBuf>);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dir = unique_temp_dir("frozen-panic");
        let snapshot = write_snapshot_file(dir.path());
        freeze_snapshot(&snapshot).expect("freeze snapshot");
        *holder.lock().unwrap() = Some(dir.path().to_path_buf());
        panic!("force unwind for frozen RAII");
    }));
    std::panic::set_hook(prev);
    assert!(panicked.is_err());
    let path = holder.lock().unwrap().clone().expect("captured path");
    assert!(
        !path.exists(),
        "frozen snapshot directory must be removed on panic unwind: {}",
        path.display()
    );
}

#[test]
fn drop_does_not_chmod_or_remove_external_symlink_target() {
    let external_parent = unique_temp_dir("ext-target-parent");
    let external = external_parent.join("keep-me");
    fs::write(&external, b"external-secret").unwrap();
    chmod_owner_only(&external, 0o644).unwrap();
    let original_mode = fs::metadata(&external).unwrap().permissions().mode() & 0o777;
    let path;
    {
        let dir = unique_temp_dir("symlink-external");
        std::os::unix::fs::symlink(&external, dir.join("link-out")).unwrap();
        path = dir.path().to_path_buf();
        assert!(path.is_dir());
    }
    assert!(!path.exists(), "temp dir with symlink must be removed");
    assert!(
        external.exists(),
        "external symlink target must remain: {}",
        external.display()
    );
    let after = fs::read(&external).unwrap();
    assert_eq!(after, b"external-secret");
    let after_mode = fs::metadata(&external).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        after_mode, original_mode,
        "cleanup must not chmod the external target"
    );
}

#[test]
fn drop_unlinks_symlink_loop_without_hanging() {
    let path;
    {
        let dir = unique_temp_dir("symlink-loop");
        std::os::unix::fs::symlink(dir.path(), dir.join("loop")).unwrap();
        std::os::unix::fs::symlink("loop", dir.join("loop2")).unwrap();
        path = dir.path().to_path_buf();
    }
    assert!(
        !path.exists(),
        "temp dir with symlink loop must be removed: {}",
        path.display()
    );
}

#[test]
fn panic_cleanup_leaves_external_symlink_target() {
    let external_parent = unique_temp_dir("ext-panic-parent");
    let external = external_parent.join("keep-panic");
    fs::write(&external, b"still-here").unwrap();
    chmod_owner_only(&external, 0o600).unwrap();
    let holder = std::sync::Mutex::new(None::<PathBuf>);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dir = unique_temp_dir("symlink-panic");
        std::os::unix::fs::symlink(&external, dir.join("link-out")).unwrap();
        *holder.lock().unwrap() = Some(dir.path().to_path_buf());
        panic!("force unwind for symlink RAII");
    }));
    std::panic::set_hook(prev);
    assert!(panicked.is_err());
    let path = holder.lock().unwrap().clone().expect("captured path");
    assert!(
        !path.exists(),
        "temp dir must be removed on panic unwind: {}",
        path.display()
    );
    assert_eq!(fs::read(&external).unwrap(), b"still-here");
    let mode = fs::metadata(&external).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
