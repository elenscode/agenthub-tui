use crate::operations::check_path;
use anyhow::{ensure, Context, Result};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub trait Protection {
    fn seal(&self, bytes: &[u8]) -> Result<Vec<u8>>;
    fn open(&self, bytes: &[u8]) -> Result<Vec<u8>>;
}

const MAGIC: &[u8] = b"AHB\x01";
pub struct UserProtection {
    state: PathBuf,
}
impl UserProtection {
    pub fn new(state: &Path) -> Self {
        Self {
            state: state.into(),
        }
    }
    fn key(&self, create: bool) -> Result<XChaCha20Poly1305> {
        let dir = self.state.join("keys");
        check_path(&dir)?;
        if create {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
        }
        let owner = unsafe { libc::geteuid() };
        let meta = fs::symlink_metadata(&dir)?;
        ensure!(
            meta.is_dir() && meta.uid() == owner && meta.mode() & 0o077 == 0,
            "Backup key directory must be private to the current user"
        );
        let path = dir.join("backup.key");
        let open = || {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
        };
        let mut file = match open() {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound && create => {
                let backups = self.state.join("backups");
                if backups.try_exists()? {
                    for entry in fs::read_dir(backups)? {
                        ensure!(
                            entry?.path().extension().is_none_or(|x| x != "ahb"),
                            "Existing snapshots require the original backup.key"
                        );
                    }
                }
                let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
                tmp.write_all(&XChaCha20Poly1305::generate_key(&mut OsRng))?;
                tmp.as_file().sync_all()?;
                match tmp.persist_noclobber(&path) {
                    Ok(file) => {
                        File::open(&dir)?.sync_all()?;
                        drop(file);
                    }
                    Err(e) if e.error.kind() == ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(e.into()),
                }
                open()?
            }
            Err(e) => return Err(e).context("Cannot open the user's backup.key"),
        };
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == owner
                && meta.mode() & 0o077 == 0
                && meta.nlink() == 1
                && meta.len() == 32,
            "Invalid backup key or file permissions"
        );
        let mut key = [0; 32];
        file.read_exact(&mut key)?;
        Ok(XChaCha20Poly1305::new((&key).into()))
    }
}
impl Protection for UserProtection {
    fn seal(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.key(true)?;
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&nonce);
        out.extend(
            cipher
                .encrypt(&nonce, bytes)
                .map_err(|_| anyhow::anyhow!("Backup encryption failed"))?,
        );
        Ok(out)
    }
    fn open(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let body = bytes.strip_prefix(MAGIC).context("Unknown backup format")?;
        let (nonce, sealed) = body.split_at_checked(24).context("Incomplete backup")?;
        self.key(false)?
            .decrypt(XNonce::from_slice(nonce), sealed)
            .map_err(|_| anyhow::anyhow!("Backup authentication failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn backup_key_persists_and_requires_private_permissions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let protection = UserProtection::new(dir.path());
        let sealed = protection.seal(b"snapshot")?;
        assert_ne!(sealed, protection.seal(b"snapshot")?);
        let reopened = UserProtection::new(dir.path());
        assert_eq!(reopened.open(&sealed)?, b"snapshot");
        let keys = dir.path().join("keys");
        let path = keys.join("backup.key");
        assert_eq!(path.metadata()?.mode() & 0o777, 0o600);
        assert_eq!(keys.metadata()?.mode() & 0o777, 0o700);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        assert!(reopened.open(&sealed).is_err());
        let old = dir.path().join("old-key");
        fs::rename(&path, &old)?;
        symlink(&old, &path)?;
        assert!(reopened.seal(b"next").is_err());
        fs::remove_file(&path)?;
        let backups = dir.path().join("backups");
        fs::create_dir(&backups)?;
        fs::write(backups.join("snapshot.ahb"), &sealed)?;
        assert!(reopened.open(&sealed).is_err());
        assert!(reopened.seal(b"next").is_err());
        assert!(!path.exists());
        Ok(())
    }
}
