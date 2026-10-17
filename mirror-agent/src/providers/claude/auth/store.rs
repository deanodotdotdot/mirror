//! Descriptor-relative, private storage. Never lock the atomically replaced inode.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use nix::fcntl::{open, openat, renameat, AtFlags, OFlag};
use nix::sys::stat::{fstatat, mkdirat, Mode};
use nix::unistd::{unlinkat, UnlinkatFlags};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use uuid::Uuid;

use super::{storage_error, CommandError, CommitOutcome, Credential, Phase};
use crate::providers::claude::{ClaudeError, ClaudeTokenVersion};

pub(super) const FILE: &str = "claude_oauth.json";
const LOCK: &str = "claude_oauth.lock";
const MAX_STORE: usize = 65_536;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub schema_version: u32,
    pub generation: Uuid,
    pub revision: u64,
    pub installation_id: Uuid,
    pub credential: Option<Credential>,
    #[serde(default, with = "optional_time")]
    pub last_refreshed_at: Option<SystemTime>,
    #[serde(default)]
    pub reauth_required: Option<ReauthRequired>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReauthRequired {
    pub generation: Uuid,
    pub revision: u64,
    pub reason: ReauthReason,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReauthReason {
    InvalidGrant,
    Revoked,
}

impl Record {
    pub fn tombstone(previous: Option<&Self>) -> Self {
        Self {
            schema_version: 1,
            generation: Uuid::new_v4(),
            revision: 0,
            installation_id: previous.map_or_else(Uuid::new_v4, |v| v.installation_id),
            credential: None,
            last_refreshed_at: None,
            reauth_required: None,
        }
    }
    pub fn version(&self) -> ClaudeTokenVersion {
        ClaudeTokenVersion {
            generation: self.generation,
            revision: self.revision,
        }
    }
    fn validate(&self) -> Result<(), ClaudeError> {
        if self.generation.is_nil()
            || self.installation_id.is_nil()
            || self.credential.is_some() != (self.revision > 0)
        {
            return Err(storage_error());
        }
        if let Some(credential) = &self.credential {
            credential.validate().map_err(|_| storage_error())?;
            if let Some(refreshed) = self.last_refreshed_at {
                if refreshed < credential.authorized_at || refreshed > credential.expires_at {
                    return Err(storage_error());
                }
            }
        } else if self.last_refreshed_at.is_some() || self.reauth_required.is_some() {
            return Err(storage_error());
        }
        if let Some(marker) = &self.reauth_required {
            if marker.generation != self.generation || marker.revision != self.revision {
                return Err(storage_error());
            }
        }
        Ok(())
    }
}

pub(super) struct Store {
    directory: PathBuf,
}

impl Store {
    pub fn new(directory: PathBuf) -> Result<Self, ClaudeError> {
        if !directory.is_absolute()
            || directory
                .components()
                .any(|p| matches!(p, Component::ParentDir | Component::CurDir))
        {
            return Err(storage_error());
        }
        Ok(Self { directory })
    }

    fn directory(&self, create: bool) -> Result<Option<File>, ClaudeError> {
        let fd = open(
            Path::new("/"),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| storage_error())?;
        // SAFETY: open returned a new, owned descriptor.
        let mut directory = unsafe { File::from_raw_fd(fd) };
        let parts: Vec<_> = self
            .directory
            .components()
            .filter_map(|p| match p {
                Component::Normal(name) => Some(name),
                _ => None,
            })
            .collect();
        for (index, part) in parts.iter().enumerate() {
            check_directory(&directory, false)?;
            let last = index + 1 == parts.len();
            if create && last {
                match mkdirat(
                    directory.as_raw_fd(),
                    Path::new(part),
                    Mode::from_bits_truncate(0o700),
                ) {
                    // Persist the new directory entry in its parent as well as
                    // the eventual grant entry inside the new directory.
                    Ok(()) => directory.sync_all().map_err(|_| storage_error())?,
                    Err(nix::errno::Errno::EEXIST) => (),
                    Err(_) => return Err(storage_error()),
                }
            }
            match openat(
                directory.as_raw_fd(),
                Path::new(part),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            ) {
                // SAFETY: openat returned a new, owned descriptor.
                Ok(fd) => directory = unsafe { File::from_raw_fd(fd) },
                Err(nix::errno::Errno::ENOENT) if !create => return Ok(None),
                Err(_) => return Err(storage_error()),
            }
        }
        check_directory(&directory, true)?;
        Ok(Some(directory))
    }

    /// Dropping this future releases any acquired descriptors. No blocking lock task.
    pub async fn lock(&self, create: bool) -> Result<Option<LockedStore>, ClaudeError> {
        let Some(directory) = self.directory(create)? else {
            return Ok(None);
        };
        let lock = match private_open(
            &directory,
            LOCK,
            OFlag::O_RDWR
                | if create {
                    OFlag::O_CREAT
                } else {
                    OFlag::empty()
                },
        ) {
            Ok(file) => file,
            Err(OpenError::Missing) if !create => {
                // A missing installation is read-only. A grant without its stable
                // lock is unexpected and must not be inspected without ordering.
                return match private_open(&directory, FILE, OFlag::O_RDONLY) {
                    Err(OpenError::Missing) => Ok(None),
                    _ => Err(storage_error()),
                };
            }
            Err(_) => return Err(storage_error()),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => (),
                Err(_) => return Err(storage_error()),
            }
            if Instant::now() >= deadline {
                return Err(super::temporary_error());
            }
            tokio::time::sleep_until((Instant::now() + Duration::from_millis(20)).min(deadline))
                .await;
        }
        let store = LockedStore { directory, lock };
        store.check_lock()?;
        Ok(Some(store))
    }
}

