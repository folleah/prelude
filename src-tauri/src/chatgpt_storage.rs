#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Renewal {
    pub client_id: String,
    pub refresh_token: String,
}

#[cfg(windows)]
mod protected {
    use super::Renewal;
    use std::{
        fs, io,
        path::{Path, PathBuf},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };

    const FILE: &str = "chatgpt-renewal.dat";

    fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len().try_into().map_err(|_| "The session is too large.")?,
            pbData: bytes.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: ptr::null_mut(),
        };
        // DPAPI allocates the output buffer. It is copied and released with LocalFree.
        unsafe {
            let result = if encrypt {
                CryptProtectData(
                    &input,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            };
            if result == 0 || output.pbData.is_null() {
                return Err("Windows could not process the saved credentials.".into());
            }
            let data = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            ptr::write_bytes(output.pbData, 0, output.cbData as usize);
            LocalFree(output.pbData.cast());
            Ok(data)
        }
    }

    pub(crate) fn load_secret<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<Option<T>, String> {
        let data = match fs::read(dir.join(name)) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Could not read the saved credentials.".into()),
        };
        let bytes = protect(&data, false)?;
        serde_json::from_slice(&bytes).map(Some).map_err(|_| "The saved credentials are corrupted.".into())
    }

    pub(crate) fn save_secret<T: serde::Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
        use std::io::Write;
        let error = |_| "Could not save the protected credentials.".to_string();
        let data = protect(&serde_json::to_vec(value).map_err(|_| "Could not prepare the credentials.")?, true)?;
        fs::create_dir_all(dir).map_err(error)?;
        let temporary: PathBuf = dir.join(format!("credential-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> io::Result<()> {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            file.write_all(&data)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, dir.join(name))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result.map_err(error)
    }

    pub(crate) fn delete_secret(dir: &Path, name: &str) -> Result<(), String> {
        match fs::remove_file(dir.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Could not delete the saved credentials.".into()),
        }
    }

    pub(crate) fn load(dir: &Path) -> Result<Option<Renewal>, String> { load_secret(dir, FILE) }
    pub(crate) fn save(dir: &Path, renewal: &Renewal) -> Result<(), String> { save_secret(dir, FILE, renewal) }
    pub(crate) fn delete(dir: &Path) -> Result<(), String> { delete_secret(dir, FILE) }

    #[cfg(test)]
    #[test]
    fn protects_replaces_and_deletes_saved_session() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let mut session = Renewal {
            client_id: "test-client".into(),
            refresh_token: "first-test-secret".into(),
        };
        assert!(load(&dir).unwrap().is_none());
        save(&dir, &session).unwrap();
        let bytes = fs::read(dir.join(FILE)).unwrap();
        assert!(!bytes
            .windows(session.refresh_token.len())
            .any(|part| part == session.refresh_token.as_bytes()));
        session.refresh_token = "rotated-test-secret".into();
        save(&dir, &session).unwrap();
        assert_eq!(
            load(&dir).unwrap().unwrap().refresh_token,
            session.refresh_token
        );
        delete(&dir).unwrap();
        assert!(load(&dir).unwrap().is_none());
        fs::remove_dir(dir).unwrap();
    }
}

#[cfg(windows)]
pub(crate) use protected::{delete, delete_secret, load, load_secret, save, save_secret};

#[cfg(not(windows))]
pub(crate) fn load(_: &std::path::Path) -> Result<Option<Renewal>, String> {
    Ok(None)
}
#[cfg(not(windows))]
pub(crate) fn save(_: &std::path::Path, _: &Renewal) -> Result<(), String> {
    Ok(())
}
#[cfg(not(windows))]
pub(crate) fn delete(_: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn load_secret<T>(_: &std::path::Path, _: &str) -> Result<Option<T>, String> { Ok(None) }
#[cfg(not(windows))]
pub(crate) fn save_secret<T>(_: &std::path::Path, _: &str, _: &T) -> Result<(), String> { Ok(()) }
#[cfg(not(windows))]
pub(crate) fn delete_secret(_: &std::path::Path, _: &str) -> Result<(), String> { Ok(()) }
