//! Cross-process coalescing via a stable metadata lock, never the credential
//! lock. Only nested order: metadata -> authority. Stores normalized data only.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use nix::fcntl::{renameat, AtFlags, OFlag};
use nix::sys::stat::fstatat;
use nix::unistd::{unlinkat, UnlinkatFlags};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{models::ClaudeModel, timestamp, usage::UsageData, ClaudeError};
use crate::providers::claude::{
    private_files::{check_directory, open_directory, private_open, OpenError},
    ClaudeErrorKind, ClaudeIdentityBinding,
};

const FILE: &str = "claude_metadata.json";
const LOCK: &str = "claude_metadata.lock";
const MAX_CACHE: usize = 2 * 1024 * 1024;
pub(super) const MAX_CONTEXTS: usize = 64;
fn storage() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::CredentialStorageError)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Key {
    pub provider: String,
    pub identity: ClaudeIdentityBinding,
    pub generation: Uuid,
    pub compatibility_profile: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Timed<T> {
    #[serde(with = "timestamp")]
    pub fetched_at: Option<SystemTime>,
    pub data: T,
}
impl<T> Timed<T> {
    pub fn new(data: T) -> Self {
        Self {
            fetched_at: Some(SystemTime::now()),
            data,
        }
    }
    pub fn fresh(&self, seconds: u64) -> bool {
        fresh(self.fetched_at, seconds)
    }
}
pub(super) fn fresh(fetched_at: Option<SystemTime>, seconds: u64) -> bool {
    fetched_at
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < Duration::from_secs(seconds))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Failure {
    pub error: ClaudeError,
    #[serde(with = "timestamp")]
    pub retry_at: Option<SystemTime>,
    pub blocks_fallback: bool,
}
impl Failure {
    pub fn cooling(&self) -> bool {
        self.retry_at.is_some_and(|t| t > SystemTime::now())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub schema_version: u32,
    pub key: Key,
    pub catalog: Option<Timed<Vec<ClaudeModel>>>,
    pub contexts: BTreeMap<String, Timed<ClaudeModel>>,
    pub usage: Option<Timed<UsageData>>,
    pub catalog_failure: Option<Failure>,
    pub context_failures: BTreeMap<String, Failure>,
    pub usage_failure: Option<Failure>,
}
impl Record {
    pub fn new(key: Key) -> Self {
        Self {
            schema_version: 1,
            key,
            catalog: None,
            contexts: BTreeMap::new(),
            usage: None,
            catalog_failure: None,
            context_failures: BTreeMap::new(),
            usage_failure: None,
        }
    }
    fn valid(&self) -> bool {
        use super::models::{context, valid_id, MAX_MODELS};
        fn valid_model(model: &ClaudeModel) -> bool {
            valid_id(&model.id)
                && model
                    .display_name
                    .as_ref()
                    .is_none_or(|s| super::models::label(&serde_json::json!(s)).is_some())
                && model
                    .context_window
                    .is_none_or(|n| context(&serde_json::json!(n)).is_some())
        }
        self.schema_version == 1
            && self.contexts.len() <= MAX_CONTEXTS
            && self.context_failures.len() <= MAX_CONTEXTS
            && self.catalog.as_ref().is_none_or(|c| {
                c.fetched_at.is_some()
                    && c.data.len() <= MAX_MODELS
                    && c.data.iter().all(valid_model)
            })
            && self.contexts.iter().all(|(id, c)| {
                valid_id(id) && c.fetched_at.is_some() && c.data.id == *id && valid_model(&c.data)
            })
            && self.context_failures.keys().all(|id| valid_id(id))
            && self
                .usage
                .as_ref()
                .is_none_or(|u| u.fetched_at.is_some() && u.data.valid())
    }
}
pub(super) struct Cache {
    directory: PathBuf,
}
impl Cache {
    pub fn new(directory: PathBuf) -> Result<Self, ClaudeError> {
        if !directory.is_absolute()
            || directory.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(storage());
        }
        Ok(Self { directory })
    }
    pub async fn lock(&self) -> Result<Locked, ClaudeError> {
        let directory = open_directory(&self.directory, true)?.ok_or_else(storage)?;
        let lock = private_open(&directory, LOCK, OFlag::O_RDWR | OFlag::O_CREAT)
            .map_err(|_| storage())?;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(_) => return Err(storage()),
            }
        }
        let locked = Locked { directory, lock };
        locked.check()?;
        Ok(locked)
    }
}
pub(super) struct Locked {
    directory: File,
    lock: File,
}
impl Locked {
    pub fn check(&self) -> Result<(), ClaudeError> {
        check_directory(&self.directory, true)?;
        let named = fstatat(
            self.directory.as_raw_fd(),
            LOCK,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|_| storage())?;
        let opened = self.lock.metadata().map_err(|_| storage())?;
        if named.st_ino != opened.ino()
            || named.st_dev as u64 != opened.dev()
            || opened.nlink() != 1
            || !opened.is_file()
            || opened.mode() & 0o777 != 0o600
            || opened.uid() != unsafe { nix::libc::getuid() }
        {
            return Err(storage());
        }
        Ok(())
    }
    pub fn load(&self, key: Key) -> Result<Record, ClaudeError> {
        self.check()?;
        let mut file = match private_open(&self.directory, FILE, OFlag::O_RDONLY) {
            Ok(file) => file,
            Err(OpenError::Missing) => return Ok(Record::new(key)),
            Err(_) => return Err(storage()),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_CACHE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| storage())?;
        if bytes.len() > MAX_CACHE {
            return Err(storage());
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| storage())?;
        if !record.valid() {
            return Err(storage());
        }
        if record.key != key {
            return Ok(Record::new(key));
        }
        Ok(record)
    }
    pub fn write(&self, record: &Record) -> Result<(), ClaudeError> {
        self.check()?;
        if !record.valid() {
            return Err(storage());
        }
        let bytes = serde_json::to_vec(record).map_err(|_| storage())?;
        if bytes.len() > MAX_CACHE {
            return Err(storage());
        }
        let name = format!(".claude-metadata-{}", Uuid::new_v4());
        let result = (|| {
            let mut file = private_open(
                &self.directory,
                &name,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL,
            )
            .map_err(|_| storage())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| storage())?;
            self.check()?;
            match private_open(&self.directory, FILE, OFlag::O_RDONLY) {
                Ok(_) | Err(OpenError::Missing) => (),
                Err(_) => return Err(storage()),
            }
            renameat(
                Some(self.directory.as_raw_fd()),
                name.as_str(),
                Some(self.directory.as_raw_fd()),
                FILE,
            )
            .map_err(|_| storage())?;
            self.directory.sync_all().map_err(|_| storage())
        })();
        let _ = unlinkat(
            Some(self.directory.as_raw_fd()),
            name.as_str(),
            UnlinkatFlags::NoRemoveDir,
        );
        result
    }
}
