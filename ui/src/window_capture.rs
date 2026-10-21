use serde::Serialize;
use windows::core::Interface;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::{DirectXPixelFormat, Direct3D11::IDirect3DDevice};
use windows::Win32::Foundation::{BOOL, HMODULE, HWND, LPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsWindow, IsWindowVisible,
};

#[derive(Clone, Serialize)]
pub struct CaptureWindow {
    pub id: String,
    pub title: String,
}

pub fn list_windows() -> Vec<CaptureWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 || len > 512 {
            return BOOL(1);
        }
        let mut title = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut title);
        if copied > 0 {
            let title = String::from_utf16_lossy(&title[..copied as usize]);
            let windows = &mut *(lparam.0 as *mut Vec<CaptureWindow>);
            windows.push(CaptureWindow { id: (hwnd.0 as isize).to_string(), title });
        }
        BOOL(1)
    }

    let mut windows = Vec::new();
    unsafe { let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize)); }
    windows.sort_by(|a: &CaptureWindow, b: &CaptureWindow| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    windows
}

pub struct WindowCapture {
    hwnd: HWND,
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    staging: Option<(u32, u32, ID3D11Texture2D)>,
    pool_size: (i32, i32),
}

pub struct CaptureApartment;

impl CaptureApartment {
    pub fn initialize() -> Result<Self, String> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|e| e.to_string())?;
        Ok(Self)
    }
}

impl Drop for CaptureApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize(); }
    }
}

impl WindowCapture {
    pub fn new(window_id: &str) -> Result<Self, String> {
        let id = window_id.parse::<isize>().map_err(|_| "Invalid window ID")?;
        let hwnd = HWND(id as *mut _);
        if !unsafe { IsWindow(hwnd) }.as_bool() {
            return Err("The selected window has closed".into());
        }
        if !GraphicsCaptureSession::IsSupported().map_err(|e| e.to_string())? {
            return Err("Windows Graphics Capture is unavailable".into());
        }
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| e.to_string())?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }
            .map_err(|e| format!("Cannot capture selected window: {e}"))?;
        let size = item.Size().map_err(|e| e.to_string())?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err("Selected window has no visible content".into());
        }

        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }.map_err(|e| format!("Cannot create capture device: {e}"))?;
        let device: ID3D11Device = device.ok_or("Missing capture device")?;
        let context = context.ok_or("Missing capture context")?;
        let dxgi: IDXGIDevice = device.cast().map_err(|e| e.to_string())?;
        let winrt_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .map_err(|e| e.to_string())?
            .cast().map_err(|e| e.to_string())?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size,
        ).map_err(|e| e.to_string())?;
        let session = pool.CreateCaptureSession(&item).map_err(|e| e.to_string())?;
        let _ = session.SetIsCursorCaptureEnabled(true);
        session.StartCapture().map_err(|e| e.to_string())?;

        Ok(Self {
            hwnd, _item: item, pool, session, device, context, winrt_device,
            staging: None, pool_size: (size.Width, size.Height),
        })
    }

    pub fn window_offset(&self) -> (i16, i16) {
        let mut rect = windows::Win32::Foundation::RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rect) }.is_ok() {
            (
                rect.left.clamp(-32768, 32767) as i16,
                rect.top.clamp(-32768, 32767) as i16,
            )
        } else {
            (0, 0)
        }
    }

    pub fn next_frame(&mut self) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
        if !unsafe { IsWindow(self.hwnd) }.as_bool() {
            return Err("The selected window has closed".into());
        }
        let frame = match self.pool.TryGetNextFrame() {
            Ok(frame) => frame,
            Err(_) => return Ok(None),
        };
        let size = frame.ContentSize().map_err(|e| e.to_string())?;
        if size.Width <= 0 || size.Height <= 0 || size.Width > 7680 || size.Height > 4320 {
            return Ok(None);
        }

        // Recreate pool if the window size has changed
        if self.pool_size != (size.Width, size.Height) {
            let _ = self.pool.Recreate(
                &self.winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            );
            self.pool_size = (size.Width, size.Height);
            return Ok(None);
        }

        let surface = frame.Surface().map_err(|e| e.to_string())?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| e.to_string())?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|e| e.to_string())?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc); }
        let width = size.Width as u32;
        let height = size.Height as u32;
        if width > desc.Width || height > desc.Height {
            let _ = self.pool.Recreate(
                &self.winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            );
            self.pool_size = (size.Width, size.Height);
            return Ok(None);
        }
        if self.staging.as_ref().map(|(w, h, _)| (*w, *h)) != Some((desc.Width, desc.Height)) {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            unsafe { self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
                .map_err(|e| e.to_string())?;
            self.staging = Some((desc.Width, desc.Height, staging.ok_or("Missing staging texture")?));
        }
        let staging = &self.staging.as_ref().unwrap().2;
        unsafe { self.context.CopyResource(staging, &texture); }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(|e| e.to_string())?;
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        let row_bytes = width as usize * 4;
        unsafe {
            for row in 0..height as usize {
                let src = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
                let dst = bgra.as_mut_ptr().add(row * row_bytes);
                std::ptr::copy_nonoverlapping(src, dst, row_bytes);
            }
            self.context.Unmap(staging, 0);
        }
        drop(frame);
        drop(texture);
        drop(access);
        drop(surface);
        Ok(Some((width, height, bgra)))
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
