!macro NSIS_HOOK_POSTINSTALL
  MessageBox MB_YESNO|MB_ICONQUESTION "Will this PC send an extended desktop to another computer? Select Yes to install the bundled NerveDesk display driver. On the receiving laptop, select No." IDNO driver_done
  DetailPrint "Installing NerveDesk display driver..."
  ExecWait '"$INSTDIR\NerveDesk.exe" --install-nervedesk-driver' $0
  IntCmp $0 0 driver_done
  MessageBox MB_ICONSTOP|MB_OK "NerveDesk display driver installation failed. The application is installed, but extended display mode is unavailable."
driver_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\NerveDesk.exe" --remove-nervedesk-driver' $0
!macroend
