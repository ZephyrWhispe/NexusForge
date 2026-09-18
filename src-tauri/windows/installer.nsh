; NexusForge NSIS 安装钩子（REL2，docs/impl/07：Tauri installerHooks 约定宏）
; 卸载后询问是否删除用户数据——默认保留（{appData}\com.nexusforge.app）
; 含：笔记库 / 剪切板历史 / 配置 / 代理备份等

!macro NSIS_HOOK_POSTUNINSTALL
  MessageBox MB_YESNO|MB_ICONQUESTION "是否同时删除 NexusForge 用户数据？$\r$\n$\r$\n（笔记库、剪切板历史、配置等，位于 %APPDATA%\com.nexusforge.app）$\r$\n选择「否」保留数据（推荐）。" IDYES nf_delete_user_data
  Goto nf_uninstall_done
nf_delete_user_data:
  RMDir /r "$APPDATA\com.nexusforge.app"
nf_uninstall_done:
!macroend
