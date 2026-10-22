#[cfg(target_os = "windows")]
const RULE_NAME: &str = "NerveDesk Private LAN";

#[cfg(target_os = "windows")]
pub fn handle_cli() -> bool {
    if std::env::args().nth(1).as_deref() != Some("--configure-nervedesk-firewall") {
        return false;
    }

    if let Err(error) = configure_private_lan() {
        use windows::core::{w, HSTRING};
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let message = HSTRING::from(error);
        unsafe { MessageBoxW(None, &message, w!("NerveDesk network access"), MB_OK | MB_ICONERROR) };
        std::process::exit(1);
    }
    true
}

#[cfg(not(target_os = "windows"))]
pub fn handle_cli() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn configure_private_lan() -> Result<(), String> {
    use std::process::Command;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let program = format!("program={}", exe.display());
    let rule = format!("name={RULE_NAME}");
    // An old rule can still point to a portable executable after installation.
    let _ = Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule", &rule])
        .status();
    let status = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            &rule,
            "dir=in",
            "action=allow",
            &program,
            "profile=private",
            "remoteip=localsubnet",
            "enable=yes",
        ])
        .status()
        .map_err(|e| format!("Could not run Windows Firewall configuration: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Windows Firewall did not accept the NerveDesk private-network rule. Check administrator permissions and try again.".into())
    }
}

#[cfg(target_os = "windows")]
pub fn request_private_lan() -> Result<(), String> {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = HSTRING::from(exe.to_string_lossy().as_ref());
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &exe,
            w!("--configure-nervedesk-firewall"),
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err("Windows did not start the network access setup. Allow the administrator prompt and try again.".into());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn request_private_lan() -> Result<(), String> {
    Err("Windows Firewall is only available on Windows".into())
}
