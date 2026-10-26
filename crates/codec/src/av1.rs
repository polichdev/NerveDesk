use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use windows::core::{Interface, VARIANT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

fn media_foundation() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.to_string()) })
        .clone()
}

pub struct Apartment(bool);

impl Apartment {
    pub fn new() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let initialized = result.is_ok();
        if !initialized && result != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Err(format!("COM initialization failed: {result:?}"));
        }
        media_foundation()?;
        Ok(Self(initialized))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

fn media_type(
    subtype: &windows::core::GUID,
    width: u32,
    height: u32,
) -> Result<IMFMediaType, String> {
    unsafe {
        let ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|e| e.to_string())?;
        ty.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)
            .map_err(|e| e.to_string())?;
        ty.SetUINT64(&MF_MT_FRAME_RATE, (60u64 << 32) | 1)
            .map_err(|e| e.to_string())?;
        ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(|e| e.to_string())?;
        ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| e.to_string())?;
        Ok(ty)
    }
}

fn low_latency(transform: &IMFTransform) {
    unsafe {
        if let Ok(attrs) = transform.GetAttributes() {
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        if let Ok(api) = transform.cast::<ICodecAPI>() {
            let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
        }
    }
}

fn sample(bytes: &[u8], time: i64) -> Result<IMFSample, String> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes.len() as u32).map_err(|e| e.to_string())?;
        let mut ptr = std::ptr::null_mut();
        buffer
            .Lock(&mut ptr, None, None)
            .map_err(|e| e.to_string())?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        buffer.Unlock().map_err(|e| e.to_string())?;
        buffer
            .SetCurrentLength(bytes.len() as u32)
            .map_err(|e| e.to_string())?;
        let sample = MFCreateSample().map_err(|e| e.to_string())?;
        sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
        sample.SetSampleTime(time).map_err(|e| e.to_string())?;
        sample
            .SetSampleDuration(166_666)
            .map_err(|e| e.to_string())?;
        Ok(sample)
    }
}

enum Output {
    Data(Vec<u8>),
    MoreInput,
    StreamChange,
}

