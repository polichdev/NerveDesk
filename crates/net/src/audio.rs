use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

const PORT: u16 = 48107;
const CONTROL_PORT: u16 = 48109;
const MAX_FORMAT_BYTES: usize = std::mem::size_of::<WAVEFORMATEXTENSIBLE>();
const MAX_PACKET_BYTES: usize = 512 * 1024;

fn audio_packet_context(sequence: u64) -> [u8; 20] {
    let mut context = [0u8; 20];
    context[..12].copy_from_slice(b"audio-packet");
    context[12..].copy_from_slice(&sequence.to_be_bytes());
    context
}

#[derive(Clone, Serialize)]
pub struct AudioStatus {
    pub enabled: bool,
    pub sending: bool,
    pub receiving: bool,
    pub target: Option<String>,
    pub requested_from: Option<String>,
    pub error: Option<String>,
}

pub struct AudioBridge {
    enabled: Arc<AtomicBool>,
    sending: Arc<AtomicBool>,
    receiving: Arc<AtomicBool>,
    receiver_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    listener_started: AtomicBool,
    control_started: AtomicBool,
    sender_worker: Mutex<Option<thread::JoinHandle<()>>>,
    paired_peer_ip: Arc<Mutex<Option<String>>>,
    security_key: Arc<Mutex<Option<String>>>,
    control_replay: Mutex<crate::secure_input::ReplayGuard>,
    target: Arc<Mutex<Option<String>>>,
    requested_from: Mutex<Option<String>>,
    error: Arc<Mutex<Option<String>>>,
}

