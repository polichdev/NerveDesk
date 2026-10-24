use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

fn key_path() -> Result<PathBuf, String> {
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?;
    Ok(PathBuf::from(appdata)
        .join("NerveDesk")
        .join("pairing-key.bin"))
}

fn protect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            w!("NerveDesk pairing key"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| e.to_string())?;
        let data = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(data)
    }
}

fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| e.to_string())?;
        let data = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(data)
    }
}

pub fn save_key(key: &str) -> Result<(), String> {
    let path = key_path()?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(path, protect(key.as_bytes())?).map_err(|e| e.to_string())
}

pub fn load_key() -> Result<Option<String>, String> {
    let path = key_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let encrypted = std::fs::read(path).map_err(|e| e.to_string())?;
    String::from_utf8(unprotect(&encrypted)?)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn clear_key() -> Result<(), String> {
    let path = key_path()?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