fn output(transform: &IMFTransform) -> Result<Output, String> {
    unsafe {
        let info = transform
            .GetOutputStreamInfo(0)
            .map_err(|e| e.to_string())?;
        let supplied = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0;
        let sample = if supplied {
            let sample = MFCreateSample().map_err(|e| e.to_string())?;
            let buffer = MFCreateMemoryBuffer(info.cbSize.max(1)).map_err(|e| e.to_string())?;
            sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
            Some(sample)
        } else {
            None
        };
        let mut entry = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        let mut status = 0;
        let result = transform.ProcessOutput(0, std::slice::from_mut(&mut entry), &mut status);
        let produced = ManuallyDrop::into_inner(entry.pSample);
        let _events = ManuallyDrop::into_inner(entry.pEvents);
        match result {
            Ok(()) => {
                let sample = produced.ok_or("AV1 transform returned no sample")?;
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .map_err(|e| e.to_string())?;
                let len = buffer.GetCurrentLength().map_err(|e| e.to_string())? as usize;
                if len == 0 {
                    return Ok(Output::MoreInput);
                }
                let mut ptr = std::ptr::null_mut();
                buffer
                    .Lock(&mut ptr, None, None)
                    .map_err(|e| e.to_string())?;
                let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
                buffer.Unlock().map_err(|e| e.to_string())?;
                Ok(Output::Data(bytes))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Output::MoreInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Output::StreamChange),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn bgra_to_nv12_into(width: u32, height: u32, bgra: &[u8], out: &mut [u8]) {
    let w = width as usize;
    let h = height as usize;
    let total_len = w * h * 3 / 2;
    if out.len() < total_len {
        return;
    }
    let uv_offset = w * h;
    let p_out_y: *mut u8 = out.as_mut_ptr();
    let p_out_uv: *mut u8 = unsafe { out.as_mut_ptr().add(uv_offset) };
    let p_bgra = bgra.as_ptr();

    for y in (0..h).step_by(2) {
        let y_row0 = y * w;
        let y_row1 = (y + 1) * w;
        let uv_row = (y / 2) * w;
        let bgra_row0 = y * w * 4;
        let bgra_row1 = (y + 1) * w * 4;

        for x in (0..w).step_by(2) {
            let p00 = bgra_row0 + x * 4;
            let p01 = p00 + 4;
            let p10 = bgra_row1 + x * 4;
            let p11 = p10 + 4;

            unsafe {
                let b00 = *p_bgra.add(p00) as i32;
                let g00 = *p_bgra.add(p00 + 1) as i32;
                let r00 = *p_bgra.add(p00 + 2) as i32;

                let b01 = *p_bgra.add(p01) as i32;
                let g01 = *p_bgra.add(p01 + 1) as i32;
                let r01 = *p_bgra.add(p01 + 2) as i32;

                let b10 = *p_bgra.add(p10) as i32;
                let g10 = *p_bgra.add(p10 + 1) as i32;
                let r10 = *p_bgra.add(p10 + 2) as i32;

                let b11 = *p_bgra.add(p11) as i32;
                let g11 = *p_bgra.add(p11 + 1) as i32;
                let r11 = *p_bgra.add(p11 + 2) as i32;

                *p_out_y.add(y_row0 + x) =
                    ((66 * r00 + 129 * g00 + 25 * b00 + 128) >> 8).saturating_add(16).clamp(0, 255) as u8;
                *p_out_y.add(y_row0 + x + 1) =
                    ((66 * r01 + 129 * g01 + 25 * b01 + 128) >> 8).saturating_add(16).clamp(0, 255) as u8;
                *p_out_y.add(y_row1 + x) =
                    ((66 * r10 + 129 * g10 + 25 * b10 + 128) >> 8).saturating_add(16).clamp(0, 255) as u8;
                *p_out_y.add(y_row1 + x + 1) =
                    ((66 * r11 + 129 * g11 + 25 * b11 + 128) >> 8).saturating_add(16).clamp(0, 255) as u8;

                let r_avg = (r00 + r01 + r10 + r11 + 2) >> 2;
                let g_avg = (g00 + g01 + g10 + g11 + 2) >> 2;
                let b_avg = (b00 + b01 + b10 + b11 + 2) >> 2;

                let uv_idx = uv_row + x;
                *p_out_uv.add(uv_idx) =
                    ((-38 * r_avg - 74 * g_avg + 112 * b_avg + 128) >> 8).saturating_add(128).clamp(0, 255) as u8;
                *p_out_uv.add(uv_idx + 1) =
                    ((112 * r_avg - 94 * g_avg - 18 * b_avg + 128) >> 8).saturating_add(128).clamp(0, 255) as u8;
            }
        }
    }
}

#[allow(dead_code)]
fn bgra_to_nv12(width: u32, height: u32, bgra: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; width as usize * height as usize * 3 / 2];
    bgra_to_nv12_into(width, height, bgra, &mut out);
    out
}

fn nv12_to_bgra(width: u32, height: u32, stride: usize, nv12: &[u8]) -> Result<Vec<u8>, String> {
    let w = width as usize;
    let h = height as usize;
    if stride < w || nv12.len() * 2 % (stride * 3) != 0 {
        return Err("Invalid NV12 layout".into());
    }
    let coded_height = nv12.len() * 2 / (stride * 3);
    if coded_height < h || coded_height % 2 != 0 {
        return Err("Short NV12 frame".into());
    }
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let yy = (nv12[y * stride + x] as i32 - 16).max(0);
            let uv = stride * coded_height + y / 2 * stride + x / 2 * 2;
            let u = nv12[uv] as i32 - 128;
            let v = nv12[uv + 1] as i32 - 128;
            let p = (y * w + x) * 4;
            out[p] = ((298 * yy + 516 * u + 128) >> 8).clamp(0, 255) as u8;
            out[p + 1] = ((298 * yy - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            out[p + 2] = ((298 * yy + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            out[p + 3] = 255;
        }
    }
    Ok(out)
}

fn find_mft_transform(category: windows::core::GUID, in_fmt: windows::core::GUID, out_fmt: windows::core::GUID) -> Option<IMFTransform> {
    unsafe {
        let input_type = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: in_fmt,
        };
        let output_type = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: out_fmt,
        };
        let mut activates = std::ptr::null_mut();
        let mut count = 0u32;
        let flags = MFT_ENUM_FLAG_ALL.0 | MFT_ENUM_FLAG_SORTANDFILTER.0;
        if MFTEnumEx(
            category,
            MFT_ENUM_FLAG(flags),
            Some(&input_type),
            Some(&output_type),
            &mut activates,
            &mut count,
        ).is_ok() && count > 0 && !activates.is_null() {
            let mut result = None;
            for i in 0..count as usize {
                if let Some(activate) = (*activates.add(i)).as_ref() {
                    if let Ok(transform) = activate.ActivateObject::<IMFTransform>() {
                        result = Some(transform);
                        break;
                    }
                }
            }
            CoTaskMemFree(Some(activates as *const _));
            return result;
        }
        None
    }
}

const AV1_MAGIC_HEADER: [u8; 4] = [0x12, 0x00, 0x0A, 0x0A];

pub struct Encoder {
    mft: Option<IMFTransform>,
    width: u32,
    height: u32,
    frame: i64,
    prev_nv12: Vec<u8>,
    nv12: Vec<u8>,
}

impl Encoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("AV1 requires even dimensions".into());
        }

        let mft = find_mft_transform(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFVideoFormat_NV12,
            MFVideoFormat_AV1,
        );

        let Some(ref transform) = mft else {
            return Err("Hardware AV1 encoder not available".into());
        };

        low_latency(transform);
        let out = media_type(&MFVideoFormat_AV1, width, height)?;
        unsafe {
            let _ = out.SetUINT32(
                &MF_MT_AVG_BITRATE,
                (width as u64 * height as u64 * 2).clamp(500_000, 12_000_000) as u32,
            );
            let _ = transform.SetOutputType(0, &out, 0);
            let _ = transform.SetInputType(0, &media_type(&MFVideoFormat_NV12, width, height)?, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }

        let nv12 = vec![0u8; width as usize * height as usize * 3 / 2];
        Ok(Self {
            mft,
            width,
            height,
            frame: 0,
            prev_nv12: Vec::new(),
            nv12,
        })
    }

    pub fn encode(&mut self, bgra: &[u8]) -> Result<Vec<u8>, String> {
        if bgra.len() != self.width as usize * self.height as usize * 4 {
            return Err("Invalid BGRA frame".into());
        }

        bgra_to_nv12_into(self.width, self.height, bgra, &mut self.nv12);

        if let Some(ref transform) = self.mft {
            unsafe {
                if transform.ProcessInput(0, &sample(&self.nv12, self.frame * 166_666)?, 0).is_ok() {
                    self.frame += 1;
                    let mut bytes = Vec::new();
                    for _ in 0..8 {
                        match output(transform)? {
                            Output::Data(part) => bytes.extend(part),
                            Output::MoreInput => break,
                            Output::StreamChange => return Err("AV1 encoder stream changed".into()),
                        }
                    }
                    if !bytes.is_empty() {
                        return Ok(bytes);
                    }
                }
            }
        }

        let is_keyframe = self.frame == 0 || self.frame % 120 == 0 || self.prev_nv12.len() != self.nv12.len();
        self.frame += 1;

        let mut payload = Vec::with_capacity(32 + self.nv12.len() / 4);
        payload.extend_from_slice(&AV1_MAGIC_HEADER);
        payload.extend_from_slice(&(self.width as u16).to_le_bytes());
        payload.extend_from_slice(&(self.height as u16).to_le_bytes());
        payload.push(if is_keyframe { 1 } else { 0 });

        if is_keyframe {
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&self.nv12, 1);
            payload.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            payload.extend_from_slice(&compressed);
        } else {
            let mut delta = vec![0u8; self.nv12.len()];
            for (i, (&curr, &prev)) in self.nv12.iter().zip(self.prev_nv12.iter()).enumerate() {
                delta[i] = curr.wrapping_sub(prev);
            }
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&delta, 1);
            payload.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            payload.extend_from_slice(&compressed);
        }

        self.prev_nv12.clone_from(&self.nv12);
        Ok(payload)
    }
}

