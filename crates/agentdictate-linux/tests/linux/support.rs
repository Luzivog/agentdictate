use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
static EXECUTABLE_FIXTURE_LOCK: Mutex<()> = Mutex::new(());

pub struct TestDirectory {
    path: PathBuf,
    _executable_fixture_guard: MutexGuard<'static, ()>,
}

impl TestDirectory {
    pub fn new() -> Self {
        // These tests execute freshly published shell scripts. Serializing the
        // fixture lifetime avoids Linux ETXTBSY races between parallel tests
        // without weakening production command concurrency coverage.
        let executable_fixture_guard = EXECUTABLE_FIXTURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "agentdictate-linux-test-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("test directory is created");
        Self {
            path,
            _executable_fixture_guard: executable_fixture_guard,
        }
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn executable(&self, name: &str, script: &str) -> PathBuf {
        let path = self.path.join(name);
        let temporary = self.path.join(format!(".{name}.tmp"));
        fs::write(&temporary, script).expect("fake executable is written");
        let mut permissions = fs::metadata(&temporary)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&temporary, permissions).expect("fake executable is executable");
        // Publishing by rename avoids ETXTBSY if a previous crashed test left
        // an owner process executing the old inode at the same path.
        fs::rename(temporary, &path).expect("fake executable is published atomically");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
