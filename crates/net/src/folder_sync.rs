use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};

const PORT: u16 = 48108;
const MAX_FRAME: usize = 16 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024 * 1024;
const MAX_EVENTS: usize = 500;

type Manifest = BTreeMap<String, String>;

#[derive(Debug, PartialEq, Eq)]
enum SyncAction {
    None,
    Upload,
    Download,
    DeleteLocal,
    DeleteRemote,
    ConflictFiles,
    RestoreLocal,
    RestoreRemote,
}

fn plan_action(
    local: Option<&String>,
    remote: Option<&String>,
    baseline: Option<&String>,
) -> SyncAction {
    if local == remote {
        return SyncAction::None;
    }
    if local == baseline {
        return if remote.is_some() {
            SyncAction::Download
        } else {
            SyncAction::DeleteLocal
        };
    }
    if remote == baseline {
        return if local.is_some() {
            SyncAction::Upload
        } else {
            SyncAction::DeleteRemote
        };
    }
    match (local, remote) {
        (Some(_), Some(_)) => SyncAction::ConflictFiles,
        (Some(_), None) => SyncAction::RestoreRemote,
        (None, Some(_)) => SyncAction::RestoreLocal,
        (None, None) => SyncAction::None,
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SyncEvent {
    pub id: String,
    pub time_ms: u64,
    pub path: String,
    pub kind: String,
    pub detail: String,
    pub version_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SyncPair {
    id: String,
    peer_ip: String,
    peer_name: String,
    local_dir: String,
    key_hash: String,
    enabled: bool,
    #[serde(default)]
    approved: bool,
    #[serde(default)]
    incoming: bool,
    #[serde(default)]
    initial_files: BTreeSet<String>,
    #[serde(default)]
    snapshot_ready: bool,
    baseline: Manifest,
    last_sync_ms: Option<u64>,
    events: Vec<SyncEvent>,
}

#[derive(Default, Serialize, Deserialize)]
struct SyncConfig {
    enabled: bool,
    pairs: Vec<SyncPair>,
}

#[derive(Clone, Serialize)]
pub struct SyncPairView {
    pub id: String,
    pub peer_ip: String,
    pub peer_name: String,
    pub local_dir: String,
    pub enabled: bool,
    pub approved: bool,
    pub incoming: bool,
    pub last_sync_ms: Option<u64>,
    pub events: Vec<SyncEvent>,
}

#[derive(Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub syncing: bool,
    pub error: Option<String>,
    pub pairs: Vec<SyncPairView>,
}

#[derive(Serialize)]
pub struct SyncPreview {
    pub changes: Vec<SyncPreviewChange>,
    pub total: usize,
    pub peer_approved: bool,
}

#[derive(Serialize)]
pub struct SyncPreviewChange {
    pub path: String,
    pub action: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message {
    SetupHello {
        path: String,
        id: String,
        key_hash: String,
        peer_name: String,
    },
    Hello {
        id: String,
    },
    Challenge {
        nonce: String,
    },
    Auth {
        proof: String,
    },
    ManifestRequest {
        #[serde(default)]
        include_existing: bool,
    },
    Manifest {
        files: Manifest,
        baseline: Manifest,
        approved: bool,
        #[serde(default)]
        initial_files: BTreeSet<String>,
        #[serde(default)]
        snapshot_ready: bool,
    },
    Fetch {
        path: String,
        hash: String,
    },
    Put {
        path: String,
        hash: String,
        expected: Option<String>,
        len: u64,
    },
    Delete {
        path: String,
        expected: String,
    },
    Commit {
        files: Manifest,
        #[serde(default)]
        ignored: BTreeSet<String>,
    },
    Disconnect,
    File {
        hash: String,
        len: u64,
    },
    Ready,
    Ok,
    Error {
        message: String,
    },
}

pub struct FolderSync {
    config: Mutex<SyncConfig>,
    data_dir: PathBuf,
    active: AtomicBool,
    listener_started: AtomicBool,
    worker_started: AtomicBool,
    session: Mutex<()>,
    error: Mutex<Option<String>>,
}

impl FolderSync {
    pub fn new() -> Arc<Self> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("NerveDesk");
        Self::with_data_dir(base)
    }

    fn with_data_dir(data_dir: PathBuf) -> Arc<Self> {
        let config = ["folder_sync.json", "folder_sync.json.bak"]
            .iter()
            .find_map(|name| {
                fs::read(data_dir.join(name))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            })
            .unwrap_or_default();
        Arc::new(Self {
            config: Mutex::new(config),
            data_dir,
            active: AtomicBool::new(false),
            listener_started: AtomicBool::new(false),
            worker_started: AtomicBool::new(false),
            session: Mutex::new(()),
            error: Mutex::new(None),
        })
    }

    pub fn status(&self) -> SyncStatus {
        let config = self.config.lock().unwrap();
        SyncStatus {
            enabled: config.enabled,
            syncing: self.active.load(Ordering::SeqCst),
            error: self.error.lock().unwrap().clone(),
            pairs: config
                .pairs
                .iter()
                .map(|p| SyncPairView {
                    id: p.id.clone(),
                    peer_ip: p.peer_ip.clone(),
                    peer_name: p.peer_name.clone(),
                    local_dir: p.local_dir.clone(),
                    enabled: p.enabled,
                    approved: p.approved,
                    incoming: p.incoming,
                    last_sync_ms: p.last_sync_ms,
                    events: p.events.clone(),
                })
                .collect(),
        }
    }

    pub fn generate_code() -> Result<String, String> {
        let mut random = [0u8; 16];
        unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
            .ok()
            .map_err(|e| format!("Random generator: {e}"))?;
        Ok(hex(&random))
    }

    pub fn add_pair(
        self: &Arc<Self>,
        peer_ip: String,
        peer_name: String,
        local_dir: String,
    ) -> Result<SyncStatus, String> {
        let code = Self::generate_code()?;
        let ip: IpAddr = peer_ip
            .parse()
            .map_err(|_| "Select a discovered device".to_string())?;
        let local_name = device_name();
        if peer_name.trim().is_empty() || peer_name.eq_ignore_ascii_case(&local_name) {
            return Err("Select another computer".into());
        }
        let root = normal_path(&fs::canonicalize(&local_dir).map_err(|e| format!("Folder: {e}"))?);
        if !root.is_dir() || root.starts_with(&self.data_dir) || self.data_dir.starts_with(&root) {
            return Err("Choose a regular folder outside NerveDesk data".into());
        }
        let initial_files = scan_paths(&root)?;
        let key_hash = digest(code.as_bytes());
        let id = key_hash[..24].to_string();
        let config = self.config.lock().unwrap();
        if config.pairs.iter().any(|p| p.id == id) {
            return Err("This pair code is already configured".into());
        }
        if config.pairs.iter().any(|p| {
            let existing = normal_path(Path::new(&p.local_dir));
            (p.approved || existing != root)
                && (root.starts_with(&existing) || existing.starts_with(&root))
        }) {
            return Err("Sync folders cannot overlap".into());
        }
        drop(config);
        self.ensure_listener()?;
        self.ensure_worker();
        if let Err(error) = send_setup(&ip.to_string(), &root.to_string_lossy(), &id, &key_hash) {
            *self.error.lock().unwrap() = Some(error.clone());
            return Err(error);
        }
        let mut config = self.config.lock().unwrap();
        config
            .pairs
            .retain(|pair| pair.approved || normal_path(Path::new(&pair.local_dir)) != root);
        config.pairs.push(SyncPair {
            id,
            peer_ip: ip.to_string(),
            peer_name: peer_name.trim().to_string(),
            local_dir: root.to_string_lossy().to_string(),
            key_hash,
            enabled: true,
            approved: true,
            incoming: false,
            initial_files,
            snapshot_ready: true,
            baseline: Manifest::new(),
            last_sync_ms: None,
            events: Vec::new(),
        });
        config.enabled = true;
        self.save(&config)?;
        drop(config);
        *self.error.lock().unwrap() = None;
        Ok(self.status())
    }

    pub fn remove_pair(&self, id: &str) -> Result<SyncStatus, String> {
        let _guard = self.session.lock().unwrap();
        let pair = self.find_pair(id)?;
        if let Ok(mut socket) = connect_pair(&pair) {
            let _ = write_frame(&mut socket, &Message::Disconnect);
            let _ = read_frame(&mut socket);
        }
        let mut config = self.config.lock().unwrap();
        let before = config.pairs.len();
        config.pairs.retain(|p| p.id != id);
        if config.pairs.len() == before {
            return Err("Pair not found".into());
        }
        self.save(&config)?;
        drop(config);
        *self.error.lock().unwrap() = None;
        Ok(self.status())
    }

    pub fn set_enabled(self: &Arc<Self>, enabled: bool) -> Result<SyncStatus, String> {
        if enabled {
            self.ensure_listener()?;
            self.ensure_worker();
        }
        let mut config = self.config.lock().unwrap();
        config.enabled = enabled;
        self.save(&config)?;
        drop(config);
        *self.error.lock().unwrap() = None;
        Ok(self.status())
    }

    pub fn start_saved(self: &Arc<Self>) {
        {
            let mut config = self.config.lock().unwrap();
            let mut changed = false;
            for pair in &mut config.pairs {
                if pair.approved && !pair.snapshot_ready {
                    if let Ok(paths) = scan_paths(Path::new(&pair.local_dir)) {
                        pair.initial_files = paths;
                        pair.baseline.clear();
                        pair.snapshot_ready = true;
                        changed = true;
                    }
                }
            }
            if changed {
                let _ = self.save(&config);
            }
        }
        if let Err(e) = self.ensure_listener() {
            *self.error.lock().unwrap() = Some(e);
        } else {
            self.ensure_worker();
        }
    }

    fn save(&self, config: &SyncConfig) -> Result<(), String> {
        fs::create_dir_all(&self.data_dir).map_err(|e| e.to_string())?;
        let path = self.data_dir.join("folder_sync.json");
        let temp = self.data_dir.join("folder_sync.json.tmp");
        let backup = self.data_dir.join("folder_sync.json.bak");
        let data = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
        fs::write(&temp, data).map_err(|e| format!("Writing sync settings: {e}"))?;
        if path.exists() {
            fs::copy(&path, &backup).map_err(|e| format!("Backing up sync settings: {e}"))?;
            fs::remove_file(&path).map_err(|e| format!("Replacing sync settings: {e}"))?;
        }
        if let Err(rename_error) = fs::rename(&temp, &path) {
            if let Err(copy_error) = fs::copy(&temp, &path) {
                if backup.exists() {
                    let _ = fs::copy(&backup, &path);
                }
                return Err(format!(
                    "Installing sync settings: {rename_error}; copy failed: {copy_error}"
                ));
            }
            let _ = fs::remove_file(&temp);
        }
        Ok(())
    }

    fn ensure_listener(self: &Arc<Self>) -> Result<(), String> {
        if self.listener_started.load(Ordering::SeqCst) {
            return Ok(());
        }
        let listener =
            TcpListener::bind(("0.0.0.0", PORT)).map_err(|e| format!("Sync port {PORT}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        self.listener_started.store(true, Ordering::SeqCst);
        let manager = Arc::clone(self);
        thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, addr)) => {
                    if let Err(e) = stream.set_nonblocking(false) {
                        *manager.error.lock().unwrap() = Some(format!("Sync connection: {e}"));
                        continue;
                    }
                    let manager = Arc::clone(&manager);
                    thread::spawn(move || {
                        if let Err(e) = manager.serve(stream, addr) {
                            *manager.error.lock().unwrap() = Some(e);
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(150))
                }
                Err(e) => {
                    *manager.error.lock().unwrap() = Some(format!("Sync listener: {e}"));
                    thread::sleep(Duration::from_secs(1));
                }
            }
        });
        Ok(())
    }

    fn ensure_worker(self: &Arc<Self>) {
        if self.worker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let manager = Arc::clone(self);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(600));
            let pairs = {
                let config = manager.config.lock().unwrap();
                if !config.enabled {
                    continue;
                }
                config
                    .pairs
                    .iter()
                    .filter(|p| {
                        p.enabled
                            && p.approved
                            && device_name().to_ascii_lowercase() < p.peer_name.to_ascii_lowercase()
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            for pair in pairs {
                if let Err(e) = manager.sync_pair(&pair) {
                    if e == "Sync is busy" {
                        continue;
                    }
                    *manager.error.lock().unwrap() = Some(format!("{}: {e}", pair.peer_name));
                } else {
                    *manager.error.lock().unwrap() = None;
                }
            }
        });
    }

    pub fn sync_now(&self, id: &str) -> Result<SyncStatus, String> {
        let pair = {
            let config = self.config.lock().unwrap();
            if !config.enabled {
                return Err("Enable folder sync first".into());
            }
            config
                .pairs
                .iter()
                .find(|p| p.id == id && p.enabled && p.approved)
                .cloned()
                .ok_or("Pair is not approved")?
        };
        self.sync_pair_with_mode(&pair, true)?;
        Ok(self.status())
    }

    pub fn preview(&self, id: &str) -> Result<SyncPreview, String> {
        let _guard = self
            .session
            .try_lock()
            .map_err(|_| "Sync is busy".to_string())?;
        let pair = self.find_pair(id)?;
        let mut socket = connect_pair(&pair)?;
        let (mut remote, remote_baseline, peer_approved, remote_initial) =
            remote_manifest(&mut socket)?;
        let ignored = pair.initial_files.union(&remote_initial).cloned().collect();
        let local = scan_excluding(Path::new(&pair.local_dir), &ignored)?;
        remote.retain(|path, _| !ignored.contains(path));
        let baseline = if pair.baseline == remote_baseline {
            pair.baseline
        } else {
            Manifest::new()
        };
        let mut paths = std::collections::BTreeSet::new();
        paths.extend(local.keys().cloned());
        paths.extend(remote.keys().cloned());
        paths.extend(baseline.keys().cloned());
        let mut changes = Vec::new();
        let mut total = 0;
        for path in paths {
            let l = local.get(&path);
            let r = remote.get(&path);
            let b = baseline.get(&path);
            let action = match plan_action(l, r, b) {
                SyncAction::None => continue,
                SyncAction::Upload => "upload",
                SyncAction::Download => "download",
                SyncAction::DeleteLocal => "delete_local",
                SyncAction::DeleteRemote => "delete_remote",
                SyncAction::ConflictFiles
                | SyncAction::RestoreLocal
                | SyncAction::RestoreRemote => "conflict",
            };
            total += 1;
            if changes.len() < 200 {
                changes.push(SyncPreviewChange {
                    path,
                    action: action.into(),
                });
            }
        }
        Ok(SyncPreview {
            changes,
            total,
            peer_approved,
        })
    }

    pub fn approve(&self, id: &str) -> Result<SyncStatus, String> {
        let pair = self.find_pair(id)?;
        let mut initial_files = BTreeSet::new();
        if !pair.approved {
            let path = validated_setup_path(&pair.local_dir, &self.data_dir)?;
            fs::create_dir_all(&path)
                .map_err(|e| format!("Creating folder on this computer: {e}"))?;
            initial_files = scan_paths(&path)?;
        }
        self.update_pair(id, |pair| {
            pair.approved = true;
            pair.initial_files = initial_files;
            pair.snapshot_ready = true;
        })?;
        Ok(self.status())
    }

    pub fn restore_version(&self, pair_id: &str, version_id: &str) -> Result<SyncStatus, String> {
        let _guard = self.session.lock().unwrap();
        let pair = self.find_pair(pair_id)?;
        let event = pair
            .events
            .iter()
            .find(|e| e.version_id.as_deref() == Some(version_id))
            .ok_or("Version not found")?;
        let stored = self.history_path(&pair.id, version_id);
        if !stored.is_file() {
            return Err("Saved version is missing".into());
        }
        let root = Path::new(&pair.local_dir);
        let target = safe_target(root, &event.path)?;
        let temp = temp_path(&target);
        if let Some(parent) = temp.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(stored, &temp).map_err(|e| e.to_string())?;
        self.install_file(&pair, &event.path, &temp, "restored")?;
        Ok(self.status())
    }

    fn find_pair(&self, id: &str) -> Result<SyncPair, String> {
        self.config
            .lock()
            .unwrap()
            .pairs
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or("Pair not found".into())
    }

    fn update_pair<F>(&self, id: &str, update: F) -> Result<(), String>
    where
        F: FnOnce(&mut SyncPair),
    {
        let mut config = self.config.lock().unwrap();
        let pair = config
            .pairs
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or("Pair not found")?;
        update(pair);
        self.save(&config)
    }

    fn history_path(&self, pair_id: &str, version_id: &str) -> PathBuf {
        self.data_dir
            .join("folder_sync_history")
            .join(pair_id)
            .join(format!("{version_id}.bin"))
    }

    fn install_file(
        &self,
        pair: &SyncPair,
        relative: &str,
        temp: &Path,
        kind: &str,
    ) -> Result<(), String> {
        let target = safe_target(Path::new(&pair.local_dir), relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let version_id = if target.exists() {
            Some(self.backup(pair, relative, &target)?)
        } else {
            None
        };
        if target.exists() {
            fs::remove_file(&target).map_err(|e| format!("Replacing old file: {e}"))?;
        }
        if let Err(rename_error) = fs::rename(temp, &target) {
            if let Err(copy_error) = fs::copy(temp, &target) {
                if let Some(ref id) = version_id {
                    let _ = fs::copy(self.history_path(&pair.id, id), &target);
                }
                return Err(format!(
                    "Installing synced file: {rename_error}; copy failed: {copy_error}"
                ));
            }
            let _ = fs::remove_file(temp);
        }
        self.record(pair, relative, kind, version_id, "")
    }

    fn delete_file(&self, pair: &SyncPair, relative: &str) -> Result<(), String> {
        let target = safe_target(Path::new(&pair.local_dir), relative)?;
        if !target.exists() {
            return Ok(());
        }
        let version_id = self.backup(pair, relative, &target)?;
        fs::remove_file(&target).map_err(|e| e.to_string())?;
        self.record(pair, relative, "deleted", Some(version_id), "")
    }

    fn backup(&self, pair: &SyncPair, _relative: &str, target: &Path) -> Result<String, String> {
        let id = Self::generate_code()?;
        let saved = self.history_path(&pair.id, &id);
        fs::create_dir_all(saved.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::copy(target, saved).map_err(|e| format!("Saving file history: {e}"))?;
        Ok(id)
    }

    fn record(
        &self,
        pair: &SyncPair,
        relative: &str,
        kind: &str,
        version_id: Option<String>,
        detail: &str,
    ) -> Result<(), String> {
        let event = SyncEvent {
            id: Self::generate_code()?,
            time_ms: now_ms(),
            path: relative.into(),
            kind: kind.into(),
            detail: detail.into(),
            version_id,
        };
        self.update_pair(&pair.id, |p| {
            p.events.insert(0, event);
            p.events.truncate(MAX_EVENTS);
        })
    }
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hmac(key: &[u8], bytes: &[u8]) -> String {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0x36u8; 64];
    let mut outer = [0x5cu8; 64];
    for i in 0..64 {
        inner[i] ^= block[i];
        outer[i] ^= block[i];
    }
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(bytes);
    let result = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(result);
    hex(&hash.finalize())
}

fn secure_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn temp_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(format!(".nervedesk-{}.part", now_ms()));
    PathBuf::from(name)
}

fn safe_target(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty()
        || relative.len() > 4096
        || relative.contains(':')
        || relative.contains('\\')
    {
        return Err("Unsafe sync path".into());
    }
    let path = Path::new(relative);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("Unsafe sync path".into());
    }
    let mut result = root.to_path_buf();
    for component in path.components() {
        result.push(component.as_os_str());
        if result
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("Sync path contains a link".into());
        }
    }
    Ok(result)
}

