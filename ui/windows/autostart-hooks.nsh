!macro NSIS_HOOK_POSTINSTALL
  ; Preserve the user's existing autostart choice when upgrading from a portable build.
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "NerveDesk"
  StrCmp $0 "" autostart_install_done
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "NerveDesk" '"$INSTDIR\NerveDesk.exe" --minimized'
autostart_install_done:
  MessageBox MB_YESNO|MB_ICONQUESTION "NerveDesk needs access on your private network to find your other computer. Allow Windows to add this network rule?" IDNO firewall_install_done
  ExecShell "runas" "$INSTDIR\NerveDesk.exe" "--configure-nervedesk-firewall"
firewall_install_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Leave a different NerveDesk installation's autostart entry alone.
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "NerveDesk"
  StrCmp $0 '"$INSTDIR\NerveDesk.exe" --minimized' 0 autostart_uninstall_done
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "NerveDesk"
autostart_uninstall_done:
!macroend
