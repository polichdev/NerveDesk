use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

const VIDEO_NAME: windows::core::PCWSTR = w!("OBSVirtualCamVideo");
const FRAME_HEADER_SIZE: usize = 32;
const MAX_DIM: usize = 1920;
const MAX_FRAME_SIZE: usize = MAX_DIM * MAX_DIM * 3 / 2;

#[repr(C)]
struct QueueHeader {
    write_idx: u32,
    read_idx: u32,
    state: u32,
    offsets: [u32; 3],
    queue_type: u32,
    cx: u32,
    cy: u32,
    interval: u64,
    reserved: [u32; 8],
}

fn align_32(val: usize) -> usize {
    (val + 31) & !31
}

pub struct VirtualCamWriter {
    handle: HANDLE,
    header_ptr: *mut QueueHeader,
    offsets: [usize; 3],
}

unsafe impl Send for VirtualCamWriter {}
unsafe impl Sync for VirtualCamWriter {}

impl VirtualCamWriter {
    pub fn new(fps: u32) -> Result<Self, String> {
        Self::new_named(VIDEO_NAME, fps)
    }

    pub fn new_named(name: windows::core::PCWSTR, fps: u32) -> Result<Self, String> {
        let mut size = align_32(std::mem::size_of::<QueueHeader>());
        let offset_0 = size;
        size = align_32(size + MAX_FRAME_SIZE + FRAME_HEADER_SIZE);
        let offset_1 = size;
        size = align_32(size + MAX_FRAME_SIZE + FRAME_HEADER_SIZE);
        let offset_2 = size;
        size = align_32(size + MAX_FRAME_SIZE + FRAME_HEADER_SIZE);
        let total_size = size;

        let interval = if fps > 0 {
            10_000_000 / (fps as u64)
        } else {
            166_666
        };

        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                total_size as u32,
                name,
            )
        }
        .map_err(|e| format!("CreateFileMappingW failed: {e}"))?;

        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err("MapViewOfFile returned null".into());
        }

        let header_ptr = view.Value as *mut QueueHeader;
        unsafe {
            std::ptr::write(
                header_ptr,
                QueueHeader {
                    write_idx: 0,
                    read_idx: 0,
                    state: 2,
                    offsets: [offset_0 as u32, offset_1 as u32, offset_2 as u32],
                    queue_type: 0,
                    cx: 1280,
                    cy: 720,
                    interval,
                    reserved: [0; 8],
                },
            );

            for &off in &[offset_0, offset_1, offset_2] {
                let base = (header_ptr as *mut u8).add(off);
                let ts_ptr = base as *mut u64;
                std::ptr::write_unaligned(ts_ptr, 0);
                let frame_ptr = base.add(FRAME_HEADER_SIZE);
                let y_len = 1280 * 720;
                let uv_len = y_len / 2;
                std::ptr::write_bytes(frame_ptr, 16, y_len);
                std::ptr::write_bytes(frame_ptr.add(y_len), 128, uv_len);
            }
        }

        Ok(Self {
            handle,
            header_ptr,
            offsets: [offset_0, offset_1, offset_2],
        })
    }

    pub fn write_transformed_rgb(
        &mut self,
        rgb: &[u8],
        src_w: u32,
        src_h: u32,
        rotation: u32,
        mirrored: bool,
        timestamp: u64,
    ) {
        let sw = src_w as usize;
        let sh = src_h as usize;
        if rgb.len() < sw * sh * 3 {
            return;
        }

        let is_rot90 = rotation == 90 || rotation == 270;
        let (raw_w, raw_h) = if is_rot90 { (sh, sw) } else { (sw, sh) };
        let out_w = (raw_w & !1).max(2);
        let out_h = (raw_h & !1).max(2);
        let y_len = out_w * out_h;
        let uv_len = y_len / 2;

        unsafe {
            let header = &mut *self.header_ptr;

            let next_write = header.write_idx.wrapping_add(1);
            let idx = (next_write % 3) as usize;
            let off = self.offsets[idx];

            let base = (self.header_ptr as *mut u8).add(off);
            let ts_ptr = base as *mut u64;
            std::ptr::write_unaligned(ts_ptr, timestamp);

            let frame_ptr = base.add(FRAME_HEADER_SIZE);
            let y_plane = std::slice::from_raw_parts_mut(frame_ptr, y_len);
            let uv_plane = std::slice::from_raw_parts_mut(frame_ptr.add(y_len), uv_len);

            for y_out in 0..out_h {
                let is_even_row = (y_out & 1) == 0;
                let uv_row_offset = (y_out >> 1) * out_w;
                let y_row_offset = y_out * out_w;

                for x_out in 0..out_w {
                    let (x_src, y_src) = match rotation {
                        90 => {
                            let xs = if mirrored { sw.saturating_sub(1 + y_out) } else { y_out };
                            let ys = sh.saturating_sub(1 + x_out);
                            (xs, ys)
                        }
                        180 => {
                            let xs = if mirrored { x_out } else { sw.saturating_sub(1 + x_out) };
                            let ys = sh.saturating_sub(1 + y_out);
                            (xs, ys)
                        }
                        270 => {
                            let xs = if mirrored { y_out } else { sw.saturating_sub(1 + y_out) };
                            let ys = x_out;
                            (xs, ys)
                        }
                        _ => {
                            let xs = if mirrored { sw.saturating_sub(1 + x_out) } else { x_out };
                            let ys = y_out;
                            (xs, ys)
                        }
                    };

                    let px = (y_src * sw + x_src) * 3;
                    if px + 2 >= rgb.len() {
                        continue;
                    }
                    let r = rgb[px] as i32;
                    let g = rgb[px + 1] as i32;
                    let b = rgb[px + 2] as i32;

                    let y_val = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
                    y_plane[y_row_offset + x_out] = y_val as u8;

                    if is_even_row && (x_out & 1) == 0 {
                        let u_val = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                        let v_val = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                        let uv_idx = uv_row_offset + x_out;
                        if uv_idx + 1 < uv_len {
                            uv_plane[uv_idx] = u_val as u8;
                            uv_plane[uv_idx + 1] = v_val as u8;
                        }
                    }
                }
            }

            header.cx = out_w as u32;
            header.cy = out_h as u32;
            header.state = 2;
            header.read_idx = next_write;
            std::sync::atomic::fence(Ordering::Release);
            header.write_idx = next_write;
        }
    }

    pub fn write_jpeg(
        &mut self,
        jpeg_bytes: &[u8],
        rotation: u32,
        mirrored: bool,
        timestamp: u64,
    ) -> Result<(), String> {
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(jpeg_bytes));
        decoder.set_max_decoding_buffer_size(3840 * 2160 * 3);
        let rgb = decoder.decode().map_err(|e| e.to_string())?;
        let info = decoder.info().ok_or("Missing JPEG info")?;
        let sw = info.width as u32;
        let sh = info.height as u32;

        self.write_transformed_rgb(&rgb, sw, sh, rotation, mirrored, timestamp);
        Ok(())
    }
}