fn file_hash(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex(&hash.finalize()))
}

fn scan_paths(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut files = BTreeSet::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                folders.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(".nervedesk-") && name.ends_with(".part") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            safe_target(root, &relative)?;
            files.insert(relative);
            if files.len() > MAX_FILES {
                return Err("Too many files in sync folder".into());
            }
        }
    }
    Ok(files)
}

fn scan_excluding(root: &Path, ignored: &BTreeSet<String>) -> Result<Manifest, String> {
    let mut files = Manifest::new();
    for relative in scan_paths(root)? {
        if ignored.contains(&relative) {
            continue;
        }
        let path = safe_target(root, &relative)?;
        let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(format!("File too large: {}", path.display()));
        }
        files.insert(relative, file_hash(&path)?);
    }
    Ok(files)
}

#[cfg(test)]
fn scan(root: &Path) -> Result<Manifest, String> {
    scan_excluding(root, &BTreeSet::new())
}

fn write_frame(stream: &mut TcpStream, message: &Message) -> Result<(), String> {
    let bytes = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("Sync message too large".into());
    }
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(&bytes).map_err(|e| e.to_string())
}

fn read_frame(stream: &mut TcpStream) -> Result<Message, String> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).map_err(|e| e.to_string())?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("Invalid sync message length".into());
    }
    let mut bytes = vec![0u8; length];
    stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn io_stream(mut input: impl Read, mut output: impl Write, length: u64) -> Result<String, String> {
    let mut hash = Sha256::new();
    let mut left = length;
    let mut buffer = [0u8; 64 * 1024];
    while left > 0 {
        let limit = left.min(buffer.len() as u64) as usize;
        let count = input
            .read(&mut buffer[..limit])
            .map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("Incomplete sync file".into());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        hash.update(&buffer[..count]);
        left -= count as u64;
    }
    output.flush().map_err(|e| e.to_string())?;
    Ok(hex(&hash.finalize()))
}

