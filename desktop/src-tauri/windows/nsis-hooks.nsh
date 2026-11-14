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
; Windows Firewall (opt in, off by default): a checkbox on the welcome page
; asks for an inbound allow rule for the installed app, so peers can
; connect while it seeds. The install runs without administrator rights,
; so the rule is added through an elevated netsh (Windows asks for
; approval; declining just skips it). The rule is named after this
; installer kind and limited to the app's path. Uninstalling removes it
; (again with approval); updates keep it. Silent and passive installs
; skip the page and add no rule.
;
; The functions below run before the template defines MAINBINARYNAME, so
; anything that needs it lives in the hook macros.
;
; Never touched: the Tensorseed config (%APPDATA%\tensorseed), data and
; state (%LOCALAPPDATA%\tensorseed), downloads, and mirrors.

!define TS_FW_RULE "Tensorseed Desktop incoming peers (per user)"

Var TsFwCheck
Var TsFwWanted

!define MUI_PAGE_CUSTOMFUNCTION_SHOW TsFwWelcomeShow
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE TsFwWelcomeLeave

Function TsFwWelcomeShow
  ; Below the welcome text, and on top of its label in the z-order.
  ${NSD_CreateCheckbox} 120u 166u 195u 22u "Allow incoming peer connections in Windows Firewall, for seeding (asks for administrator approval)"
  Pop $TsFwCheck
  SetCtlColors $TsFwCheck "" "FFFFFF"
  System::Call "user32::SetWindowPos(p $TsFwCheck, p 0, i 0, i 0, i 0, i 0, i 3)"
  ${NSD_SetState} $TsFwCheck $TsFwWanted
FunctionEnd

Function TsFwWelcomeLeave
  ${NSD_GetState} $TsFwCheck $TsFwWanted
FunctionEnd

!macro NSIS_HOOK_POSTINSTALL
  ${If} $UpdateMode <> 1
  ${AndIf} $TsFwWanted == ${BST_CHECKED}
    DetailPrint "Adding the Windows Firewall rule (asks for approval)"
    ExecShellWait "runas" "$SYSDIR\cmd.exe" '/c netsh advfirewall firewall delete rule name="${TS_FW_RULE}" program="$INSTDIR\${MAINBINARYNAME}.exe" >nul & netsh advfirewall firewall add rule name="${TS_FW_RULE}" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes profile=any' SW_HIDE
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    nsExec::Exec 'netsh advfirewall firewall show rule name="${TS_FW_RULE}"'
    Pop $R8
    ${If} $R8 == 0
      DetailPrint "Removing the Windows Firewall rule (asks for approval)"
      ExecShellWait "runas" "$SYSDIR\netsh.exe" 'advfirewall firewall delete rule name="${TS_FW_RULE}" program="$INSTDIR\${MAINBINARYNAME}.exe"' SW_HIDE
    ${EndIf}

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
