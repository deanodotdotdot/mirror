//! Narrow descriptor-relative filesystem primitives shared by private stores.
//! No credentials, cache schemas, lock ordering or record access live here.
use super::{ClaudeError, ClaudeErrorKind};
use nix::fcntl::{open, openat, OFlag};
use nix::sys::stat::{mkdirat, Mode};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
fn storage_error() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::CredentialStorageError)
}
pub(super) fn open_directory(path: &Path, create: bool) -> Result<Option<File>, ClaudeError> {
    let fd = open(
        Path::new("/"),
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| storage_error())?;
    // SAFETY: open returned a new, owned descriptor.
    let mut directory = unsafe { File::from_raw_fd(fd) };
    let parts: Vec<_> = path
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

pub(super) fn check_directory(file: &File, private: bool) -> Result<(), ClaudeError> {
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

pub(super) enum OpenError {
    Missing,
    Unsafe,
}

pub(super) fn private_open(directory: &File, name: &str, flags: OFlag) -> Result<File, OpenError> {
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
