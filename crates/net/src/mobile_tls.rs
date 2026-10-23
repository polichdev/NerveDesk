use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rcgen::{CertificateParams, KeyPair, SanType};
use sha2::{Digest, Sha256};
use tokio_rustls::rustls::{
    self,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use windows::core::w;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

static IDENTITY_LOCK: Mutex<()> = Mutex::new(());

fn protected(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                w!("NerveDesk mobile TLS key"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|e| e.to_string())?;
        let data = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Ok(data)
    }
}

fn identity_paths(ip: std::net::IpAddr) -> Result<(PathBuf, PathBuf), String> {
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?;
    let folder = PathBuf::from(appdata).join("NerveDesk");
    let id = ip.to_string().replace(':', "_");
    Ok((
        folder.join(format!("mobile-{id}.der")),
        folder.join(format!("mobile-{id}.key")),
    ))
}

fn identity(ip: std::net::IpAddr) -> Result<(Vec<u8>, Vec<u8>), String> {
    let _guard = IDENTITY_LOCK.lock().map_err(|e| e.to_string())?;
    let (cert_path, key_path) = identity_paths(ip)?;
    if let (Ok(cert), Ok(encrypted_key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        if let Ok(key) = protected(&encrypted_key, false) {
            return Ok((cert, key));
        }
    }
    let mut params = CertificateParams::new(vec!["localhost".into()]).map_err(|e| e.to_string())?;
    params.subject_alt_names.push(SanType::IpAddress(ip));
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    let cert_der = cert.der().to_vec();
    let key_der = key.serialize_der();
    std::fs::create_dir_all(cert_path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(cert_path, &cert_der).map_err(|e| e.to_string())?;
    std::fs::write(key_path, protected(&key_der, true)?).map_err(|e| e.to_string())?;
    Ok((cert_der, key_der))
}

pub fn server_config(ip: std::net::IpAddr) -> Result<Arc<rustls::ServerConfig>, String> {
    let (cert, key) = identity(ip)?;
    let provider = rustls::crypto::ring::default_provider();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key)),
        )
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

pub fn fingerprint(ip: std::net::IpAddr) -> Result<String, String> {
    let (cert, _) = identity(ip)?;
    let hash = Sha256::digest(cert);
    Ok(hash
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}
