; OpenPicasa · Windows NSIS 安装脚本（x64 与 arm64 共用，由 make_win.sh 传 -D 定义）
;
; 设计取舍：
; - 装到 $LOCALAPPDATA\Programs\OpenPicasa（HKCU，免管理员、免 UAC 弹窗）。
;   个人相册工具不需要写 Program Files；这也让 x86 的 NSIS 安装器在 arm64
;   Windows 的模拟层里跑时不会踩注册表重定向。
; - 卸载信息写 HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall，
;   与安装位置同为当前用户，卸载时不提权。
; - 图标用 dist/OpenPicasa.ico（7 档），快捷方式与卸载程序都带图标。
;
; 由脚本注入的定义：
;   -DAPP_VERSION=0.4.1-Beta2   -DARCH=x64|arm64
;   -DOUT_FILE=<setup.exe 路径>  -DBIN_FILE=<OpenPicasa.exe 路径>
;   -DICO_FILE=<OpenPicasa.ico 路径>

!include "MUI2.nsh"

Name "OpenPicasa ${APP_VERSION} (${ARCH})"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\OpenPicasa"
InstallDirRegKey HKCU "Software\OpenPicasa" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Unicode true

!define MUI_ICON "${ICO_FILE}"
!define MUI_UNICON "${ICO_FILE}"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSE_FILE}"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "OpenPicasa" SEC01
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "/oname=OpenPicasa.exe" "${BIN_FILE}"
  File "/oname=OpenPicasa.ico" "${ICO_FILE}"

  ; README / 许可证随包
  SetOutPath "$INSTDIR\docs"
  File /nonfatal "..\README.md"

  ; 开始菜单快捷方式（HKCU 上下文，免管理员）
  CreateDirectory "$SMPROGRAMS\OpenPicasa"
  CreateShortCut "$SMPROGRAMS\OpenPicasa\OpenPicasa.lnk" "$INSTDIR\OpenPicasa.exe" "" "$INSTDIR\OpenPicasa.ico" 0
  CreateShortCut "$SMPROGRAMS\OpenPicasa\卸载 OpenPicasa.lnk" "$INSTDIR\Uninstall.exe"

  ; 记录安装位置与版本
  WriteRegStr HKCU "Software\OpenPicasa" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\OpenPicasa" "Version" "${APP_VERSION}"
  WriteRegStr HKCU "Software\OpenPicasa" "Arch" "${ARCH}"

  ; 卸载程序 + 卸载注册表项（HKCU，免管理员）
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "DisplayName" "OpenPicasa ${APP_VERSION} (${ARCH})"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "DisplayIcon" "$\"$INSTDIR\OpenPicasa.ico$\""
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "Publisher" "OpenPicasa contributors"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "DisplayVersion" "${APP_VERSION}"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\OpenPicasa.exe"
  Delete "$INSTDIR\OpenPicasa.ico"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\OpenPicasa\OpenPicasa.lnk"
  Delete "$SMPROGRAMS\OpenPicasa\卸载 OpenPicasa.lnk"
  RMDir "$SMPROGRAMS\OpenPicasa"
  RMDir /r "$INSTDIR\docs"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenPicasa"
  ; 安装时写入的三个值必须逐个删除，否则 /ifempty 永远不成立：
  ; 卸载后 HKCU\Software\OpenPicasa 会残留（Windows ARM64 安装/卸载实测复现）。
  DeleteRegValue HKCU "Software\OpenPicasa" "InstallDir"
  DeleteRegValue HKCU "Software\OpenPicasa" "Version"
  DeleteRegValue HKCU "Software\OpenPicasa" "Arch"
  DeleteRegKey /ifempty HKCU "Software\OpenPicasa"
SectionEnd
