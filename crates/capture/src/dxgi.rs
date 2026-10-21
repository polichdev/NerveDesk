use std::time::Instant;
use tracing::{info, warn};

use common::{NerveError, Result};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

#[derive(Debug, Clone)]
pub struct DxgiCaptureConfig {
    pub adapter_index: u32,
    pub output_index: u32,
    pub timeout_ms: u32,
}

impl Default for DxgiCaptureConfig {
    fn default() -> Self {
        Self {
            adapter_index: 0,
            output_index: 0,
            timeout_ms: 16,
        }
    }
}

impl DxgiCaptureConfig {
    pub fn primary_display() -> Result<Self> {
        let factory: IDXGIFactory1 = unsafe {
            CreateDXGIFactory1()
                .map_err(|e| NerveError::Platform(format!("Failed to create DXGI factory: {e}")))?
        };
        let mut first_attached = None;
        for adapter_index in 0..32 {
            let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
                Ok(adapter) => adapter,
                Err(_) => break,
            };
            for output_index in 0..32 {
                let output = match unsafe { adapter.EnumOutputs(output_index) } {
                    Ok(output) => output,
                    Err(_) => break,
                };
                let desc = unsafe { output.GetDesc() }
                    .map_err(|e| NerveError::Platform(format!("Failed to inspect DXGI output: {e}")))?;
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let config = Self { adapter_index, output_index, timeout_ms: 16 };
                if desc.DesktopCoordinates.left == 0 && desc.DesktopCoordinates.top == 0 {
                    return Ok(config);
                }
                if first_attached.is_none() {
                    first_attached = Some(config);
                }
            }
        }
        first_attached.ok_or_else(|| NerveError::Platform("No active desktop display found".into()))
    }
}

pub struct GpuFrame<'a> {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub format: DXGI_FORMAT,
    pub frame_info: DXGI_OUTDUPL_FRAME_INFO,
    pub acquire_duration: std::time::Duration,
    duplicator: &'a DxgiOutputDuplicator,
}

impl<'a> GpuFrame<'a> {
    pub fn copy_to_cpu(&self, dest: &mut [u8]) -> Result<usize> {
        let expected_size = (self.width * self.height * 4) as usize;
        if dest.len() < expected_size {
            return Err(NerveError::Platform(format!(
                "Destination buffer too small: required {}, provided {}",
                expected_size,
                dest.len()
            )));
        }

        unsafe {
            self.duplicator.context.CopyResource(
                &self.duplicator.staging_texture,
                &self.texture,
            );

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.duplicator
                .context
                .Map(
                    &self.duplicator.staging_texture,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped),
                )
                .map_err(|e| NerveError::Platform(format!("Failed to map staging texture: {e}")))?;

            let src_slice = std::slice::from_raw_parts(
                mapped.pData as *const u8,
                (mapped.RowPitch * self.height) as usize,
            );

            let row_size = (self.width * 4) as usize;
            for row in 0..self.height as usize {
                let src_offset = row * mapped.RowPitch as usize;
                let dst_offset = row * row_size;
                dest[dst_offset..dst_offset + row_size]
                    .copy_from_slice(&src_slice[src_offset..src_offset + row_size]);
            }

            self.duplicator
                .context
                .Unmap(&self.duplicator.staging_texture, 0);

            Ok(expected_size)
        }
    }
}

impl<'a> Drop for GpuFrame<'a> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.duplicator.duplication.ReleaseFrame();
        }
    }
}