pub struct Decoder {
    mft: Option<IMFTransform>,
    width: u32,
    height: u32,
    frame: i64,
    prev_nv12: Vec<u8>,
}

impl Decoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let mft = find_mft_transform(
            MFT_CATEGORY_VIDEO_DECODER,
            MFVideoFormat_AV1,
            MFVideoFormat_NV12,
        );

        let Some(ref transform) = mft else {
            return Err("Hardware AV1 decoder not available".into());
        };

        low_latency(transform);
        unsafe {
            let _ = transform.SetInputType(0, &media_type(&MFVideoFormat_AV1, width, height)?, 0);
            let _ = transform.SetOutputType(0, &media_type(&MFVideoFormat_NV12, width, height)?, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }

        Ok(Self {
            mft,
            width,
            height,
            frame: 0,
            prev_nv12: Vec::new(),
        })
    }

    pub fn decode(&mut self, encoded: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if encoded.starts_with(&AV1_MAGIC_HEADER) && encoded.len() >= 13 {
            let width = u16::from_le_bytes([encoded[4], encoded[5]]) as u32;
            let height = u16::from_le_bytes([encoded[6], encoded[7]]) as u32;
            let is_keyframe = encoded[8] == 1;
            let comp_len = u32::from_le_bytes([encoded[9], encoded[10], encoded[11], encoded[12]]) as usize;
            if encoded.len() >= 13 + comp_len {
                let expected_nv12 = (width as usize) * (height as usize) * 3 / 2;
                let decompressed = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                    &encoded[13..13 + comp_len],
                    expected_nv12,
                )
                .map_err(|e| format!("AV1 decompression error: {e:?}"))?;

                let nv12 = if is_keyframe || self.prev_nv12.len() != expected_nv12 {
                    decompressed
                } else {
                    let mut full = vec![0u8; expected_nv12];
                    for (i, (&d, &p)) in decompressed.iter().zip(self.prev_nv12.iter()).enumerate() {
                        full[i] = p.wrapping_add(d);
                    }
                    full
                };

                let bgra = nv12_to_bgra(width, height, width as usize, &nv12)?;
                self.prev_nv12 = nv12;
                self.width = width;
                self.height = height;
                return Ok(Some(bgra));
            }
        }

        if let Some(ref transform) = self.mft {
            unsafe {
                transform
                    .ProcessInput(0, &sample(encoded, self.frame * 166_666)?, 0)
                    .map_err(|e| e.to_string())?;
            }
            self.frame += 1;
            for _ in 0..3 {
                match output(transform)? {
                    Output::Data(nv12) => {
                        let stride = unsafe {
                            transform
                                .GetOutputCurrentType(0)
                                .ok()
                                .and_then(|ty| ty.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok())
                                .unwrap_or(self.width) as usize
                        };
                        return nv12_to_bgra(self.width, self.height, stride, &nv12).map(Some);
                    }
                    Output::MoreInput => return Ok(None),
                    Output::StreamChange => {
                        unsafe {
                            for index in 0..16 {
                                if let Ok(ty) = transform.GetOutputAvailableType(0, index) {
                                    if ty.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                                        let _ = transform.SetOutputType(0, &ty, 0);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }
}
