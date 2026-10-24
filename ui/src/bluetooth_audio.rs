use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use windows::core::HSTRING;
use windows::Devices::Enumeration::DeviceInformation;
use windows::Media::Audio::{
    AudioPlaybackConnection, AudioPlaybackConnectionOpenResultStatus, AudioPlaybackConnectionState,
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[derive(Clone, Serialize)]
pub struct BluetoothAudioDevice {
    id: String,
    name: String,
}

#[derive(Clone, Serialize)]
pub struct BluetoothAudioStatus {
    connected: bool,
    device_name: Option<String>,
    state: String,
}

enum Request {
    List(mpsc::Sender<Result<Vec<BluetoothAudioDevice>, String>>),
    Connect(String, mpsc::Sender<Result<BluetoothAudioStatus, String>>),
    Reconnect(mpsc::Sender<Result<BluetoothAudioStatus, String>>),
    Disconnect(mpsc::Sender<Result<BluetoothAudioStatus, String>>),
    Status(mpsc::Sender<Result<BluetoothAudioStatus, String>>),
}

static WORKER: OnceLock<mpsc::Sender<Request>> = OnceLock::new();

fn worker() -> &'static mpsc::Sender<Request> {
    WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
                .map_err(|e| format!("Windows Bluetooth audio initialization: {e}"));
            let mut active: Option<(BluetoothAudioDevice, AudioPlaybackConnection)> = None;
            while let Ok(request) = receiver.recv() {
                match request {
                    Request::List(reply) => {
                        let result = initialized
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|_| list_devices());
                        let _ = reply.send(result);
                    }
                    Request::Connect(id, reply) => {
                        let result = initialized.as_ref().map_err(Clone::clone).and_then(|_| {
                            if let Some((device, connection)) = active.as_ref() {
                                if device.id == id
                                    && connection.State().ok()
                                        == Some(AudioPlaybackConnectionState::Opened)
                                {
                                    return connection_status(Some((device, connection)));
                                }
                            }
                            if let Some((_, old)) = active.take() {
                                let _ = old.Close();
                            }
                            let device = list_devices()?
                                .into_iter()
                                .find(|device| device.id == id)
                                .ok_or("Pair the phone with this PC in Windows Bluetooth settings first")?;
                            active = Some(connect_device(device)?);
                            connection_status(active.as_ref().map(|(d, c)| (d, c)))
                        });
                        let _ = reply.send(result);
                    }
                    Request::Reconnect(reply) => {
                        let result = initialized.as_ref().map_err(Clone::clone).and_then(|_| {
                            let (device, old) = active
                                .take()
                                .ok_or("No phone audio connection to reconnect")?;
                            let _ = old.Close();
                            drop(old);
                            std::thread::sleep(Duration::from_millis(1500));
                            active = Some(connect_device(device)?);
                            connection_status(active.as_ref().map(|(d, c)| (d, c)))
                        });
                        let _ = reply.send(result);
                    }
                    Request::Disconnect(reply) => {
                        if let Some((_, connection)) = active.take() {
                            let _ = connection.Close();
                        }
                        let _ = reply.send(connection_status(None));
                    }
                    Request::Status(reply) => {
                        let result = initialized
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|_| connection_status(active.as_ref().map(|(d, c)| (d, c))));
                        let _ = reply.send(result);
                    }
                }
            }
            if initialized.is_ok() {
                unsafe { windows::Win32::System::WinRT::RoUninitialize() };
            }
        });
        sender
    })
}

