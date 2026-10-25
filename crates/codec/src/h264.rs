use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use windows::core::{Interface, VARIANT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};

fn find_mft_transform(
    category: windows::core::GUID,
    in_fmt: windows::core::GUID,
    out_fmt: windows::core::GUID,
) -> Option<IMFTransform> {
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
        let flags = MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0;
        if MFTEnumEx(
            category,
            MFT_ENUM_FLAG(flags),
            Some(&input_type),
            Some(&output_type),
            &mut activates,
            &mut count,
        )
        .is_ok()
            && count > 0
            && !activates.is_null()
        {
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
    fps: u32,
) -> Result<IMFMediaType, String> {
    unsafe {
        let ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|e| e.to_string())?;
        ty.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)
            .map_err(|e| e.to_string())?;
        ty.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)
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

fn refresh_decoder_output(transform: &IMFTransform) -> Result<(), String> {
    unsafe {
        for index in 0..16 {
            let Ok(ty) = transform.GetOutputAvailableType(0, index) else {
                break;
            };
            if ty.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                return transform
                    .SetOutputType(0, &ty, 0)
                    .map_err(|e| e.to_string());
            }
        }
    }
    Err("H.264 decoder does not offer NV12 output".into())
}

fn sample(bytes: &[u8], time: i64, duration: i64) -> Result<IMFSample, String> {
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
            .SetSampleDuration(duration)
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
                let sample = produced.ok_or("H.264 transform returned no sample")?;
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

fn bgra_to_nv12(width: u32, height: u32, bgra: &[u8]) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let total_len = w * h * 3 / 2;
    let mut out = Vec::with_capacity(total_len);
    unsafe {
        out.set_len(total_len);
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
    let total = w * h * 4;
    let mut out = Vec::with_capacity(total);
    unsafe {
        out.set_len(total);
    }
    let uv_plane = stride * coded_height;
    let p_out: *mut u8 = out.as_mut_ptr();
    let p_nv12 = nv12.as_ptr();

    for y in 0..h {
        let y_row = y * stride;
        let uv_row = uv_plane + (y / 2) * stride;
        let out_row = y * w * 4;

        for x in (0..w).step_by(2) {
            let u = unsafe { *p_nv12.add(uv_row + x) as i32 - 128 };
            let v = unsafe { *p_nv12.add(uv_row + x + 1) as i32 - 128 };

            let r_coeff = 409 * v + 128;
            let g_coeff = -100 * u - 208 * v + 128;
            let b_coeff = 516 * u + 128;

            let y0 = unsafe { (*p_nv12.add(y_row + x) as i32 - 16).max(0) * 298 };
            let y1 = unsafe { (*p_nv12.add(y_row + x + 1) as i32 - 16).max(0) * 298 };

            let p0 = out_row + x * 4;
            let p1 = p0 + 4;

            unsafe {
                *p_out.add(p0) = ((y0 + b_coeff) >> 8).clamp(0, 255) as u8;
                *p_out.add(p0 + 1) = ((y0 + g_coeff) >> 8).clamp(0, 255) as u8;
                *p_out.add(p0 + 2) = ((y0 + r_coeff) >> 8).clamp(0, 255) as u8;
                *p_out.add(p0 + 3) = 255;

                *p_out.add(p1) = ((y1 + b_coeff) >> 8).clamp(0, 255) as u8;
                *p_out.add(p1 + 1) = ((y1 + g_coeff) >> 8).clamp(0, 255) as u8;
                *p_out.add(p1 + 2) = ((y1 + r_coeff) >> 8).clamp(0, 255) as u8;
                *p_out.add(p1 + 3) = 255;
            }
        }
    }
    Ok(out)
}

pub struct Encoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    fps: u32,
    frame: i64,
}