impl AudioBridge {
    pub fn new(
        paired_peer_ip: Arc<Mutex<Option<String>>>,
        security_key: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            sending: Arc::new(AtomicBool::new(false)),
            receiving: Arc::new(AtomicBool::new(false)),
            receiver_cancel: Arc::new(Mutex::new(None)),
            listener_started: AtomicBool::new(false),
            control_started: AtomicBool::new(false),
            sender_worker: Mutex::new(None),
            paired_peer_ip,
            security_key,
            control_replay: Mutex::new(crate::secure_input::ReplayGuard::new()),
            target: Arc::new(Mutex::new(None)),
            requested_from: Mutex::new(None),
            error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn status(&self) -> AudioStatus {
        AudioStatus {
            enabled: self.enabled.load(Ordering::SeqCst),
            sending: self.sending.load(Ordering::SeqCst),
            receiving: self.receiving.load(Ordering::SeqCst),
            target: self.target.lock().unwrap().clone(),
            requested_from: self.requested_from.lock().unwrap().clone(),
            error: self.error.lock().unwrap().clone(),
        }
    }

    pub fn set_enabled(self: &Arc<Self>, enabled: bool) -> Result<AudioStatus, String> {
        if enabled && !self.listener_started.load(Ordering::SeqCst) {
            let listener =
                TcpListener::bind(("0.0.0.0", PORT)).map_err(|e| format!("Audio receiver: {e}"))?;
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            self.listener_started.store(true, Ordering::SeqCst);
            let active = self.enabled.clone();
            let receiving = self.receiving.clone();
            let error = self.error.clone();
            let paired_peer_ip = self.paired_peer_ip.clone();
            let security_key = self.security_key.clone();
            let receiver_cancel = self.receiver_cancel.clone();
            thread::spawn(move || loop {
                match listener.accept() {
                    Ok((mut stream, peer)) if active.load(Ordering::SeqCst) => {
                        let paired = paired_peer_ip.lock().ok().and_then(|ip| ip.clone());
                        if paired.as_deref() != Some(peer.ip().to_string().as_str()) {
                            continue;
                        }
                        if receiving.swap(true, Ordering::SeqCst) {
                            continue;
                        }
                        let stream_active = Arc::new(AtomicBool::new(true));
                        *receiver_cancel.lock().unwrap() = Some(stream_active.clone());
                        if !active.load(Ordering::SeqCst) || stream.write_all(b"OK").is_err() {
                            stream_active.store(false, Ordering::SeqCst);
                            receiving.store(false, Ordering::SeqCst);
                            continue;
                        }
                        let receiving = receiving.clone();
                        let error = error.clone();
                        let key = security_key.lock().ok().and_then(|key| key.clone());
                        let Some(key) = key else {
                            receiving.store(false, Ordering::SeqCst);
                            continue;
                        };
                        thread::spawn(move || {
                            if let Err(e) = receive(stream, &stream_active, &key) {
                                if e != "Audio connection closed" && e != "Audio stream stopped" {
                                    *error.lock().unwrap() = Some(e);
                                }
                            }
                            receiving.store(false, Ordering::SeqCst);
                        });
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        *error.lock().unwrap() = Some(format!("Audio receiver: {e}"));
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            });
        }
        if enabled && !self.control_started.load(Ordering::SeqCst) {
            let listener = TcpListener::bind(("0.0.0.0", CONTROL_PORT))
                .map_err(|e| format!("Audio control: {e}"))?;
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            self.control_started.store(true, Ordering::SeqCst);
            let bridge = Arc::clone(self);
            thread::spawn(move || loop {
                match listener.accept() {
                    Ok((mut stream, peer)) => {
                        let paired = bridge.paired_peer_ip.lock().ok().and_then(|ip| ip.clone());
                        if !bridge.enabled.load(Ordering::SeqCst)
                            || paired.as_deref() != Some(peer.ip().to_string().as_str())
                        {
                            continue;
                        }
                        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
                        let key = bridge.security_key.lock().ok().and_then(|key| key.clone());
                        let Some(key) = key else { continue };
                        let mut encrypted_command = [0u8; 32];
                        if stream.read_exact(&mut encrypted_command).is_err() {
                            continue;
                        }
                        let command = bridge.control_replay.lock().ok().and_then(|mut replay| {
                            replay
                                .open_payload(&key, b"audio-control", &encrypted_command, 4)
                                .ok()
                        });
                        let result = match command.as_deref() {
                            Some(b"PULL") => bridge.start(peer.ip().to_string()).map(|_| ()),
                            Some(b"STOP") => {
                                bridge.stop();
                                Ok(())
                            }
                            _ => Err("Unknown audio command".into()),
                        };
                        let _ = stream.write_all(if result.is_ok() { b"OK" } else { b"NO" });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(100))
                    }
                    Err(e) => {
                        *bridge.error.lock().unwrap() = Some(format!("Audio control: {e}"));
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            });
        }
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.stop();
            self.stop_requested();
            if let Some(cancel) = self.receiver_cancel.lock().unwrap().take() {
                cancel.store(false, Ordering::SeqCst);
            }
        }
        *self.error.lock().unwrap() = None;
        Ok(self.status())
    }

    pub fn start(&self, target: String) -> Result<AudioStatus, String> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err("Enable Audio Bridge first".into());
        }
        if self
            .paired_peer_ip
            .lock()
            .ok()
            .and_then(|ip| ip.clone())
            .as_deref()
            != Some(target.as_str())
        {
            return Err("Audio target is not the paired device".into());
        }
        if self.sending.swap(true, Ordering::SeqCst) {
            return Err("Audio stream is already running".into());
        }
        if let Some(worker) = self.sender_worker.lock().unwrap().take() {
            let _ = worker.join();
        }
        let address = match format!("{target}:{PORT}").parse() {
            Ok(address) => address,
            Err(e) => {
                self.sending.store(false, Ordering::SeqCst);
                return Err(format!("Invalid target: {e}"));
            }
        };
        let mut socket = match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            Ok(socket) => socket,
            Err(e) => {
                self.sending.store(false, Ordering::SeqCst);
                return Err(format!("Audio target unavailable: {e}"));
            }
        };
        socket.set_write_timeout(Some(Duration::from_secs(2))).ok();
        socket.set_read_timeout(Some(Duration::from_secs(3))).ok();
        let mut ready = [0; 2];
        if socket.read_exact(&mut ready).is_err() || &ready != b"OK" {
            self.sending.store(false, Ordering::SeqCst);
            return Err("Audio receiver is disabled or unavailable".into());
        }
        *self.target.lock().unwrap() = Some(target);
        *self.error.lock().unwrap() = None;
        let sending = self.sending.clone();
        let enabled = self.enabled.clone();
        let error = self.error.clone();
        let key = self
            .security_key
            .lock()
            .ok()
            .and_then(|key| key.clone())
            .ok_or_else(|| "Pairing key is required for audio".to_string())?;
        let worker = thread::spawn(move || {
            if let Err(e) = capture_and_send(socket, &enabled, &sending, &key) {
                if sending.load(Ordering::SeqCst) {
                    *error.lock().unwrap() = Some(e);
                }
            }
            sending.store(false, Ordering::SeqCst);
        });
        *self.sender_worker.lock().unwrap() = Some(worker);
        Ok(self.status())
    }