impl FolderSync {
    fn serve(&self, mut socket: TcpStream, address: SocketAddr) -> Result<(), String> {
        socket.set_read_timeout(Some(Duration::from_secs(30))).ok();
        socket.set_write_timeout(Some(Duration::from_secs(30))).ok();
        let id = match read_frame(&mut socket)? {
            Message::Hello { id } => id,
            Message::SetupHello {
                path,
                id,
                key_hash,
                peer_name,
            } => {
                let result = self.accept_setup(address, path, id, key_hash, peer_name);
                match &result {
                    Ok(_) => write_frame(&mut socket, &Message::Ok)?,
                    Err(message) => write_frame(
                        &mut socket,
                        &Message::Error {
                            message: message.clone(),
                        },
                    )?,
                }
                return result;
            }
            _ => return Err("Expected sync hello".into()),
        };
        let pair = {
            let config = self.config.lock().unwrap();
            if !config.enabled {
                return Err("Folder sync is disabled".into());
            }
            config
                .pairs
                .iter()
                .find(|p| p.id == id && p.enabled)
                .cloned()
                .ok_or("Unknown sync pair")?
        };
        if address.ip().to_string() != pair.peer_ip {
            return Err("Sync peer address does not match pair".into());
        }
        if !pair.approved {
            write_frame(
                &mut socket,
                &Message::Error {
                    message: "Approve this folder pair on the receiving computer".into(),
                },
            )?;
            return Ok(());
        }
        let nonce = Self::generate_code()?;
        write_frame(
            &mut socket,
            &Message::Challenge {
                nonce: nonce.clone(),
            },
        )?;
        let proof = match read_frame(&mut socket)? {
            Message::Auth { proof } => proof,
            _ => return Err("Expected sync authentication".into()),
        };
        let expected = hmac(
            pair.key_hash.as_bytes(),
            format!("{}:{nonce}", pair.id).as_bytes(),
        );
        if !secure_equal(&proof, &expected) {
            write_frame(
                &mut socket,
                &Message::Error {
                    message: "Pair code mismatch".into(),
                },
            )?;
            return Err("Sync authentication failed".into());
        }
        let _session = match self.session.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                write_frame(
                    &mut socket,
                    &Message::Error {
                        message: "Sync is busy".into(),
                    },
                )?;
                return Ok(());
            }
        };
        write_frame(&mut socket, &Message::Ok)?;
        let root = Path::new(&pair.local_dir);
        let mut include_existing_for_session = false;
        loop {
            if !self.config.lock().unwrap().enabled {
                return Ok(());
            }
            let message = match read_frame(&mut socket) {
                Ok(message) => message,
                Err(_) => return Ok(()),
            };
            match message {
                Message::ManifestRequest { include_existing } => {
                    include_existing_for_session = include_existing;
                    let current_pair = self.find_pair(&id)?;
                    let ignored = if include_existing {
                        BTreeSet::new()
                    } else {
                        current_pair.initial_files.clone()
                    };
                    let files = scan_excluding(root, &ignored)?;
                    write_frame(
                        &mut socket,
                        &Message::Manifest {
                            files,
                            baseline: current_pair.baseline,
                            approved: current_pair.approved,
                            initial_files: current_pair.initial_files,
                            snapshot_ready: current_pair.snapshot_ready,
                        },
                    )?;
                }
                Message::Fetch { path, hash } => {
                    let target = safe_target(root, &path)?;
                    let metadata = fs::metadata(&target).map_err(|e| e.to_string())?;
                    if !metadata.is_file()
                        || metadata.len() > MAX_FILE_SIZE
                        || file_hash(&target)? != hash
                    {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "File changed during sync".into(),
                            },
                        )?;
                        continue;
                    }
                    write_frame(
                        &mut socket,
                        &Message::File {
                            hash: hash.clone(),
                            len: metadata.len(),
                        },
                    )?;
                    let actual = io_stream(
                        File::open(target).map_err(|e| e.to_string())?,
                        &mut socket,
                        metadata.len(),
                    )?;
                    if actual != hash {
                        return Err("File changed during transfer".into());
                    }
                }
                Message::Put {
                    path,
                    hash,
                    expected,
                    len,
                } => {
                    if !self.find_pair(&id)?.approved {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "Approve this folder pair on the receiver".into(),
                            },
                        )?;
                        continue;
                    }
                    if len > MAX_FILE_SIZE {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "File too large".into(),
                            },
                        )?;
                        continue;
                    }
                    let target = safe_target(root, &path)?;
                    let current = if target.is_file() {
                        Some(file_hash(&target)?)
                    } else {
                        None
                    };
                    if current != expected {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "File changed during sync".into(),
                            },
                        )?;
                        continue;
                    }
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    let temp = temp_path(&target);
                    write_frame(&mut socket, &Message::Ready)?;
                    let received = io_stream(
                        &mut socket,
                        File::create(&temp).map_err(|e| e.to_string())?,
                        len,
                    );
                    match received {
                        Ok(actual) if actual == hash => {
                            if let Err(e) = self.install_file(&pair, &path, &temp, "received") {
                                let _ = fs::remove_file(&temp);
                                write_frame(&mut socket, &Message::Error { message: e })?;
                            } else {
                                write_frame(&mut socket, &Message::Ok)?;
                            }
                        }
                        Ok(_) => {
                            let _ = fs::remove_file(&temp);
                            write_frame(
                                &mut socket,
                                &Message::Error {
                                    message: "File checksum mismatch".into(),
                                },
                            )?;
                        }
                        Err(e) => {
                            let _ = fs::remove_file(&temp);
                            return Err(e);
                        }
                    }
                }
                Message::Delete { path, expected } => {
                    if !self.find_pair(&id)?.approved {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "Approve this folder pair on the receiver".into(),
                            },
                        )?;
                        continue;
                    }
                    let target = safe_target(root, &path)?;
                    if !target.is_file() || file_hash(&target)? != expected {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "File changed during sync".into(),
                            },
                        )?;
                        continue;
                    }
                    self.delete_file(&pair, &path)?;
                    write_frame(&mut socket, &Message::Ok)?;
                }
                Message::Commit { files, ignored } => {
                    if !self.find_pair(&id)?.approved {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "Approve this folder pair on the receiver".into(),
                            },
                        )?;
                        continue;
                    }
                    let current_pair = self.find_pair(&id)?;
                    if files.len() > MAX_FILES
                        || (include_existing_for_session && !ignored.is_empty())
                        || (!include_existing_for_session
                            && !current_pair.initial_files.is_subset(&ignored))
                        || scan_excluding(root, &ignored)? != files
                    {
                        write_frame(
                            &mut socket,
                            &Message::Error {
                                message: "Folder changed during sync".into(),
                            },
                        )?;
                        continue;
                    }
                    self.update_pair(&id, |p| {
                        p.baseline = files;
                        if include_existing_for_session {
                            p.initial_files.clear();
                        }
                        p.last_sync_ms = Some(now_ms());
                    })?;
                    write_frame(&mut socket, &Message::Ok)?;
                }
                Message::Disconnect => {
                    let mut config = self.config.lock().unwrap();
                    config.pairs.retain(|p| p.id != id);
                    self.save(&config)?;
                    write_frame(&mut socket, &Message::Ok)?;
                    return Ok(());
                }
                _ => return Err("Unexpected sync request".into()),
            }
        }
    }

    fn accept_setup(
        &self,
        address: SocketAddr,
        path: String,
        id: String,
        key_hash: String,
        peer_name: String,
    ) -> Result<(), String> {
        let paired_ip = address.ip().to_string();
        if key_hash.len() != 64
            || !key_hash.bytes().all(|b| b.is_ascii_hexdigit())
            || id != key_hash[..24]
        {
            return Err("Invalid folder pair identifier".into());
        }
        if peer_name.trim().is_empty() || peer_name.eq_ignore_ascii_case(&device_name()) {
            return Err("Invalid peer name".into());
        }
        let requested = validated_setup_path(&path, &self.data_dir)?;
        let _session = self.session.lock().unwrap();
        let mut config = self.config.lock().unwrap();
        if let Some(existing) = config.pairs.iter().find(|pair| pair.id == id) {
            if normal_path(Path::new(&existing.local_dir)) == requested
                && existing.peer_ip == paired_ip
            {
                return Ok(());
            }
            return Err("Folder pair identifier is already used".into());
        }
        if config.pairs.iter().any(|pair| {
            let existing = normal_path(Path::new(&pair.local_dir));
            (pair.approved || existing != requested)
                && (requested.starts_with(&existing) || existing.starts_with(&requested))
        }) {
            return Err("Another sync pair already uses this folder".into());
        }
        let root = requested;
        if root.starts_with(&self.data_dir) || self.data_dir.starts_with(&root) {
            return Err("Folder overlaps NerveDesk data".into());
        }
        config
            .pairs
            .retain(|pair| pair.approved || normal_path(Path::new(&pair.local_dir)) != root);
        config.pairs.push(SyncPair {
            id,
            peer_ip: paired_ip,
            peer_name,
            local_dir: root.to_string_lossy().to_string(),
            key_hash,
            enabled: true,
            approved: false,
            incoming: true,
            initial_files: BTreeSet::new(),
            snapshot_ready: false,
            baseline: Manifest::new(),
            last_sync_ms: None,
            events: Vec::new(),
        });
        config.enabled = true;
        self.save(&config)?;
        *self.error.lock().unwrap() = None;
        Ok(())
    }

    fn sync_pair(&self, pair: &SyncPair) -> Result<(), String> {
        self.sync_pair_with_mode(pair, false)
    }

    fn sync_pair_with_mode(&self, pair: &SyncPair, include_existing: bool) -> Result<(), String> {
        let _guard = self
            .session
            .try_lock()
            .map_err(|_| "Sync is busy".to_string())?;
        if !self.config.lock().unwrap().enabled {
            return Err("Folder sync is disabled".into());
        }
        self.active.store(true, Ordering::SeqCst);
        let result = self.sync_pair_inner_at_mode(pair, PORT, include_existing);
        self.active.store(false, Ordering::SeqCst);
        result
    }

    #[cfg(test)]
    fn sync_pair_inner_at(&self, pair: &SyncPair, port: u16) -> Result<(), String> {
        self.sync_pair_inner_at_mode(pair, port, false)
    }

    fn sync_pair_inner_at_mode(
        &self,
        pair: &SyncPair,
        port: u16,
        include_existing: bool,
    ) -> Result<(), String> {
        let mut socket = connect_pair_at(pair, port)?;
        let root = Path::new(&pair.local_dir);
        let (mut remote, remote_baseline, peer_approved, remote_initial) =
            remote_manifest_with_mode(&mut socket, include_existing)?;
        let ignored: BTreeSet<String> = if include_existing {
            BTreeSet::new()
        } else {
            pair.initial_files.union(&remote_initial).cloned().collect()
        };
        let local = scan_excluding(root, &ignored)?;
        remote.retain(|path, _| !ignored.contains(path));
        if !peer_approved {
            return Err("Approve this folder pair on the other computer".into());
        }
        let baseline = if pair.baseline == remote_baseline {
            pair.baseline.clone()
        } else {
            Manifest::new()
        };
        if pair.last_sync_ms.is_some() && local == remote && local == baseline {
            return Ok(());
        }
        let mut paths = std::collections::BTreeSet::new();
        paths.extend(local.keys().cloned());
        paths.extend(remote.keys().cloned());
        paths.extend(baseline.keys().cloned());
        for path in paths {
            if !self.config.lock().unwrap().enabled {
                return Err("Folder sync stopped".into());
            }
            let l = local.get(&path);
            let r = remote.get(&path);
            let b = baseline.get(&path);
            match plan_action(l, r, b) {
                SyncAction::None => {}
                SyncAction::Upload => self.put_file(pair, &mut socket, &path, l.unwrap(), r)?,
                SyncAction::Download => {
                    self.fetch_file(pair, &mut socket, &path, &path, r.unwrap(), l)?
                }
                SyncAction::DeleteLocal => self.delete_local_checked(pair, &path, l)?,
                SyncAction::DeleteRemote => self.delete_remote(&mut socket, &path, r.unwrap())?,
                SyncAction::ConflictFiles => {
                    let conflict = format!(
                        "{path}.conflict-{}-{}",
                        sanitize_name(&pair.peer_name),
                        now_ms()
                    );
                    self.fetch_file(
                        pair,
                        &mut socket,
                        &path,
                        &conflict,
                        r.unwrap(),
                        None::<&String>,
                    )?;
                    self.put_file(pair, &mut socket, &conflict, r.unwrap(), None::<&String>)?;
                    self.put_file(pair, &mut socket, &path, l.unwrap(), r)?;
                    self.record(pair, &path, "conflict", None, &conflict)?;
                }
                SyncAction::RestoreRemote => {
                    self.put_file(pair, &mut socket, &path, l.unwrap(), r)?;
                    self.record(
                        pair,
                        &path,
                        "conflict",
                        None,
                        "Remote deletion was not applied",
                    )?;
                }
                SyncAction::RestoreLocal => {
                    self.fetch_file(pair, &mut socket, &path, &path, r.unwrap(), l)?;
                    self.record(
                        pair,
                        &path,
                        "conflict",
                        None,
                        "Local deletion was not applied",
                    )?;
                }
            }
        }
        let final_local = scan_excluding(root, &ignored)?;
        let (mut final_remote, _, _, _) = remote_manifest_with_mode(&mut socket, include_existing)?;
        final_remote.retain(|path, _| !ignored.contains(path));
        if final_local != final_remote {
            return Err("Folders changed during sync; retrying".into());
        }
        write_frame(
            &mut socket,
            &Message::Commit {
                files: final_local.clone(),
                ignored,
            },
        )?;
        expect_ok(read_frame(&mut socket)?)?;
        self.update_pair(&pair.id, |p| {
            p.baseline = final_local;
            if include_existing {
                p.initial_files.clear();
            }
            p.last_sync_ms = Some(now_ms());
        })
    }

    fn fetch_file(
        &self,
        pair: &SyncPair,
        socket: &mut TcpStream,
        source_path: &str,
        dest_path: &str,
        hash: &str,
        expected: Option<&String>,
    ) -> Result<(), String> {
        let root = Path::new(&pair.local_dir);
        let target = safe_target(root, dest_path)?;
        let current = if target.is_file() {
            Some(file_hash(&target)?)
        } else {
            None
        };
        if current.as_ref() != expected {
            return Err("Local file changed during sync".into());
        }
        write_frame(
            socket,
            &Message::Fetch {
                path: source_path.into(),
                hash: hash.into(),
            },
        )?;
        let len = match read_frame(socket)? {
            Message::File {
                hash: response_hash,
                len,
            } if response_hash == hash && len <= MAX_FILE_SIZE => len,
            other => return Err(message_error(other)),
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temp = temp_path(&target);
        let received = io_stream(
            &mut *socket,
            File::create(&temp).map_err(|e| e.to_string())?,
            len,
        );
        match received {
            Ok(actual) if actual == hash => self.install_file(pair, dest_path, &temp, "received"),
            Ok(_) => {
                let _ = fs::remove_file(&temp);
                Err("File checksum mismatch".into())
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(e)
            }
        }
    }

    fn put_file(
        &self,
        pair: &SyncPair,
        socket: &mut TcpStream,
        path: &str,
        hash: &str,
        expected: Option<&String>,
    ) -> Result<(), String> {
        let target = safe_target(Path::new(&pair.local_dir), path)?;
        if file_hash(&target)? != hash {
            return Err("Local file changed during sync".into());
        }
        let len = fs::metadata(&target).map_err(|e| e.to_string())?.len();
        if len > MAX_FILE_SIZE {
            return Err("File too large".into());
        }
        write_frame(
            socket,
            &Message::Put {
                path: path.into(),
                hash: hash.into(),
                expected: expected.cloned(),
                len,
            },
        )?;
        match read_frame(socket)? {
            Message::Ready => {}
            other => return Err(message_error(other)),
        }
        let actual = io_stream(
            File::open(target).map_err(|e| e.to_string())?,
            &mut *socket,
            len,
        )?;
        if actual != hash {
            return Err("Local file changed during transfer".into());
        }
        expect_ok(read_frame(socket)?)
    }

    fn delete_remote(
        &self,
        socket: &mut TcpStream,
        path: &str,
        expected: &str,
    ) -> Result<(), String> {
        write_frame(
            socket,
            &Message::Delete {
                path: path.into(),
                expected: expected.into(),
            },
        )?;
        expect_ok(read_frame(socket)?)
    }

    fn delete_local_checked(
        &self,
        pair: &SyncPair,
        path: &str,
        expected: Option<&String>,
    ) -> Result<(), String> {
        let target = safe_target(Path::new(&pair.local_dir), path)?;
        let current = if target.is_file() {
            Some(file_hash(&target)?)
        } else {
            None
        };
        if current.as_ref() != expected {
            return Err("Local file changed during sync".into());
        }
        self.delete_file(pair, path)
    }
}