fn check_directory(file: &File, private: bool) -> Result<(), ClaudeError> {
    let info = file.metadata().map_err(|_| storage_error())?;
    // SAFETY: getuid has no preconditions and does not access credentials.
    let uid = unsafe { nix::libc::getuid() };
    let unsafe_write = info.mode() & 0o022 != 0 && info.mode() & 0o1000 == 0;
    if !info.is_dir()
        || (info.uid() != 0 && info.uid() != uid)
        || unsafe_write
        || (private && (info.uid() != uid || info.mode() & 0o022 != 0))
    {
        return Err(storage_error());
    }
    Ok(())
}

enum OpenError {
    Missing,
    Unsafe,
}

fn private_open(directory: &File, name: &str, flags: OFlag) -> Result<File, OpenError> {
    let fd = openat(
        directory.as_raw_fd(),
        name,
        flags | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|error| {
        if error == nix::errno::Errno::ENOENT {
            OpenError::Missing
        } else {
            OpenError::Unsafe
        }
    })?;
    // SAFETY: openat returned a new, owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let info = file.metadata().map_err(|_| OpenError::Unsafe)?;
    // SAFETY: getuid has no preconditions.
    if !info.is_file()
        || info.uid() != unsafe { nix::libc::getuid() }
        || info.mode() & 0o777 != 0o600
        || info.nlink() != 1
    {
        return Err(OpenError::Unsafe);
    }
    Ok(file)
}

pub(super) struct LockedStore {
    directory: File,
    lock: File,
}

impl LockedStore {
    fn check_lock(&self) -> Result<(), ClaudeError> {
        check_directory(&self.directory, true)?;
        let named = fstatat(
            self.directory.as_raw_fd(),
            LOCK,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|_| storage_error())?;
        let opened = self.lock.metadata().map_err(|_| storage_error())?;
        if named.st_ino != opened.ino()
            || named.st_dev as u64 != opened.dev()
            || opened.nlink() != 1
            || !opened.is_file()
            || opened.mode() & 0o777 != 0o600
            || opened.uid() != unsafe { nix::libc::getuid() }
        {
            return Err(storage_error());
        }
        Ok(())
    }
    pub fn load(&self) -> Result<Option<Record>, ClaudeError> {
        self.check_lock()?;
        let mut file = match private_open(&self.directory, FILE, OFlag::O_RDONLY) {
            Ok(file) => file,
            Err(OpenError::Missing) => return Ok(None),
            Err(_) => return Err(storage_error()),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_STORE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| storage_error())?;
        if bytes.len() > MAX_STORE {
            return Err(storage_error());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| storage_error())?;
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        {
            return Err(ClaudeError::configuration(
                crate::providers::claude::ClaudeConfigurationError::UnknownSchema,
            ));
        }
        let record: Record = serde_json::from_value(value).map_err(|_| storage_error())?;
        record.validate()?;
        Ok(Some(record))
    }
    pub fn write(&self, record: &Record) -> Result<(), CommandError> {
        self.write_inner(record, false, false)
    }
    fn write_inner(
        &self,
        record: &Record,
        fail_before: bool,
        fail_after: bool,
    ) -> Result<(), CommandError> {
        let failure = || CommandError::new(storage_error(), Phase::Committing);
        self.check_lock().map_err(|_| failure())?;
        record.validate().map_err(|_| failure())?;
        let bytes = serde_json::to_vec(record).map_err(|_| failure())?;
        if bytes.len() > MAX_STORE {
            return Err(failure());
        }
        let name = format!(".claude-oauth-{}", Uuid::new_v4());
        let result = (|| {
            let mut file = private_open(
                &self.directory,
                &name,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL,
            )
            .map_err(|_| failure())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| failure())?;
            self.check_lock().map_err(|_| failure())?;
            match private_open(&self.directory, FILE, OFlag::O_RDONLY) {
                Ok(_) | Err(OpenError::Missing) => (),
                Err(_) => return Err(failure()),
            }
            if fail_before {
                return Err(failure());
            }
            renameat(
                Some(self.directory.as_raw_fd()),
                name.as_str(),
                Some(self.directory.as_raw_fd()),
                FILE,
            )
            .map_err(|_| failure())?;
            if fail_after || self.directory.sync_all().is_err() {
                let mut error = failure();
                error.commit_outcome = Some(CommitOutcome::Unknown);
                return Err(error);
            }
            Ok(())
        })();
        let _ = unlinkat(
            Some(self.directory.as_raw_fd()),
            name.as_str(),
            UnlinkatFlags::NoRemoveDir,
        );
        result
    }

    #[cfg(test)]
    pub fn fault_write(&self, record: &Record, after: bool) -> Result<(), CommandError> {
        self.write_inner(record, !after, after)
    }
}

pub(super) mod timestamp {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        time: &SystemTime,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        prost_types::Timestamp::from(*time)
            .to_string()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SystemTime, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse::<prost_types::Timestamp>()
            .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))?
            .try_into()
            .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))
    }
}
mod optional_time {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        time: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        time.map(|v| prost_types::Timestamp::from(v).to_string())
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|v| {
                v.parse::<prost_types::Timestamp>()
                    .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))?
                    .try_into()
                    .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))
            })
            .transpose()
    }
}