    pub fn stop(&self) -> AudioStatus {
        self.sending.store(false, Ordering::SeqCst);
        if let Some(worker) = self.sender_worker.lock().unwrap().take() {
            let _ = worker.join();
        }
        self.status()
    }

    pub fn request_from(&self, target: String) -> Result<AudioStatus, String> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err("Enable Audio Bridge first".into());
        }
        if self
            .paired_peer_ip
            .lock()
            .ok()
            .and_then(|ip| ip.clone())
            .as_deref()
            != Some(target.as_str())
        {
            return Err("Audio target is not the paired device".into());
        }
        if self.requested_from.lock().unwrap().is_some() {
            return Err("Audio request is already active".into());
        }
        let key = self
            .security_key
            .lock()
            .ok()
            .and_then(|key| key.clone())
            .ok_or_else(|| "Pairing key is required for audio".to_string())?;
        audio_control(&target, b"PULL", &key)?;
        *self.requested_from.lock().unwrap() = Some(target);
        Ok(self.status())
    }

    pub fn stop_requested(&self) -> AudioStatus {
        if let Some(target) = self.requested_from.lock().unwrap().take() {
            if let Some(key) = self.security_key.lock().ok().and_then(|key| key.clone()) {
                let _ = audio_control(&target, b"STOP", &key);
            }
        }
        self.status()
    }
}

fn audio_control(target: &str, command: &[u8; 4], key: &str) -> Result<(), String> {
    let address = format!("{target}:{CONTROL_PORT}")
        .parse()
        .map_err(|e| format!("Invalid audio target: {e}"))?;
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|e| format!("Audio target unavailable: {e}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(8))).ok();
    socket.set_write_timeout(Some(Duration::from_secs(3))).ok();
    let encrypted = crate::secure_input::seal_payload(key, b"audio-control", command, 4)?;
    socket.write_all(&encrypted).map_err(|e| e.to_string())?;
    let mut response = [0u8; 2];
    socket
        .read_exact(&mut response)
        .map_err(|e| e.to_string())?;
    if &response == b"OK" {
        Ok(())
    } else {
        Err("Remote Audio Bridge could not start streaming".into())
    }
}

struct ComApartment;

impl ComApartment {
    fn init() -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
            .map_err(|e| format!("COM initialization: {e}"))?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn audio_client() -> Result<IAudioClient, String> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("Default audio device: {e}"))?;
        device.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())
    }
}

