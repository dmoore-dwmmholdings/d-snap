; D-Snap installer hooks (DSNA-75). Per-user install, so only the user PATH is touched.
;
; The program installs to %LOCALAPPDATA%\D-Snap; data lives in its own subfolder,
; %LOCALAPPDATA%\D-Snap\data, which Tauri's uninstaller never touches (it removes only the
; files it installed).
;
; Install: offer to put the install folder (it holds dsnap.exe) on the user PATH. Silent
; installs add it. Uninstall: remove that PATH entry, and delete the snapshots only if the
; user says so (default: keep them).

!macro NSIS_HOOK_POSTINSTALL
  IfSilent dsnap_add_path
  MessageBox MB_YESNO|MB_ICONQUESTION "Add the dsnap command-line tool to your PATH?$\r$\n$\r$\n(Needed for the Claude Code hooks shown in Settings.)" IDNO dsnap_skip_path
  dsnap_add_path:
    nsExec::Exec `powershell -NoProfile -ExecutionPolicy Bypass -Command "$$d='$INSTDIR'; $$p=[Environment]::GetEnvironmentVariable('Path','User'); $$parts=@($$p -split ';' | Where-Object { $$_ }); if ($$parts -notcontains $$d) { [Environment]::SetEnvironmentVariable('Path', (($$parts + $$d) -join ';'), 'User') }"`
    Pop $0
  dsnap_skip_path:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::Exec `powershell -NoProfile -ExecutionPolicy Bypass -Command "$$d='$INSTDIR'; $$p=[Environment]::GetEnvironmentVariable('Path','User'); if ($$p) { $$parts=@($$p -split ';' | Where-Object { $$_ -and $$_ -ne $$d }); [Environment]::SetEnvironmentVariable('Path', ($$parts -join ';'), 'User') }"`
  Pop $0
  IfSilent dsnap_keep_data
  MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "Also delete all D-Snap snapshots in $LOCALAPPDATA\D-Snap\data?$\r$\n$\r$\nThis cannot be undone. Choose No to keep them." IDNO dsnap_keep_data
    RMDir /r "$LOCALAPPDATA\D-Snap\data"
    RMDir "$LOCALAPPDATA\D-Snap"
  dsnap_keep_data:
!macroend