fn connect_device(
    device: BluetoothAudioDevice,
) -> Result<(BluetoothAudioDevice, AudioPlaybackConnection), String> {
    let connection = AudioPlaybackConnection::TryCreateFromId(&HSTRING::from(&device.id))
        .map_err(|e| e.to_string())?;
    connection
        .StartAsync()
        .map_err(|e| e.to_string())?
        .get()
        .map_err(|e| e.to_string())?;
    let opened = connection
        .OpenAsync()
        .map_err(|e| e.to_string())?
        .get()
        .map_err(|e| e.to_string())?;
    let open_status = opened.Status().map_err(|e| e.to_string())?;
    if open_status != AudioPlaybackConnectionOpenResultStatus::Success {
        let _ = connection.Close();
        return Err(
            if open_status == AudioPlaybackConnectionOpenResultStatus::RequestTimedOut {
                "Phone audio connection timed out. Check that Bluetooth is enabled on the phone"
                    .into()
            } else if open_status == AudioPlaybackConnectionOpenResultStatus::DeniedBySystem {
                "Windows denied the phone audio connection".into()
            } else {
                format!(
                    "Phone audio connection failed: {:?}",
                    opened.ExtendedError().ok()
                )
            },
        );
    }
    Ok((device, connection))
}

fn list_devices() -> Result<Vec<BluetoothAudioDevice>, String> {
    let selector = AudioPlaybackConnection::GetDeviceSelector().map_err(|e| e.to_string())?;
    let collection = DeviceInformation::FindAllAsyncAqsFilter(&selector)
        .map_err(|e| e.to_string())?
        .get()
        .map_err(|e| e.to_string())?;
    let mut devices = Vec::new();
    for index in 0..collection.Size().map_err(|e| e.to_string())? {
        let device = collection.GetAt(index).map_err(|e| e.to_string())?;
        devices.push(BluetoothAudioDevice {
            id: device.Id().map_err(|e| e.to_string())?.to_string(),
            name: device.Name().map_err(|e| e.to_string())?.to_string(),
        });
    }
    Ok(devices)
}

fn connection_status(
    active: Option<(&BluetoothAudioDevice, &AudioPlaybackConnection)>,
) -> Result<BluetoothAudioStatus, String> {
    let Some((device, connection)) = active else {
        return Ok(BluetoothAudioStatus {
            connected: false,
            device_name: None,
            state: "disconnected".into(),
        });
    };
    let state = connection.State().map_err(|e| e.to_string())?;
    Ok(BluetoothAudioStatus {
        connected: state == AudioPlaybackConnectionState::Opened,
        device_name: Some(device.name.clone()),
        state: if state == AudioPlaybackConnectionState::Opened {
            "connected"
        } else if state == AudioPlaybackConnectionState::Closed {
            "disconnected"
        } else {
            "connecting"
        }
        .into(),
    })
}

async fn ask<T: Send + 'static>(
    build: impl FnOnce(mpsc::Sender<Result<T, String>>) -> Request + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(move || {
        let (reply, result) = mpsc::channel();
        worker().send(build(reply)).map_err(|e| e.to_string())?;
        result
            .recv_timeout(Duration::from_secs(30))
            .map_err(|e| e.to_string())?
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_bluetooth_audio_devices() -> Result<Vec<BluetoothAudioDevice>, String> {
    ask(Request::List).await
}

#[tauri::command]
pub async fn connect_bluetooth_audio(device_id: String) -> Result<BluetoothAudioStatus, String> {
    ask(move |reply| Request::Connect(device_id, reply)).await
}

#[tauri::command]
pub async fn reconnect_bluetooth_audio() -> Result<BluetoothAudioStatus, String> {
    ask(Request::Reconnect).await
}

#[tauri::command]
pub async fn disconnect_bluetooth_audio() -> Result<BluetoothAudioStatus, String> {
    ask(Request::Disconnect).await
}

#[tauri::command]
pub async fn get_bluetooth_audio_status() -> Result<BluetoothAudioStatus, String> {
    ask(Request::Status).await
}

#[tauri::command]
pub fn open_bluetooth_settings() -> Result<(), String> {
    std::process::Command::new("cmd")
        .args(["/c", "start", "ms-settings:bluetooth"])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn open_sound_settings() -> Result<(), String> {
    std::process::Command::new("cmd")
        .args(["/c", "start", "ms-settings:sound"])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
