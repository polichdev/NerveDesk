use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::AppState;
use net::secure_input::{seal_payload, ReplayGuard};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::warn;

pub const PAIRING_PORT: u16 = 48112;
const PORT: u16 = PAIRING_PORT;
const MAX_NAME: usize = 255;
const CHALLENGE_LEN: usize = 12;
const MAX_REQUEST: usize = MAX_NAME + CHALLENGE_LEN;

pub fn generate_random_security_key() -> String {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789!@#$%^&*";
    let mut bytes = [0u8; 24];
    unsafe {
        let _ = BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
    bytes.iter().map(|&b| CHARS[(b as usize) % CHARS.len()] as char).collect()
}

async fn read_packet<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Vec<u8>, String> {
    let len = stream.read_u16().await.map_err(|e| e.to_string())? as usize;
    if len == 0 {
        let reason = stream.read_u8().await.map_err(|e| e.to_string())?;
        return Err(match reason {
            1 => "The security code does not match the other PC".into(),
            2 => "The other PC is already paired with another device".into(),
            3 => "The pairing request is stale; retry with the latest build on both PCs".into(),
            _ => "The other PC rejected the pairing request".into(),
        });
    }
    if !(1..=MAX_REQUEST + 28).contains(&len) {
        return Err("Invalid pairing packet length".into());
    }
    let mut packet = vec![0; len];
    stream
        .read_exact(&mut packet)
        .await
        .map_err(|e| e.to_string())?;
    Ok(packet)
}

async fn write_packet<W: AsyncWrite + Unpin>(stream: &mut W, packet: &[u8]) -> Result<(), String> {
    stream
        .write_u16(packet.len() as u16)
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(packet).await.map_err(|e| e.to_string())
}

async fn read_pair_response<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Vec<u8>, String> {
    let len = stream
        .read_u16()
        .await
        .map_err(|e| format!("Reading pairing response: {e}"))? as usize;
    if len == 0 {
        let reason = stream.read_u8().await.map_err(|e| e.to_string())?;
        return Err(match reason {
            1 => "The security code does not match the other PC".into(),
            2 => "The other PC is already paired with another device".into(),
            3 => "The pairing request is stale; retry with the latest build on both PCs".into(),
            _ => "The other PC rejected the pairing request".into(),
        });
    }
    if !(28..=MAX_REQUEST + 28).contains(&len) {
        return Err("Incompatible pairing protocol; install the same build on both PCs".into());
    }
    let mut packet = vec![0; len];
    stream
        .read_exact(&mut packet)
        .await
        .map_err(|e| e.to_string())?;
    Ok(packet)
}

async fn write_rejection<W: AsyncWrite + Unpin>(stream: &mut W, error: &str) {
    let reason = if error.contains("authentication failed") {
        1
    } else if error == "Another device is already paired" {
        2
    } else if error == "Stale pairing request" {
        3
    } else {
        4
    };
    let _ = stream.write_all(&[0, 0, reason]).await;
}

pub async fn request_pair(ip: &str, key: &str, my_name: &str) -> Result<(), String> {
    let address: SocketAddr = format!("{ip}:{PORT}")
        .parse()
        .map_err(|e| format!("Invalid address: {e}"))?;
    let mut stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(address))
        .await
        .map_err(|_| "Pairing connection timed out".to_string())?
        .map_err(|e| format!("Cannot reach pairing service: {e}"))?;
    tokio::time::timeout(Duration::from_secs(4), async {
        let challenge = read_packet(&mut stream).await?;
        if challenge.len() != CHALLENGE_LEN {
            return Err("Invalid challenge received".into());
        }
        let mut payload = challenge;
        payload.extend_from_slice(my_name.as_bytes());
        let request = seal_payload(key, b"pair-request", &payload, MAX_REQUEST)?;
        write_packet(&mut stream, &request).await?;
        let reply = read_pair_response(&mut stream).await?;
        let answer = ReplayGuard::new().open_payload(key, b"pair-response", &reply, MAX_NAME)?;
        if answer != b"paired" {
            return Err("Pairing was not accepted".into());
        }
        Ok(())
    })
    .await
    .map_err(|_| "Pairing response timed out".to_string())?
}

pub fn start_listener(state: &AppState) {
    let key = state.paired_security_key.clone();
    let peer_ip = state.paired_peer_ip.clone();
    let peer_name = state.peer_device.clone();
    let seen = state.authenticated_peer_seen.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind(("0.0.0.0", PORT)).await {
            Ok(listener) => listener,
            Err(e) => {
                warn!("Pairing listener: {e}");
                return;
            }
        };
        loop {
            let Ok((mut stream, address)) = listener.accept().await else {
                continue;
            };
            let key = key.clone();
            let peer_ip = peer_ip.clone();
            let peer_name = peer_name.clone();
            let seen = seen.clone();
            tauri::async_runtime::spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(5),
                    accept_pair(&mut stream, address, key, peer_ip, peer_name, seen),
                )
                .await;
                if let Ok(Err(error)) = result {
                    warn!("Pairing request from {address}: {error}");
                    write_rejection(&mut stream, &error).await;
                }
            });
        }
    });
}

