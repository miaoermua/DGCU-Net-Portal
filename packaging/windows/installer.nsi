; DGCU-Net-Portal 的 Windows 安装器。
;
; 装到当前用户的 Programs 目录，不写 Program Files、不提权：应用本身的后台服务
; 也是当前用户级的登录启动任务（crates/portal-cli/src/service.rs 的 TASK），
; 安装器跟它保持同一权限层级，装完即可用。
;
; 由 scripts/package_windows.py 通过 -D 传入下列量，不在这里写死：
;   VERSION     语义版本，例如 0.5.13
;   VERSION4    四段版本，VIProductVersion 只接受这种形式，例如 0.5.13.0
;   ARCH        包名里用的架构名，例如 x86_64
;   BINARYDIR   暂存目录，内含两个 exe 与 LICENSE
;   ICONFILE    安装器图标
;   OUTFILE     输出文件名

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

; 高分屏：不声明 DPI 感知时 Windows 会把整个向导当位图整体拉伸，中文发虚。
; PerMonitorV2 要 Win10 1703+，更老的系统按列表里的 System 回落；
; dpiAware 那行管 Vista 到 Win10 1607。ManifestDPIAwareness 需要 NSIS 3.03+。
; 本安装器没有自定义位图（未定义 MUI_HEADERIMAGE / 欢迎页图），所以不存在
; NSIS 用最近邻算法缩放位图导致糊图的老问题。
ManifestDPIAware true
ManifestDPIAwareness "PerMonitorV2,System"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "x64.nsh"
; 桌面快捷方式要「默认勾选、但可以取消」，NSIS 没有直接语法，用这里的宏实现。
!include "Sections.nsh"
!include "Win\RestartManager.nsh"

!ifndef VERSION
  !error "缺少 VERSION"
!endif
!ifndef VERSION4
  !error "缺少 VERSION4"
!endif
!ifndef ARCH
  !error "缺少 ARCH"
!endif
!ifndef BINARYDIR
  !error "缺少 BINARYDIR"
!endif
!ifndef OUTFILE
  !define OUTFILE "DGCU-Net-Portal-${VERSION}-windows-${ARCH}-setup.exe"
!endif

!define PRODUCT_NAME "DGCU-Net-Portal"
!define PRODUCT_PUBLISHER "DGCU"
!define PRODUCT_URL "https://github.com/miaoermua/DGCU-Net-Portal"
; 两个可执行文件必须装在同一目录：GUI 沿自身路径的祖先目录找同包的 daemon。
!define GUI_EXE "portal-gui.exe"
!define CLI_EXE "portal-cli.exe"
; 应用在运行时用这个名字注册登录启动任务，卸载时按同一名字删除。
!define TASK_NAME "DGCU-Portal"
!define APP_KEY "Software\DGCU\${PRODUCT_NAME}"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"

Name "${PRODUCT_NAME} ${VERSION}"
OutFile "${OUTFILE}"
BrandingText "${PRODUCT_NAME} ${VERSION}"

; 装到当前用户的 Programs 目录；InstallDirRegKey 让升级时自动回到上次装的位置。
InstallDir "$LOCALAPPDATA\Programs\${PRODUCT_NAME}"
InstallDirRegKey HKCU "${APP_KEY}" "InstallLocation"

VIProductVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PRODUCT_PUBLISHER}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-only"
VIAddVersionKey "FileDescription" "${PRODUCT_NAME} 安装程序"

!ifdef ICONFILE
  !define MUI_ICON "${ICONFILE}"
  !define MUI_UNICON "${ICONFILE}"
!endif
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_NOAUTOCLOSE
!define MUI_UNFINISHPAGE_NOAUTOCLOSE

!insertmacro MUI_PAGE_WELCOME
; 协议页显示的就是随包分发的 LICENSE（GPL-3.0）正文，编译时嵌进安装器，
; 装完也仍以文件形式留在安装目录里。静默安装会跳过这一页。
!define MUI_LICENSEPAGE_RADIOBUTTONS
!insertmacro MUI_PAGE_LICENSE "${BINARYDIR}\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "SimpChinese"