pub(crate) fn capture_for_mobile(
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    running: Arc<AtomicBool>,
    audio_enabled: Arc<AtomicBool>,
) -> Result<(), String> {
    if !audio_enabled.load(Ordering::SeqCst) {
        return Err("Mobile audio is disabled".into());
    }
    let _com = ComApartment::init()?;
    let client = audio_client()?;
    unsafe {
        let format = client.GetMixFormat().map_err(|e| e.to_string())?;
        let wave = &*format;
        let tag = if wave.wFormatTag == 0xfffe && wave.cbSize >= 22 {
            std::ptr::read_unaligned(format.cast::<u8>().add(24).cast::<u16>())
        } else {
            wave.wFormatTag
        };
        let supported = matches!(
            (tag, wave.wBitsPerSample),
            (3, 32) | (1, 16) | (1, 24) | (1, 32)
        );
        let expected_align = wave.nChannels as usize * (wave.wBitsPerSample as usize / 8);
        let sample_rate = wave.nSamplesPerSec;
        if !supported
            || wave.nChannels == 0
            || wave.nChannels > 8
            || !(8_000..=384_000).contains(&sample_rate)
            || wave.nBlockAlign as usize != expected_align
        {
            CoTaskMemFree(Some(format.cast()));
            return Err("Unsupported Windows audio output format".into());
        }
        let mut header = Vec::with_capacity(16);
        header.extend_from_slice(b"NDA3");
        header.extend_from_slice(&wave.nSamplesPerSec.to_le_bytes());
        header.extend_from_slice(&wave.nChannels.to_le_bytes());
        header.extend_from_slice(&wave.wBitsPerSample.to_le_bytes());
        header.extend_from_slice(&tag.to_le_bytes());
        header.extend_from_slice(&wave.nBlockAlign.to_le_bytes());
        let block_align = wave.nBlockAlign as usize;
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            1_000_000,
            0,
            format,
            None,
        );
        CoTaskMemFree(Some(format.cast()));
        init.map_err(|e| format!("WASAPI loopback: {e}"))?;
        sender
            .blocking_send(header)
            .map_err(|_| "Mobile listener closed".to_string())?;
        let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
        client.Start().map_err(|e| e.to_string())?;
        let result = (|| {
            while running.load(Ordering::SeqCst)
                && audio_enabled.load(Ordering::SeqCst)
                && !sender.is_closed()
            {
                let count = capture.GetNextPacketSize().map_err(|e| e.to_string())?;
                if count == 0 {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| e.to_string())?;
                let bytes = (frames as usize).checked_mul(block_align);
                if bytes.is_none_or(|bytes| bytes > MAX_PACKET_BYTES) {
                    let _ = capture.ReleaseBuffer(frames);
                    return Err("Audio packet is too large".into());
                }
                let bytes = bytes.unwrap();
                let packet = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    vec![0; bytes]
                } else {
                    std::slice::from_raw_parts(data, bytes).to_vec()
                };
                capture.ReleaseBuffer(frames).map_err(|e| e.to_string())?;
                if sender.blocking_send(packet).is_err() {
                    break;
                }
            }
            Ok(())
        })();
        let _ = client.Stop();
        result
    }
}

struct SourceMute {
    volume: IAudioEndpointVolume,
    restore: bool,
}

impl SourceMute {
    fn new() -> Result<Self, String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| e.to_string())?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| format!("Default audio device: {e}"))?;
            let volume: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("Audio volume control: {e}"))?;
            let restore = !volume.GetMute().map_err(|e| e.to_string())?.as_bool();
            if restore {
                volume
                    .SetMute(true, std::ptr::null())
                    .map_err(|e| format!("Mute source audio: {e}"))?;
            }
            Ok(Self { volume, restore })
        }
    }
}

impl Drop for SourceMute {
    fn drop(&mut self) {
        if self.restore {
            unsafe {
                if self.volume.GetMute().is_ok_and(|muted| muted.as_bool()) {
                    let _ = self.volume.SetMute(false, std::ptr::null());
                }
            }
        }
    }
}