pub struct DxgiOutputDuplicator {
    config: DxgiCaptureConfig,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging_texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl DxgiOutputDuplicator {
    pub fn new(config: DxgiCaptureConfig) -> Result<Self> {
        let (device, context) = Self::create_d3d11_device(config.adapter_index)?;
        let (duplication, width, height) =
            Self::create_duplication(&device, config.adapter_index, config.output_index)?;
        let staging_texture = Self::create_staging_texture(&device, width, height)?;

        info!(
            "DXGI Output Duplication initialized: {}x{} on adapter={}, output={}",
            width, height, config.adapter_index, config.output_index
        );

        Ok(Self {
            config,
            device,
            context,
            duplication,
            staging_texture,
            width,
            height,
        })
    }

    pub fn acquire_frame(&mut self) -> Result<Option<GpuFrame<'_>>> {
        let start = Instant::now();
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;

        let res = unsafe {
            self.duplication.AcquireNextFrame(
                self.config.timeout_ms,
                &mut frame_info,
                &mut resource,
            )
        };

        match res {
            Ok(()) => {
                let resource = match resource {
                    Some(r) => r,
                    None => return Ok(None),
                };

                let texture: ID3D11Texture2D = resource
                    .cast()
                    .map_err(|e| NerveError::Platform(format!("Failed to cast resource to ID3D11Texture2D: {e}")))?;

                let acquire_duration = start.elapsed();

                Ok(Some(GpuFrame {
                    texture,
                    width: self.width,
                    height: self.height,
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    frame_info,
                    acquire_duration,
                    duplicator: self,
                }))
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                Ok(None)
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                warn!("DXGI access lost (resolution change or desktop switch). Reinitializing duplication...");
                self.reinitialize()?;
                Ok(None)
            }
            Err(e) => Err(NerveError::Platform(format!(
                "AcquireNextFrame failed: {e}"
            ))),
        }
    }

    pub fn reinitialize(&mut self) -> Result<()> {
        let (duplication, width, height) = Self::create_duplication(
            &self.device,
            self.config.adapter_index,
            self.config.output_index,
        )?;

        if width != self.width || height != self.height {
            info!("Resolution changed to {}x{}", width, height);
            self.staging_texture = Self::create_staging_texture(&self.device, width, height)?;
            self.width = width;
            self.height = height;
        }

        self.duplication = duplication;
        info!("DXGI Output Duplication successfully reinitialized");
        Ok(())
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    fn create_d3d11_device(adapter_index: u32) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
        let factory: IDXGIFactory1 = unsafe {
            CreateDXGIFactory1()
                .map_err(|e| NerveError::Platform(format!("Failed to create DXGI factory: {e}")))?
        };

        let adapter = unsafe {
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|e| NerveError::Platform(format!("Failed to enumerate adapter {adapter_index}: {e}")))?
        };

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_0];

        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| NerveError::Platform(format!("D3D11CreateDevice failed: {e}")))?;
        }

        let device = device.ok_or_else(|| NerveError::Platform("Device pointer null".into()))?;
        let context = context.ok_or_else(|| NerveError::Platform("Context pointer null".into()))?;

        Ok((device, context))
    }

    fn create_duplication(
        device: &ID3D11Device,
        adapter_index: u32,
        output_index: u32,
    ) -> Result<(IDXGIOutputDuplication, u32, u32)> {
        let factory: IDXGIFactory1 = unsafe {
            CreateDXGIFactory1()
                .map_err(|e| NerveError::Platform(format!("Failed to create DXGI factory: {e}")))?
        };

        let adapter: IDXGIAdapter1 = unsafe {
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|e| NerveError::Platform(format!("Failed to get adapter {adapter_index}: {e}")))?
        };

        let output: IDXGIOutput = unsafe {
            adapter
                .EnumOutputs(output_index)
                .map_err(|e| NerveError::Platform(format!("Failed to get output {output_index}: {e}")))?
        };

        let output1: IDXGIOutput1 = output
            .cast()
            .map_err(|e| NerveError::Platform(format!("Failed to cast output to IDXGIOutput1: {e}")))?;

        let desc = unsafe {
            output
                .GetDesc()
                .map_err(|e| NerveError::Platform(format!("Failed to get output desc: {e}")))?
        };

        let width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left).unsigned_abs();
        let height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top).unsigned_abs();

        let duplication: IDXGIOutputDuplication = unsafe {
            output1
                .DuplicateOutput(device)
                .map_err(|e| NerveError::Platform(format!("DuplicateOutput failed: {e}")))?
        };

        Ok((duplication, width, height))
    }

    fn create_staging_texture(
        device: &ID3D11Device,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe {
            device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(|e| NerveError::Platform(format!("Failed to create staging texture: {e}")))?;
        }

        texture.ok_or_else(|| NerveError::Platform("Staging texture pointer null".into()))
    }
}
