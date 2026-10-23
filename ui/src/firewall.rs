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