fn connect_pair(pair: &SyncPair) -> Result<TcpStream, String> {
    connect_pair_at(pair, PORT)
}

fn normal_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix("\\\\?\\").unwrap_or(&text))
}

fn validated_setup_path(path: &str, data_dir: &Path) -> Result<PathBuf, String> {
    let requested = PathBuf::from(path);
    if path.len() > 4096 || !requested.is_absolute() || requested.components().count() < 3 {
        return Err("Choose a full folder path below a drive root".into());
    }
    if requested
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("Unsafe folder path".into());
    }
    if requested.starts_with(data_dir) || data_dir.starts_with(&requested) {
        return Err("Folder overlaps NerveDesk data".into());
    }
    if let Some(windows_dir) = std::env::var_os("WINDIR") {
        if requested.starts_with(windows_dir) {
            return Err("System folders cannot be synced".into());
        }
    }
    Ok(requested)
}

fn send_setup(peer_ip: &str, root: &str, id: &str, key_hash: &str) -> Result<(), String> {
    let address: SocketAddr = format!("{peer_ip}:{PORT}")
        .parse()
        .map_err(|e| format!("Peer address: {e}"))?;
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|e| format!("Other computer unavailable on TCP {PORT}: {e}. Run the latest NerveDesk on both PCs and allow the app through Windows Firewall"))?;
    socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
    socket.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let peer_name = device_name();
    write_frame(
        &mut socket,
        &Message::SetupHello {
            path: root.into(),
            id: id.into(),
            key_hash: key_hash.into(),
            peer_name: peer_name.clone(),
        },
    )?;
    expect_ok(read_frame(&mut socket)?)
}

