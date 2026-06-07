; Custom NSIS hooks for AllTheThings.
; Register an elevated logon scheduled task on install (so the app auto-starts
; with admin rights into the system tray), and remove it on uninstall. The
; per-machine installer already runs elevated, which `/rl highest` requires.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec 'schtasks /create /tn "AllTheThings" /tr "\"$INSTDIR\${MAINBINARYNAME}.exe\" --minimized" /sc onlogon /rl highest /f'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'schtasks /delete /tn "AllTheThings" /f'
!macroend
