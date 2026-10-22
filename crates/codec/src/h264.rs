use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use windows::core::{Interface, VARIANT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
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
        ty.SetUINT64(&MF_MT_FRAME_RATE, (30u64 << 32) | 1)
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
            .SetSampleDuration(333_333)
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
    let mut out = vec![0u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            let p = (y * w + x) * 4;
            let b = bgra[p] as i32;
            let g = bgra[p + 1] as i32;
            let r = bgra[p + 2] as i32;
            out[y * w + x] = ((66 * r + 129 * g + 25 * b + 128) >> 8)
                .saturating_add(16)
                .clamp(0, 255) as u8;
            if y % 2 == 0 && x % 2 == 0 {
                let pixels = [p, p + 4, p + w * 4, p + w * 4 + 4];
                let b = pixels.iter().map(|&i| bgra[i] as i32).sum::<i32>() / 4;
                let g = pixels.iter().map(|&i| bgra[i + 1] as i32).sum::<i32>() / 4;
                let r = pixels.iter().map(|&i| bgra[i + 2] as i32).sum::<i32>() / 4;
                let uv = w * h + y / 2 * w + x;
                out[uv] = ((-38 * r - 74 * g + 112 * b + 128) >> 8)
                    .saturating_add(128)
                    .clamp(0, 255) as u8;
                out[uv + 1] = ((112 * r - 94 * g - 18 * b + 128) >> 8)
                    .saturating_add(128)
                    .clamp(0, 255) as u8;
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

pub struct Encoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    frame: i64,
}

impl Encoder {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("H.264 requires even dimensions".into());
        }
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| e.to_string())?;
            low_latency(&transform);
            let out = media_type(&MFVideoFormat_H264, width, height)?;
            out.SetUINT32(
                &MF_MT_AVG_BITRATE,
                (width as u64 * height as u64 * 3).clamp(800_000, 16_000_000) as u32,
            )
            .map_err(|e| e.to_string())?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                .map_err(|e| e.to_string())?;
            transform
                .SetOutputType(0, &out, 0)
                .map_err(|e| e.to_string())?;
            transform
                .SetInputType(0, &media_type(&MFVideoFormat_NV12, width, height)?, 0)
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

    pub fn encode(&mut self, bgra: &[u8]) -> Result<Vec<u8>, String> {
        if bgra.len() != self.width as usize * self.height as usize * 4 {
            return Err("Invalid BGRA frame".into());
        }
        let nv12 = bgra_to_nv12(self.width, self.height, bgra);
        unsafe {
            self.transform
                .ProcessInput(0, &sample(&nv12, self.frame * 333_333)?, 0)
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
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| e.to_string())?;
            low_latency(&transform);
            transform
                .SetInputType(0, &media_type(&MFVideoFormat_H264, width, height)?, 0)
                .map_err(|e| e.to_string())?;
            transform
                .SetOutputType(0, &media_type(&MFVideoFormat_NV12, width, height)?, 0)
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
                .ProcessInput(0, &sample(encoded, self.frame * 333_333)?, 0)
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