impl Drop for VirtualCamWriter {
    fn drop(&mut self) {
        unsafe {
            if !self.header_ptr.is_null() {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.header_ptr as *mut _,
                });
            }
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

struct VcamShared {
    pending: Mutex<Option<(Arc<Vec<u8>>, u32, u32, u32)>>,
    transform: Mutex<(u32, bool)>,
    condvar: Condvar,
    running: AtomicBool,
}

pub struct VirtualCameraService {
    shared: Arc<VcamShared>,
}

impl VirtualCameraService {
    pub fn new() -> Self {
        let shared = Arc::new(VcamShared {
            pending: Mutex::new(None),
            transform: Mutex::new((0, false)),
            condvar: Condvar::new(),
            running: AtomicBool::new(true),
        });

        let worker_shared = shared.clone();
        std::thread::spawn(move || {
            let mut writer: Option<VirtualCamWriter> = VirtualCamWriter::new(30).ok();
            while worker_shared.running.load(Ordering::SeqCst) {
                let item = {
                    let mut guard = worker_shared.pending.lock().unwrap();
                    while guard.is_none() && worker_shared.running.load(Ordering::SeqCst) {
                        guard = worker_shared.condvar.wait(guard).unwrap();
                    }
                    guard.take()
                };

                let (rotation, mirrored) = *worker_shared.transform.lock().unwrap();

                if let Some((frame, _w, _h, _fps)) = item {
                    if writer.is_none() {
                        writer = VirtualCamWriter::new(30).ok();
                    }
                    if let Some(wr) = &mut writer {
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64 / 100)
                            .unwrap_or(0);
                        let _ = wr.write_jpeg(&frame, rotation, mirrored, ts);
                    }
                }
            }
        });

        Self { shared }
    }

    pub fn push_jpeg(&self, jpeg_bytes: Arc<Vec<u8>>, width: u32, height: u32, fps: u32) {
        {
            let mut guard = self.shared.pending.lock().unwrap();
            *guard = Some((jpeg_bytes, width, height, fps));
        }
        self.shared.condvar.notify_one();
    }

    pub fn set_transform(&self, rotation: u32, mirrored: bool) {
        if let Ok(mut guard) = self.shared.transform.lock() {
            *guard = (rotation, mirrored);
        }
        self.shared.condvar.notify_one();
    }
}

impl Drop for VirtualCameraService {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        self.shared.condvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcam_writer_create_and_write() {
        let mut writer = VirtualCamWriter::new_named(w!("Local\\TestNerveDeskVCam"), 30).unwrap();
        let rgb = vec![128u8; 320 * 240 * 3];
        writer.write_transformed_rgb(&rgb, 320, 240, 90, false, 1000);
        unsafe {
            assert_eq!((*writer.header_ptr).state, 2);
            assert_eq!((*writer.header_ptr).cx, 240);
            assert_eq!((*writer.header_ptr).cy, 320);
            assert_eq!((*writer.header_ptr).write_idx, 1);
            assert_eq!((*writer.header_ptr).read_idx, 1);
        }
    }
}
