#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn terminate_other_instances() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    let my_pid = std::process::id();
    let my_exe_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "nervedesk.exe".to_string());

    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return,
        };

        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        let mut killed_pids = Vec::new();

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(&entry.szExeFile)
                    .trim_matches('\0')
                    .to_lowercase();

                let is_match =
                    name == my_exe_name || name == "nervedesk.exe" || name == "nerve-desk.exe";

                if is_match && entry.th32ProcessID != my_pid {
                    killed_pids.push(entry.th32ProcessID);
                    if let Ok(process_handle) =
                        OpenProcess(PROCESS_TERMINATE, false, entry.th32ProcessID)
                    {
                        let _ = TerminateProcess(process_handle, 0);
                        let _ = CloseHandle(process_handle);
                    }
                }

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        if !killed_pids.is_empty() {
            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    if killed_pids.contains(&entry.th32ParentProcessID) {
                        if let Ok(child_handle) =
                            OpenProcess(PROCESS_TERMINATE, false, entry.th32ProcessID)
                        {
                            let _ = TerminateProcess(child_handle, 0);
                            let _ = CloseHandle(child_handle);
                        }
                    }
                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
            std::thread::sleep(std::time::Duration::from_millis(500));
        } else {
            let _ = CloseHandle(snapshot);
        }
    }
}

fn main() {
    if nervedesk_ui_lib::handle_firewall_cli() {
        return;
    }
    if nervedesk_ui_lib::handle_driver_cli() {
        return;
    }
    #[cfg(target_os = "windows")]
    terminate_other_instances();

    nervedesk_ui_lib::run();
}