LangString RUNNING_TEXT  ${LANG_SIMPCHINESE} "${PRODUCT_NAME} 正在运行。$\r$\n$\r$\n安装程序需要先关闭它才能替换文件。是否关闭并继续？"
LangString UNINST_RUNNING_TEXT ${LANG_SIMPCHINESE} "${PRODUCT_NAME} 正在运行。$\r$\n$\r$\n卸载前需要先关闭它。是否关闭并继续？"
LangString WEBVIEW2_TEXT  ${LANG_SIMPCHINESE} "未检测到 Microsoft Edge WebView2 运行时，界面可能无法显示。$\r$\n$\r$\nWindows 11 与较新的 Windows 10 已自带；若界面空白，请安装后重试。$\r$\n$\r$\n是否现在打开下载页面？"
LangString SEC_MAIN ${LANG_SIMPCHINESE} "${PRODUCT_NAME} 程序文件（必需）"
LangString SEC_DESKTOP ${LANG_SIMPCHINESE} "创建桌面快捷方式"

; 判断某个文件是否被进程占用。入参压栈为文件路径，返回 $R8：1 = 被占用。
; 用 Restart Manager 而不是 tasklist：它按「谁持有这个文件」回答，
; 不会因为镜像名相同而误判，daemon 这种没有窗口的进程也能识别。
; 安装器与卸载器各要一份定义（卸载器只认 un. 前缀的函数），所以函数体写成宏。
!macro FileInUseBody
  Exch $R7
  StrCpy $R8 0
  !insertmacro RestartManager_StartSession $R0
  ${If} $R0 != ""
    !insertmacro RestartManager_RegisterFile $R0 "$R7"
    ; pnProcInfo 传 0 表示「只要告诉我有几个」；有进程占用时返回 ERROR_MORE_DATA(234)。
    System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
    ${If} $r0 = 234
      StrCpy $R8 1
    ${EndIf}
    !insertmacro RestartManager_EndSession $R0
  ${EndIf}
  Pop $R7
!macroend

Function FileInUse
  !insertmacro FileInUseBody
FunctionEnd
Function un.FileInUse
  !insertmacro FileInUseBody
FunctionEnd

; 关闭正在运行的界面与后台进程。入参压栈：1 = 卸载流程，0 = 安装流程。
; 两个 exe 各查一次：GUI 有窗口、daemon 没有，只有按文件占用才能都覆盖到。
; 函数前缀（un.）与标签前缀（un_）要分开传：卸载器的函数必须带 un.，
; 而标签名里不能出现点号。
!macro StopRunningProcessesBody callprefix labelprefix
  Exch $R9
  Push $R7
  Push $R8

  Push "$INSTDIR\${GUI_EXE}"
  Call ${callprefix}FileInUse
  StrCpy $R7 $R8
  Push "$INSTDIR\${CLI_EXE}"
  Call ${callprefix}FileInUse

  ${If} $R7 = 1
  ${OrIf} $R8 = 1
    ; 静默安装没人能点确认，按「关闭并继续」处理。
    ${If} ${Silent}
      Goto ${labelprefix}kill
    ${EndIf}
    ${If} $R9 = 1
      MessageBox MB_OKCANCEL|MB_ICONQUESTION "$(UNINST_RUNNING_TEXT)" IDOK ${labelprefix}kill IDCANCEL ${labelprefix}cancel
    ${Else}
      MessageBox MB_OKCANCEL|MB_ICONQUESTION "$(RUNNING_TEXT)" IDOK ${labelprefix}kill IDCANCEL ${labelprefix}cancel
    ${EndIf}
    ${labelprefix}cancel:
      Abort
    ${labelprefix}kill:
      !insertmacro RestartManager_ShutdownFile "$INSTDIR\${GUI_EXE}" $0
      !insertmacro RestartManager_ShutdownFile "$INSTDIR\${CLI_EXE}" $0
      ; 强制关闭后文件句柄不是立刻消失，等一会儿再让调用方覆盖。
      Sleep 800
  ${EndIf}

  Pop $R8
  Pop $R7
  Pop $R9
!macroend

Function StopRunningProcesses
  !insertmacro StopRunningProcessesBody "" ""
