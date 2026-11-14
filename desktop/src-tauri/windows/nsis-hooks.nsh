; SPDX-License-Identifier: AGPL-3.0-or-later
; Copyright 2026 Tuned Chaos
;
; NSIS installer hooks (bundle.windows.nsis.installerHooks in
; tauri.conf.json). Native NSIS commands only. NSIS_HOOK_POSTUNINSTALL runs
; after Tauri's own uninstall steps.
;
; Install location: the installer stores $INSTDIR as the default value of
; HKCU\Software\Tuned Chaos\Tensorseed Desktop, next to "Installer
; Language". Tauri removes them only when "Delete the application data" is
; ticked, and the MSI reads that default value as its install folder, so
; an MSI installed later would reuse the old NSIS folder. Remove both
; values on every uninstall that is not an update, then the key and the
; Tuned Chaos key when nothing else uses them (an installed MSI keeps its
; own values there). Updates keep them: the new installer reinstalls into
; that folder.
;
; WebView2 profile (%LOCALAPPDATA%\dev.tensorseed.desktop): removed only
; when the checkbox is ticked and this is not an update. Tauri tries this
; too; WebView2 helper processes can still hold files for a moment after
; the window closes, so retry for up to five seconds.
;
; Never touched: the Tensorseed config (%APPDATA%\tensorseed), data and
; state (%LOCALAPPDATA%\tensorseed), downloads, and mirrors.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue SHCTX "${MANUPRODUCTKEY}" ""
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
    DeleteRegKey /ifempty SHCTX "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty SHCTX "${MANUKEY}"

    ${If} $DeleteAppDataCheckboxState = 1
      SetShellVarContext current
      StrCpy $R9 0
      ${Do}
        RMDir /r "$LOCALAPPDATA\${BUNDLEID}"
        ${IfNot} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\*.*"
          ${Break}
        ${EndIf}
        IntOp $R9 $R9 + 1
        ${If} $R9 >= 10
          DetailPrint "Could not remove $LOCALAPPDATA\${BUNDLEID}"
          ${Break}
        ${EndIf}
        Sleep 500
      ${Loop}
    ${EndIf}
  ${EndIf}
!macroend