fn connect_pair_at(pair: &SyncPair, port: u16) -> Result<TcpStream, String> {
    let address: SocketAddr = format!("{}:{port}", pair.peer_ip)
        .parse()
        .map_err(|e| format!("Peer address: {e}"))?;
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|e| format!("Peer unavailable: {e}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(30))).ok();
    socket.set_write_timeout(Some(Duration::from_secs(30))).ok();
    write_frame(
        &mut socket,
        &Message::Hello {
            id: pair.id.clone(),
        },
    )?;
    let nonce = match read_frame(&mut socket)? {
        Message::Challenge { nonce } => nonce,
        other => return Err(message_error(other)),
    };
    let proof = hmac(
        pair.key_hash.as_bytes(),
        format!("{}:{nonce}", pair.id).as_bytes(),
    );
    write_frame(&mut socket, &Message::Auth { proof })?;
    expect_ok(read_frame(&mut socket)?)?;
    Ok(socket)
}

fn remote_manifest(
    socket: &mut TcpStream,
) -> Result<(Manifest, Manifest, bool, BTreeSet<String>), String> {
    remote_manifest_with_mode(socket, false)
}

fn remote_manifest_with_mode(
    socket: &mut TcpStream,
    include_existing: bool,
) -> Result<(Manifest, Manifest, bool, BTreeSet<String>), String> {
    write_frame(socket, &Message::ManifestRequest { include_existing })?;
    match read_frame(socket)? {
        Message::Manifest {
            files,
            baseline,
            approved,
            initial_files,
            snapshot_ready: true,
        } if files.len() <= MAX_FILES
            && baseline.len() <= MAX_FILES
            && initial_files.len() <= MAX_FILES =>
        {
            Ok((files, baseline, approved, initial_files))
        }
        Message::Manifest { .. } => {
            Err("Update NerveDesk on both computers to use the new folder sync".into())
        }
        other => Err(message_error(other)),
    }
}

fn expect_ok(message: Message) -> Result<(), String> {
    match message {
        Message::Ok => Ok(()),
        other => Err(message_error(other)),
    }
}

fn message_error(message: Message) -> String {
    match message {
        Message::Error { message } => message,
        _ => "Unexpected sync response".into(),
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(root: &Path, name: &str, code: &str) -> SyncPair {
        let key_hash = digest(code.as_bytes());
        SyncPair {
            id: key_hash[..24].into(),
            peer_ip: "127.0.0.1".into(),
            peer_name: name.into(),
            local_dir: root.to_string_lossy().to_string(),
            key_hash,
            enabled: true,
            approved: true,
            incoming: false,
            initial_files: BTreeSet::new(),
            snapshot_ready: true,
            baseline: Manifest::new(),
            last_sync_ms: None,
            events: Vec::new(),
        }
    }

    #[test]
    fn rejects_unsafe_paths() {
        let root = Path::new("C:\\sync");
        assert!(safe_target(root, "../outside").is_err());
        assert!(safe_target(root, "C:/outside").is_err());
        assert!(safe_target(root, "folder\\file.txt").is_err());
        assert!(safe_target(root, "folder/file.txt").is_ok());
    }

    #[test]
    fn validates_automatic_pair_path() {
        let data = Path::new("C:\\Users\\tester\\AppData\\Roaming\\NerveDesk");
        assert!(validated_setup_path("Downloads", data).is_err());
        assert!(validated_setup_path(r"C:\", data).is_err());
        assert!(
            validated_setup_path("C:\\Users\\tester\\AppData\\Roaming\\NerveDesk\\sync", data)
                .is_err()
        );
        assert!(validated_setup_path("C:\\Users\\tester\\Downloads", data).is_ok());
        assert_eq!(
            normal_path(Path::new("\\\\?\\C:\\Users\\tester\\Downloads")),
            PathBuf::from("C:\\Users\\tester\\Downloads")
        );
    }

    #[test]
    fn incoming_request_waits_for_approval_before_creating_folder() {
        let base = std::env::temp_dir().join(format!(
            "nervedesk-sync-approval-{}",
            FolderSync::generate_code().unwrap()
        ));
        let root = base.join("new-folder");
        let manager = FolderSync::with_data_dir(base.join("data"));
        let key_hash = digest(b"0123456789abcdef0123456789abcdef");
        let id = key_hash[..24].to_string();
        manager
            .accept_setup(
                "127.0.0.2:49152".parse().unwrap(),
                root.to_string_lossy().to_string(),
                id.clone(),
                key_hash,
                "OTHER".into(),
            )
            .unwrap();
        assert!(!root.exists());
        let restored = FolderSync::with_data_dir(base.join("data"));
        assert!(restored.status().pairs[0].incoming);
        restored.approve(&id).unwrap();
        assert!(root.is_dir());
        assert!(restored.status().pairs[0].approved);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn only_files_created_after_connection_enter_manifest() {
        let root = std::env::temp_dir().join(format!(
            "nervedesk-sync-new-only-{}",
            FolderSync::generate_code().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("old.txt"), b"old").unwrap();
        let initial_files = scan_paths(&root).unwrap();
        let remote_initial = BTreeSet::from(["remote.txt".to_string()]);
        let ignored: BTreeSet<String> = initial_files.union(&remote_initial).cloned().collect();
        fs::write(root.join("new.txt"), b"new").unwrap();
        let manifest = scan_excluding(&root, &ignored).unwrap();
        assert!(!manifest.contains_key("old.txt"));
        assert!(manifest.contains_key("new.txt"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plans_initial_merge_changes_deletions_and_conflicts() {
        let old = "old".to_string();
        let left = "left".to_string();
        let right = "right".to_string();
        assert_eq!(plan_action(Some(&left), None, None), SyncAction::Upload);
        assert_eq!(plan_action(None, Some(&right), None), SyncAction::Download);
        assert_eq!(
            plan_action(Some(&left), Some(&right), None),
            SyncAction::ConflictFiles
        );
        assert_eq!(
            plan_action(Some(&old), None, Some(&old)),
            SyncAction::DeleteLocal
        );
        assert_eq!(
            plan_action(None, Some(&old), Some(&old)),
            SyncAction::DeleteRemote
        );
        assert_eq!(
            plan_action(Some(&old), Some(&right), Some(&old)),
            SyncAction::Download
        );
        assert_eq!(
            plan_action(Some(&left), Some(&old), Some(&old)),
            SyncAction::Upload
        );
        assert_eq!(
            plan_action(Some(&left), None, Some(&old)),
            SyncAction::RestoreRemote
        );
        assert_eq!(
            plan_action(None, Some(&right), Some(&old)),
            SyncAction::RestoreLocal
        );
        assert_eq!(
            plan_action(Some(&left), Some(&right), Some(&old)),
            SyncAction::ConflictFiles
        );
    }

    #[test]
    fn keeps_previous_versions_for_restore() {
        let base = std::env::temp_dir().join(format!(
            "nervedesk-sync-history-{}",
            FolderSync::generate_code().unwrap()
        ));
        let root = base.join("files");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), b"original").unwrap();
        let manager = FolderSync::with_data_dir(base.join("data"));
        let pair = pair(&root, "OTHER", "0123456789abcdef0123456789abcdef");
        manager.config.lock().unwrap().pairs.push(pair.clone());
        let temp = root.join("new.part");
        fs::write(&temp, b"changed").unwrap();
        manager
            .install_file(&pair, "note.txt", &temp, "received")
            .unwrap();
        let saved = manager.find_pair(&pair.id).unwrap().events[0]
            .version_id
            .clone()
            .unwrap();
        assert_eq!(fs::read(root.join("note.txt")).unwrap(), b"changed");
        manager.restore_version(&pair.id, &saved).unwrap();
        assert_eq!(fs::read(root.join("note.txt")).unwrap(), b"original");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    #[ignore = "requires a local TCP socket"]
    fn syncs_changes_conflicts_deletions_and_restore() {
        let base = std::env::temp_dir().join(format!(
            "nervedesk-sync-test-{}",
            FolderSync::generate_code().unwrap()
        ));
        let left_root = base.join("left");
        let right_root = base.join("right");
        fs::create_dir_all(&left_root).unwrap();
        fs::create_dir_all(&right_root).unwrap();
        fs::write(left_root.join("left.txt"), b"left").unwrap();
        fs::write(right_root.join("right.txt"), b"right").unwrap();
        fs::write(left_root.join("shared.txt"), b"left version").unwrap();
        fs::write(right_root.join("shared.txt"), b"right version").unwrap();
        let left = FolderSync::with_data_dir(base.join("left-data"));
        let right = FolderSync::with_data_dir(base.join("right-data"));
        let left_pair = pair(&left_root, "RIGHT", "0123456789abcdef0123456789abcdef");
        let right_pair = pair(&right_root, "LEFT", "0123456789abcdef0123456789abcdef");
        left.config.lock().unwrap().pairs.push(left_pair.clone());
        right.config.lock().unwrap().pairs.push(right_pair);
        left.config.lock().unwrap().enabled = true;
        right.config.lock().unwrap().enabled = true;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver = {
            let right = right.clone();
            thread::spawn(move || {
                for _ in 0..6 {
                    let (socket, address) = listener.accept().unwrap();
                    right.serve(socket, address).unwrap();
                }
            })
        };
        left.sync_pair_inner_at(&left_pair, port).unwrap();
        assert_eq!(scan(&left_root).unwrap(), scan(&right_root).unwrap());
        assert!(scan(&left_root)
            .unwrap()
            .keys()
            .any(|p| p.starts_with("shared.txt.conflict-")));
        fs::write(left_root.join("left.txt"), b"left changed").unwrap();
        fs::write(right_root.join("right.txt"), b"right changed").unwrap();
        let current = left.find_pair(&left_pair.id).unwrap();
        left.sync_pair_inner_at(&current, port).unwrap();
        assert_eq!(scan(&left_root).unwrap(), scan(&right_root).unwrap());
        fs::remove_file(left_root.join("left.txt")).unwrap();
        let current = left.find_pair(&left_pair.id).unwrap();
        left.sync_pair_inner_at(&current, port).unwrap();
        assert!(!right_root.join("left.txt").exists());
        let remote = right.find_pair(&left_pair.id).unwrap();
        let version = remote
            .events
            .iter()
            .find(|e| e.path == "left.txt" && e.kind == "deleted")
            .unwrap()
            .version_id
            .clone()
            .unwrap();
        right.restore_version(&left_pair.id, &version).unwrap();
        let current = left.find_pair(&left_pair.id).unwrap();
        left.sync_pair_inner_at(&current, port).unwrap();
        assert_eq!(
            fs::read(left_root.join("left.txt")).unwrap(),
            b"left changed"
        );
        assert_eq!(scan(&left_root).unwrap(), scan(&right_root).unwrap());
        fs::write(
            left_root.join("previously-ignored.txt"),
            b"existing content",
        )
        .unwrap();
        left.update_pair(&left_pair.id, |p| {
            p.initial_files.insert("previously-ignored.txt".into());
        })
        .unwrap();
        let current = left.find_pair(&left_pair.id).unwrap();
        left.sync_pair_inner_at(&current, port).unwrap();
        assert!(!right_root.join("previously-ignored.txt").exists());
        let current = left.find_pair(&left_pair.id).unwrap();
        left.sync_pair_inner_at_mode(&current, port, true).unwrap();
        assert_eq!(
            fs::read(right_root.join("previously-ignored.txt")).unwrap(),
            b"existing content"
        );
        assert!(left
            .find_pair(&left_pair.id)
            .unwrap()
            .initial_files
            .is_empty());
        assert!(right
            .find_pair(&left_pair.id)
            .unwrap()
            .initial_files
            .is_empty());
        receiver.join().unwrap();
        fs::remove_dir_all(base).unwrap();
    }
}