impl Encoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let bitrate_bps = (width as u64 * height as u64 * 3).clamp(800_000, 16_000_000) as u32;
        Self::new_with_fps_and_bitrate(width, height, 60, bitrate_bps)
    }

    pub fn new_with_bitrate(width: u32, height: u32, bitrate_bps: u32) -> Result<Self, String> {
        Self::new_with_fps_and_bitrate(width, height, 60, bitrate_bps)
    }

    pub fn new_with_fps_and_bitrate(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Result<Self, String> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("H.264 requires even dimensions".into());
        }
        if !(800_000..=50_000_000).contains(&bitrate_bps) {
            return Err("H.264 bitrate is out of range".into());
        }
        let fps = fps.clamp(30, 240);
        unsafe {
            let transform: IMFTransform = match find_mft_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFVideoFormat_NV12,
                MFVideoFormat_H264,
            ) {
                Some(hw) => hw,
                None => CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| e.to_string())?,
            };
            low_latency(&transform);
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(0u32));
                let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
                let _ = api.SetValue(&CODECAPI_AVEncCommonQualityVsSpeed, &VARIANT::from(100u32));
            }
            let out = media_type(&MFVideoFormat_H264, width, height, fps)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)
                .map_err(|e| e.to_string())?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                .map_err(|e| e.to_string())?;
            transform
                .SetOutputType(0, &out, 0)
                .map_err(|e| e.to_string())?;
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate_bps));
                let _ = api.SetValue(&CODECAPI_AVEncCommonQualityVsSpeed, &VARIANT::from(100u32));
            }
            transform
                .SetInputType(0, &media_type(&MFVideoFormat_NV12, width, height, fps)?, 0)
                .map_err(|e| e.to_string())?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| e.to_string())?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| e.to_string())?;
            Ok(Self {
                transform,
                width,
                height,
                fps,
                frame: 0,
            })
        }
    }

    pub fn encode(&mut self, bgra: &[u8]) -> Result<Vec<u8>, String> {
        if bgra.len() != self.width as usize * self.height as usize * 4 {
            return Err("Invalid BGRA frame".into());
        }
        let nv12 = bgra_to_nv12(self.width, self.height, bgra);
        let frame_duration_100ns = 10_000_000 / self.fps as i64;
        unsafe {
            self.transform
                .ProcessInput(0, &sample(&nv12, self.frame * frame_duration_100ns, frame_duration_100ns)?, 0)
                .map_err(|e| e.to_string())?;
        }
        self.frame += 1;
        let mut bytes = Vec::new();
        for _ in 0..8 {
            match output(&self.transform)? {
                Output::Data(part) => bytes.extend(part),
                Output::MoreInput => break,
                Output::StreamChange => return Err("H.264 encoder stream changed".into()),
            }
        }
        Ok(bytes)
    }
}

pub struct Decoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    frame: i64,
}

impl Decoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        unsafe {
            let transform: IMFTransform = match find_mft_transform(
                MFT_CATEGORY_VIDEO_DECODER,
                MFVideoFormat_H264,
                MFVideoFormat_NV12,
            ) {
                Some(hw) => hw,
                None => CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| e.to_string())?,
            };
            low_latency(&transform);
            transform
                .SetInputType(0, &media_type(&MFVideoFormat_H264, width, height, 60)?, 0)
                .map_err(|e| e.to_string())?;
            transform
                .SetOutputType(0, &media_type(&MFVideoFormat_NV12, width, height, 60)?, 0)
                .map_err(|e| e.to_string())?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| e.to_string())?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| e.to_string())?;
            Ok(Self {
                transform,
                width,
                height,
                frame: 0,
            })
        }
    }

    pub fn decode(&mut self, encoded: &[u8]) -> Result<Option<Vec<u8>>, String> {
        unsafe {
            self.transform
                .ProcessInput(0, &sample(encoded, self.frame * 166_667, 166_667)?, 0)
                .map_err(|e| e.to_string())?;
        }
        self.frame += 1;
        for _ in 0..3 {
            match output(&self.transform)? {
                Output::Data(nv12) => {
                    let stride = unsafe {
                        self.transform
                            .GetOutputCurrentType(0)
                            .ok()
                            .and_then(|ty| ty.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok())
                            .unwrap_or(self.width) as usize
                    };
                    return nv12_to_bgra(self.width, self.height, stride, &nv12).map(Some);
                }
                Output::MoreInput => return Ok(None),
                Output::StreamChange => refresh_decoder_output(&self.transform)?,
            }
        }
        Err("H.264 decoder output kept changing".into())
    }
}