FunctionEnd
Function un.StopRunningProcesses
  !insertmacro StopRunningProcessesBody "un." "un_"
FunctionEnd

; WebView2 是 Tauri 界面的运行前提。检测不到时不阻断安装，只提示可去下载：
; 安装器在安装过程中静默下载并执行别的 exe，对使用者来说不是个受欢迎的行为。
Function CheckWebView2
  StrCpy $0 ""
  ; 64 位视图下 Pv 键可能在原生路径、WOW6432Node 或每用户位置，三处都看。
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} $0 == ""
    ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
    ReadRegStr $0 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  SetRegView lastused
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    MessageBox MB_YESNO|MB_ICONINFORMATION "$(WEBVIEW2_TEXT)" IDNO done
    ExecShell "open" "https://developer.microsoft.com/microsoft-edge/webview2/"
    done:
  ${EndIf}
FunctionEnd

Section "$(SEC_MAIN)" SecMain
  SectionIn RO
  Push 0
  Call StopRunningProcesses

  SetOutPath $INSTDIR
  SetOverwrite on
  File "${BINARYDIR}\${GUI_EXE}"
  File "${BINARYDIR}\${CLI_EXE}"
  File "${BINARYDIR}\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; 卸载信息进「应用和功能」。写 HKCU：安装是当前用户级的。
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${GUI_EXE}"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "${PRODUCT_URL}"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"

  ; 自己的键：InstallDirRegKey 靠它找回上次的安装位置，升级时不用再选目录。
  WriteRegStr HKCU "${APP_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${APP_KEY}" "Version" "${VERSION}"

  SetShellVarContext current
  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk" "$INSTDIR\${GUI_EXE}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\卸载 ${PRODUCT_NAME}.lnk" "$INSTDIR\uninstall.exe"

  Call CheckWebView2
SectionEnd

; 桌面快捷方式默认勾选，用户仍可在组件页取消（默认值在文件末尾的 .onInit 里设置）。
Section /o "$(SEC_DESKTOP)" SecDesktop
  SetShellVarContext current
  CreateShortCut "$DESKTOP\${PRODUCT_NAME}.lnk" "$INSTDIR\${GUI_EXE}"
SectionEnd

Section "Uninstall"
  Push 1
  Call un.StopRunningProcesses

  ; 登录启动任务指向即将被删除的 exe，留着会让系统在每次登录时报错。
  ; 任务可能在系统任务库里、普通权限删不掉，被拒时再提权试一次。
  nsExec::ExecToStack 'cmd /c schtasks /Query /TN "${TASK_NAME}"'
  Pop $0
  ${If} $0 == 0
    nsExec::ExecToStack 'cmd /c schtasks /Delete /F /TN "${TASK_NAME}"'
    Pop $0
    ${If} $0 != 0
      DetailPrint "登录启动任务需要管理员权限才能删除，正在请求提权"
      ; 用 ExecShell 而不是 ExecShellWait：前者从 NSIS 2.x 就在，
      ; 不必赌 runner 上的 NSIS 版本有后者。代价是不等提权结果。
      ExecShell "runas" "schtasks.exe" "/Delete /F /TN $\"${TASK_NAME}$\"" SW_HIDE
    ${EndIf}
  ${EndIf}

  SetShellVarContext current
  Delete "$DESKTOP\${PRODUCT_NAME}.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\卸载 ${PRODUCT_NAME}.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT_NAME}"

  Delete "$INSTDIR\${GUI_EXE}"
  Delete "$INSTDIR\${CLI_EXE}"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  ; 设置与凭据不在这里删：重装后能接着用，凭据本身也不该由安装器处置。
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"
SectionEnd

; 桌面快捷方式默认勾选，但仍可在组件页取消。
;
; NSIS 没有「默认勾选的可选项」这种直接写法：`Section` 是必选（复选框被禁用、
; 用户改不了），`Section /o` 是可选但默认不勾。要「默认勾上、可以取消」只能在
; 这里主动选中。必须放在 Section 定义之后 —— SelectSection 用的是 Section 索引。
Function .onInit
  !insertmacro SelectSection ${SecDesktop}
FunctionEnd