async fn accept_pair<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    address: SocketAddr,
    key: Arc<Mutex<Option<String>>>,
    peer_ip: Arc<Mutex<Option<String>>>,
    peer_name: Arc<Mutex<Option<String>>>,
    seen: Arc<Mutex<Option<Instant>>>,
) -> Result<(), String> {
    let local_key = key
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("Pairing code is not ready")?;
    let challenge_packet = seal_payload(&local_key, b"pair-challenge", b"", 0)?;
    let challenge = &challenge_packet[..CHALLENGE_LEN];
    write_packet(stream, challenge).await?;
    let request = read_packet(stream).await?;
    let payload =
        ReplayGuard::new().open_payload(&local_key, b"pair-request", &request, MAX_REQUEST)?;
    if payload.len() < CHALLENGE_LEN || &payload[..CHALLENGE_LEN] != challenge {
        return Err("Stale pairing request".into());
    }
    let name = String::from_utf8(payload[CHALLENGE_LEN..].to_vec()).map_err(|e| e.to_string())?;
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_NAME {
        return Err("Invalid device name".into());
    }
    let ip = address.ip().to_string();
    if peer_ip
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .is_some_and(|existing| existing != &ip)
    {
        return Err("Another device is already paired".into());
    }
    let reply = seal_payload(&local_key, b"pair-response", b"paired", MAX_NAME)?;
    *peer_ip.lock().map_err(|e| e.to_string())? = Some(ip);
    *peer_name.lock().map_err(|e| e.to_string())? = Some(name.to_string());
    *seen.lock().map_err(|e| e.to_string())? = Some(Instant::now());
    #[cfg(not(test))]
    let _ = crate::secret_store::save_key(&local_key);
    write_packet(stream, &reply).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn receiver_accepts_only_the_displayed_code() {
        let address: SocketAddr = "127.0.0.1:41000".parse().unwrap();
        let key = Arc::new(Mutex::new(Some("a-long-secret-pairing-code".to_string())));
        let ip = Arc::new(Mutex::new(None));
        let name = Arc::new(Mutex::new(None));
        let seen = Arc::new(Mutex::new(None));
        let mut valid_packet = None;

        for (candidate, accepted) in [
            ("incorrect-pairing-code", false),
            ("a-long-secret-pairing-code", true),
        ] {
            let (mut client, mut server) = tokio::io::duplex(1024);
            let (server_key, server_ip, server_name, server_seen) =
                (key.clone(), ip.clone(), name.clone(), seen.clone());
            let server_task = tokio::spawn(async move {
                let result = accept_pair(
                    &mut server,
                    address,
                    server_key,
                    server_ip,
                    server_name,
                    server_seen,
                )
                .await;
                if let Err(ref error) = result {
                    write_rejection(&mut server, error).await;
                }
                result
            });
            let challenge = read_packet(&mut client).await.unwrap();
            let mut payload = challenge;
            payload.extend_from_slice(b"My-PC");
            let packet = seal_payload(candidate, b"pair-request", &payload, MAX_REQUEST).unwrap();
            if accepted {
                valid_packet = Some(packet.clone());
            }
            write_packet(&mut client, &packet).await.unwrap();
            let result = server_task.await.unwrap();
            assert_eq!(result.is_ok(), accepted);
            if accepted {
                let reply = read_pair_response(&mut client).await.unwrap();
                assert_eq!(
                    ReplayGuard::new()
                        .open_payload(candidate, b"pair-response", &reply, MAX_NAME)
                        .unwrap(),
                    b"paired"
                );
                assert_eq!(name.lock().unwrap().as_deref(), Some("My-PC"));
                assert_eq!(ip.lock().unwrap().as_deref(), Some("127.0.0.1"));
            } else {
                assert!(ip.lock().unwrap().is_none());
                assert_eq!(
                    read_pair_response(&mut client).await.unwrap_err(),
                    "The security code does not match the other PC"
                );
            }
        }

        let (mut client, mut server) = tokio::io::duplex(1024);
        let server_task =
            tokio::spawn(
                async move { accept_pair(&mut server, address, key, ip, name, seen).await },
            );
        let _ = read_packet(&mut client).await.unwrap();
        write_packet(&mut client, &valid_packet.unwrap())
            .await
            .unwrap();
        assert_eq!(
            server_task.await.unwrap().unwrap_err(),
            "Stale pairing request"
        );

        let (mut client, mut server) = tokio::io::duplex(1024);
        let other_address: SocketAddr = "127.0.0.2:41000".parse().unwrap();
        let (key, ip, name, seen) = (
            Arc::new(Mutex::new(Some("a-long-secret-pairing-code".to_string()))),
            Arc::new(Mutex::new(Some("127.0.0.1".to_string()))),
            Arc::new(Mutex::new(Some("My-PC".to_string()))),
            Arc::new(Mutex::new(None)),
        );
        let server_task = tokio::spawn(async move {
            let result = accept_pair(&mut server, other_address, key, ip, name, seen).await;
            if let Err(ref error) = result {
                write_rejection(&mut server, error).await;
            }
            result
        });
        let challenge = read_packet(&mut client).await.unwrap();
        let mut payload = challenge;
        payload.extend_from_slice(b"My-PC");
        let packet = seal_payload(
            "a-long-secret-pairing-code",
            b"pair-request",
            &payload,
            MAX_REQUEST,
        )
        .unwrap();
        write_packet(&mut client, &packet).await.unwrap();
        assert_eq!(
            server_task.await.unwrap().unwrap_err(),
            "Another device is already paired"
        );
        assert_eq!(
            read_pair_response(&mut client).await.unwrap_err(),
            "The other PC is already paired with another device"
        );
    }
}
