use std::path::PathBuf;
use windows::core::{w, HSTRING};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW, DICD_GENERATE_ID,
    DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, INSTALLFLAG_FORCE,
    SPDRP_DRIVER, SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, SW_SHOWNORMAL};

const HARDWARE_ID: &str = "Root\\NerveDeskDisplay";
const INSTANCE_PREFIX: &str = "ROOT\\NERVEDESKDISPLAY\\";

fn package_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let base = exe
        .parent()
        .ok_or("Cannot find NerveDesk executable directory")?;
    let dir = [base.join("resources").join("driver"), base.join("driver")]
        .into_iter()
        .find(|candidate| candidate.join("NerveDeskDisplay.inf").is_file())
        .ok_or("Signed NerveDesk driver package is not bundled with this build")?;
    for name in [
        "NerveDeskDisplay.inf",
        "NerveDeskDisplay.dll",
        "NerveDeskDisplay.cat",
    ] {
        if !dir.join(name).is_file() {
            return Err(format!(
                "Signed NerveDesk driver package is incomplete: {}",
                dir.display()
            ));
        }
    }
    Ok(dir)
}

pub fn package_available() -> bool {
    package_dir().is_ok()
}

pub fn elevate(operation: &str) -> Result<String, String> {
    if operation == "install" {
        package_dir()?;
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = HSTRING::from(exe.to_string_lossy().as_ref());
    let arg = if operation == "install" {
        w!("--install-nervedesk-driver")
    } else {
        w!("--remove-nervedesk-driver")
    };
    let result = unsafe { ShellExecuteW(None, w!("runas"), &exe, arg, None, SW_SHOWNORMAL) };
    if result.0 as isize <= 32 {
        return Err(format!(
            "Windows did not start driver {} (ShellExecute code {})",
            operation, result.0 as isize
        ));
    }
    Ok(format!("NerveDesk driver {} started", operation))
}

fn device_set() -> Result<windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO, String> {
    unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), None, None, DIGCF_PRESENT) }
        .map_err(|e| e.to_string())
}

fn find_device() -> Result<
    Option<(
        windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
        SP_DEVINFO_DATA,
    )>,
    String,
> {
    let set = device_set()?;
    for index in 0..256 {
        let mut data = SP_DEVINFO_DATA::default();
        data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(set, index, &mut data) }.is_err() {
            break;
        }
        let mut id = [0u16; 512];
        if unsafe { SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut id), None) }.is_ok() {
            let end = id.iter().position(|&c| c == 0).unwrap_or(id.len());
            if String::from_utf16_lossy(&id[..end])
                .to_uppercase()
                .starts_with(INSTANCE_PREFIX)
            {
                return Ok(Some((set, data)));
            }
        }
    }
    let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    Ok(None)
}

pub fn is_installed() -> bool {
    match find_device() {
        Ok(Some((set, data))) => {
            let mut driver_key = [0u8; 512];
            let installed = unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    set,
                    &data,
                    SPDRP_DRIVER,
                    None,
                    Some(&mut driver_key),
                    None,
                )
            }
            .is_ok();
            let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
            installed
        }
        _ => false,
    }
}

fn install() -> Result<(), String> {
    let dir = package_dir()?;
    let existing = find_device()?;
    let needs_device = existing.is_none();
    if let Some((set, _)) = existing {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    }
    if needs_device {
        let set = unsafe { SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_DISPLAY), None) }
            .map_err(|e| e.to_string())?;
        let mut data = SP_DEVINFO_DATA::default();
        data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
        let result = (|| -> windows::core::Result<()> {
            unsafe {
                SetupDiCreateDeviceInfoW(
                    set,
                    w!("NerveDeskDisplay"),
                    &GUID_DEVCLASS_DISPLAY,
                    None,
                    None,
                    DICD_GENERATE_ID,
                    Some(&mut data),
                )?;
                let mut hardware_id: Vec<u16> = HARDWARE_ID.encode_utf16().collect();
                hardware_id.extend_from_slice(&[0, 0]);
                let bytes = std::slice::from_raw_parts(
                    hardware_id.as_ptr() as *const u8,
                    hardware_id.len() * 2,
                );
                SetupDiSetDeviceRegistryPropertyW(set, &mut data, SPDRP_HARDWAREID, Some(bytes))?;
                SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, Some(&data))?;
            }
            Ok(())
        })();
        let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
        result.map_err(|e| e.to_string())?;
    }
    let inf = HSTRING::from(dir.join("NerveDeskDisplay.inf").to_string_lossy().as_ref());
    let result = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            None,
            w!("Root\\NerveDeskDisplay"),
            &inf,
            INSTALLFLAG_FORCE,
            None,
        )
    };
    if result.is_err() && needs_device {
        let _ = remove();
    }
    result.map_err(|e| format!("Windows rejected the NerveDesk driver package: {e}"))
}

fn remove() -> Result<(), String> {
    let Some((set, data)) = find_device()? else {
        return Ok(());
    };
    let result = unsafe { SetupDiCallClassInstaller(DIF_REMOVE, set, Some(&data)) };
    let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    result.map_err(|e| e.to_string())
}

pub fn handle_cli() -> bool {
    let operation = match std::env::args().nth(1).as_deref() {
        Some("--install-nervedesk-driver") => install,
        Some("--remove-nervedesk-driver") => remove,
        _ => return false,
    };
    if let Err(error) = operation() {
        let message = HSTRING::from(error);
        unsafe {
            MessageBoxW(None, &message, w!("NerveDesk driver"), MB_OK | MB_ICONERROR);
        }
        std::process::exit(1);
    }
    true
}
