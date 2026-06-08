; Custom NSIS hooks for AllTheThings (per-machine installer; runs elevated).
;
; On install: register and start the LocalSystem background index **service**
; (the indexer, so the GUI runs unelevated), and create a logon **task** that
; auto-launches the GUI into the tray. The task is NOT elevated — the service
; does the indexing, so the auto-started GUI just queries it. On uninstall: stop
; + remove the service and the task. A service step that fails is surfaced in the
; install log but does not abort the install (the user can install it later from
; Settings, and the migration banner nudges them).

!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --svc-install' $0
  ${If} $0 != 0
    DetailPrint "Warning: background service install failed ($0); install it later from Settings."
  ${EndIf}
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --svc-start' $1
  ${If} $1 != 0
    DetailPrint "Warning: background service did not start ($1)."
  ${EndIf}
  ; Unelevated logon auto-launch into the tray.
  nsExec::Exec 'schtasks /create /tn "AllTheThings" /tr "\"$INSTDIR\${MAINBINARYNAME}.exe\" --minimized" /sc onlogon /f'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --svc-stop'
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --svc-uninstall'
  nsExec::Exec 'schtasks /delete /tn "AllTheThings" /f'
!macroend
