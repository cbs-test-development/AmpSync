; AmpSync installer hooks (bundle.windows.nsis.installerHooks).
;
; The uninstaller's check box ("Also remove my sign-ins, settings and download history ...") is
; Tauri's built-in "delete app data" box, checked by default in windows/installer.nsi. When it is
; ticked, everything AmpSync stored for the user is removed: Windows Credential Manager entries,
; settings, job state and download history, the sign-in profile and the WebView cache. Downloaded
; files are never touched.
;
; The work is done by the app itself (`ampsync-desktop.exe --purge`, see src/purge.rs) because
; only it knows every location and credential name. The uninstaller cannot ask for the box in
; silent mode, so silent uninstalls and updates (/S, /UPDATE, /P) never purge. Scripted purge:
;   uninstall.exe /S /REMOVEALL

!macro NSIS_HOOK_PREUNINSTALL
  ; Explicit request on the command line (for IT scripts); the check box is not shown with /S.
  ${GetOptions} $CMDLINE "/REMOVEALL" $R8
  ${IfNot} ${Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}

  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; Never purge under a running AmpSync: offer to close it first (Cancel aborts the uninstall).
    !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
    ${If} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
      ; Waits for the purge to finish. The exe is still here: it is deleted right after this hook.
      ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --purge'
    ${EndIf}
  ${EndIf}
!macroend