fn capture_and_send(
    mut socket: TcpStream,
    enabled: &AtomicBool,
    sending: &AtomicBool,
    key: &str,
) -> Result<(), String> {
    let _com = ComApartment::init()?;
    let client = audio_client()?;
    unsafe {
        let format = client.GetMixFormat().map_err(|e| e.to_string())?;
        let length = 18usize + (*format).cbSize as usize;
        if length > MAX_FORMAT_BYTES || (*format).nBlockAlign == 0 {
            CoTaskMemFree(Some(format.cast()));
            return Err("Unsupported audio mix format".into());
        }
        let format_bytes = std::slice::from_raw_parts(format.cast::<u8>(), length).to_vec();
        let block_align = (*format).nBlockAlign as usize;
        let result = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            1_000_000,
            0,
            format,
            None,
        );
        CoTaskMemFree(Some(format.cast()));
        result.map_err(|e| format!("WASAPI loopback: {e}"))?;
        socket.write_all(b"NDA2").map_err(|e| e.to_string())?;
        let encrypted_format = crate::secure_input::seal_payload(
            key,
            b"audio-format",
            &format_bytes,
            MAX_FORMAT_BYTES,
        )?;
        socket
            .write_all(&(encrypted_format.len() as u16).to_le_bytes())
            .map_err(|e| e.to_string())?;
        socket
            .write_all(&encrypted_format)
            .map_err(|e| e.to_string())?;
        let mut ready = [0; 1];
        socket
            .read_exact(&mut ready)
            .map_err(|e| format!("Audio receiver rejected stream: {e}"))?;
        if ready[0] != b'Y' {
            return Err("Receiver does not support this audio format".into());
        }
        let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
        client.Start().map_err(|e| e.to_string())?;
        let source_mute = match SourceMute::new() {
            Ok(mute) => mute,
            Err(e) => {
                let _ = client.Stop();
                return Err(e);
            }
        };
        let result = (|| {
            let mut sequence = 0u64;
            while enabled.load(Ordering::SeqCst) && sending.load(Ordering::SeqCst) {
                let count = capture.GetNextPacketSize().map_err(|e| e.to_string())?;
                if count == 0 {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| e.to_string())?;
                let bytes = frames as usize * block_align;
                let packet = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    vec![0; bytes]
                } else {
                    std::slice::from_raw_parts(data, bytes).to_vec()
                };
                capture.ReleaseBuffer(frames).map_err(|e| e.to_string())?;
                if packet.len() > MAX_PACKET_BYTES {
                    return Err("Audio packet is too large".into());
                }
                let encrypted = crate::secure_input::seal_payload(
                    key,
                    &audio_packet_context(sequence),
                    &packet,
                    MAX_PACKET_BYTES,
                )?;
                sequence = sequence.checked_add(1).ok_or("Audio sequence overflow")?;
                socket
                    .write_all(&(encrypted.len() as u32).to_le_bytes())
                    .map_err(|e| e.to_string())?;
                socket.write_all(&encrypted).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        let _ = client.Stop();
        drop(source_mute);
        result
    }
}

fn receive(mut socket: TcpStream, enabled: &AtomicBool, key: &str) -> Result<(), String> {
    socket
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok();
    let mut magic = [0; 4];
    read_exact_active(&mut socket, &mut magic, enabled)?;
    if &magic != b"NDA2" {
        return Err("Unknown audio stream format".into());
    }
    let mut size = [0; 2];
    read_exact_active(&mut socket, &mut size, enabled)?;
    let length = u16::from_le_bytes(size) as usize;
    if !(46..=MAX_FORMAT_BYTES + 28).contains(&length) {
        return Err("Invalid audio format".into());
    }
    let mut encrypted_format = vec![0; length];
    read_exact_active(&mut socket, &mut encrypted_format, enabled)?;
    let mut replay = crate::secure_input::ReplayGuard::new();
    let format_bytes =
        replay.open_payload(key, b"audio-format", &encrypted_format, MAX_FORMAT_BYTES)?;
    let length = format_bytes.len();
    if !(18..=MAX_FORMAT_BYTES).contains(&length) {
        return Err("Invalid audio format".into());
    }
    let _com = ComApartment::init()?;
    let client = audio_client()?;
    unsafe {
        let mut format: WAVEFORMATEXTENSIBLE = std::mem::zeroed();
        std::ptr::copy_nonoverlapping(
            format_bytes.as_ptr(),
            (&mut format as *mut WAVEFORMATEXTENSIBLE).cast::<u8>(),
            length,
        );
        let wave = (&format as *const WAVEFORMATEXTENSIBLE).cast::<WAVEFORMATEX>();
        let sample_rate = (*wave).nSamplesPerSec;
        if (*wave).nBlockAlign == 0
            || (*wave).nChannels == 0
            || (*wave).nChannels > 8
            || !(8_000..=192_000).contains(&sample_rate)
            || (*wave).nAvgBytesPerSec == 0
            || length != 18 + (*wave).cbSize as usize
        {
            return Err("Invalid audio block format".into());
        }
        if let Err(e) = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 1_000_000, 0, wave, None) {
            let _ = socket.write_all(b"N");
            return Err(format!("Playback format unsupported: {e}"));
        }
        let buffer_frames = client.GetBufferSize().map_err(|e| e.to_string())?;
        let render: IAudioRenderClient = client.GetService().map_err(|e| e.to_string())?;
        client.Start().map_err(|e| e.to_string())?;
        socket.write_all(b"Y").map_err(|e| e.to_string())?;
        let result = (|| {
            let mut sequence = 0u64;
            while enabled.load(Ordering::SeqCst) {
                let mut len_bytes = [0; 4];
                read_exact_active(&mut socket, &mut len_bytes, enabled)?;
                let len = u32::from_le_bytes(len_bytes) as usize;
                if len <= 28 || len > MAX_PACKET_BYTES + 28 {
                    return Err("Invalid audio packet".into());
                }
                let mut encrypted = vec![0; len];
                read_exact_active(&mut socket, &mut encrypted, enabled)?;
                let packet = replay.open_payload(
                    key,
                    &audio_packet_context(sequence),
                    &encrypted,
                    MAX_PACKET_BYTES,
                )?;
                sequence = sequence.checked_add(1).ok_or("Audio sequence overflow")?;
                let len = packet.len();
                if len == 0 || len % (*wave).nBlockAlign as usize != 0 {
                    return Err("Invalid audio packet".into());
                }
                let mut offset = 0;
                while offset < len && enabled.load(Ordering::SeqCst) {
                    let padding = client.GetCurrentPadding().map_err(|e| e.to_string())?;
                    let available = buffer_frames.saturating_sub(padding) as usize;
                    let frames = ((len - offset) / (*wave).nBlockAlign as usize).min(available);
                    if frames == 0 {
                        thread::sleep(Duration::from_millis(3));
                        continue;
                    }
                    let bytes = frames * (*wave).nBlockAlign as usize;
                    let output = render.GetBuffer(frames as u32).map_err(|e| e.to_string())?;
                    std::ptr::copy_nonoverlapping(packet.as_ptr().add(offset), output, bytes);
                    render
                        .ReleaseBuffer(frames as u32, 0)
                        .map_err(|e| e.to_string())?;
                    offset += bytes;
                }
            }
            Ok(())
        })();
        let _ = client.Stop();
        result
    }
}

fn read_exact_active(
    socket: &mut TcpStream,
    buffer: &mut [u8],
    enabled: &AtomicBool,
) -> Result<(), String> {
    let mut offset = 0;
    while offset < buffer.len() && enabled.load(Ordering::SeqCst) {
        match socket.read(&mut buffer[offset..]) {
            Ok(0) => return Err("Audio connection closed".into()),
            Ok(n) => offset += n,
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if offset == buffer.len() {
        Ok(())
    } else {
        Err("Audio stream stopped".into())
    }
}
