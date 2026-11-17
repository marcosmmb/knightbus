Unicode true
!include "MUI2.nsh"
!include "x64.nsh"
!define PRODUCT "KnightBus"
!define VERSION "0.1.0"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\KnightBus"
Name "${PRODUCT}"
OutFile "${OUTPUT}"
InstallDir "$PROGRAMFILES64\KnightBus"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show
Icon "${ICON}"
UninstallIcon "${ICON}"
VIProductVersion "0.1.0.0"
VIAddVersionKey "ProductName" "KnightBus"
VIAddVersionKey "FileDescription" "KnightBus setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "KnightBus contributors"
!define MUI_WELCOMEPAGE_TEXT "Install or update KnightBus and its background service for this Windows account.$\r$\n$\r$\nDisconnect USB devices and choose Quit UI in the tray before upgrading. Pairing data is retained. The signed USB/IP driver is included.$\r$\n$\r$\nThis development build is unsigned."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PAYLOAD}\LICENSE"
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TEXT "KnightBus app and service are installed. Open the app from Start.$\r$\n$\r$\nThe setup guide is in C:\Program Files\KnightBus\INSTALL.md.$\r$\n$\r$\nThe Microsoft-signed USB/IP driver is installed. Restart Windows if setup requested it."
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Function .onInit
 ${IfNot} ${RunningX64}
  MessageBox MB_ICONSTOP "This package requires 64-bit Windows."
  Abort
 ${EndIf}
 SetRegView 64
 ; Elevated task executables must stay in the protected Program Files directory.
 StrCpy $INSTDIR "$PROGRAMFILES64\KnightBus"
FunctionEnd
Section "Install"
 InitPluginsDir
 SetOutPath "$PLUGINSDIR\payload"
 File /r "${PAYLOAD}\*"
 ${DisableX64FSRedirection}
 nsExec::ExecToLog '"$WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$PLUGINSDIR\payload\install-windows.ps1"'
 Pop $0
 ${EnableX64FSRedirection}
 ${If} $0 != 0
  SetErrorLevel 1
  MessageBox MB_ICONSTOP "Setup failed. See the Details panel for the reason and INSTALL.md in the extracted ZIP for help." /SD IDOK
  Abort
 ${EndIf}
 SetOutPath "$INSTDIR"
 WriteUninstaller "$INSTDIR\Uninstall.exe"
 WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
 WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
 WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\knightbus-desktop.exe"
 WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
 WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
 WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
 WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd
Section "Uninstall"
 SetRegView 64
 ${DisableX64FSRedirection}
 nsExec::ExecToLog '"$WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\uninstall-windows.ps1"'
 Pop $0
 ${EnableX64FSRedirection}
 ${If} $0 != 0
  SetErrorLevel 1
  MessageBox MB_ICONSTOP "Uninstall stopped. See Details; close the UI and let device connections finish before retrying." /SD IDOK
  Abort
 ${EndIf}
 DeleteRegKey HKLM "${UNINSTALL_KEY}"
 Delete "$INSTDIR\Uninstall.exe"
 RMDir "$INSTDIR"
SectionEnd
