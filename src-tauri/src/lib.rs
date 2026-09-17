//! NexusForge Tauri 壳（docs/impl/01 S7）

mod commands;
mod state;

use state::StartupOptions;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let opts = StartupOptions::from_env();

    // --restore-proxy：紧急还原系统代理后退出（真实实现在阶段二 PR4 接入）
    if opts.restore_proxy {
        eprintln!("[NexusForge] --restore-proxy：系统代理还原将在代理模块交付后生效。");
        std::process::exit(0);
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::host_system_accent,
            commands::host_log,
            commands::host_modules_status,
            commands::host_module_restart,
            commands::host_config_get,
            commands::host_config_set,
            commands::host_config_schema,
            commands::clipboard_search,
            commands::clipboard_get,
            commands::clipboard_get_image,
            commands::clipboard_paste,
            commands::clipboard_pin,
            commands::clipboard_delete,
            commands::clipboard_clear,
            commands::clipboard_group_counts,
            commands::screenshot_start,
            commands::screenshot_task,
            commands::screenshot_confirm,
            commands::screenshot_discard,
            commands::screenshot_finish,
            commands::screenshot_history_list,
            commands::screenshot_pins,
            commands::screenshot_pin_get,
            commands::screenshot_pin_update,
            commands::screenshot_pin_close,
            commands::ocr_recognize,
            commands::ocr_engine_status,
            commands::ocr_copy_text,
            commands::kvm_issue_pair_code,
            commands::kvm_pair_with,
            commands::kvm_unpair,
            commands::kvm_paired_peers,
            commands::kvm_discovered_peers,
            commands::kvm_connect_to,
            commands::kvm_send_clip,
            commands::kvm_send_file,
            commands::kvm_session_list,
            commands::kvm_set_edge_map,
            commands::kvm_edge_map,
            commands::kvm_control_state,
            commands::kvm_release_control,
            commands::vault_status,
            commands::vault_create,
            commands::vault_unlock,
            commands::vault_lock,
            commands::vault_change_master_password,
            commands::vault_folders,
            commands::vault_folder_create,
            commands::vault_folder_rename,
            commands::vault_folder_delete,
            commands::vault_entries,
            commands::vault_entry_get,
            commands::vault_entry_add,
            commands::vault_entry_update,
            commands::vault_entry_delete,
            commands::vault_generate_password,
            commands::vault_totp_now,
            commands::file_drives,
            commands::file_list,
            commands::file_breadcrumbs,
            commands::file_mkdir,
            commands::file_rename_entry,
            commands::file_enqueue,
            commands::file_ops_active,
            commands::file_ops_pending,
            commands::file_op_pause,
            commands::file_op_resume,
            commands::file_op_cancel,
            commands::file_op_drop_pending,
            commands::file_preview,
            commands::file_search,
            commands::file_drivers,
            commands::file_rename_plan,
            commands::file_rename_apply,
        ])
        .setup(move |app| {
            let dir = app.path().app_data_dir()?;

            // 日志必须最先初始化（guard 泄漏持有至进程结束，属预期行为）
            let log_guard = host_core::logging::init_tracing(&dir.join("log"));
            Box::leak(Box::new(log_guard));
            tracing::info!(version = host_core::HOST_CORE_VERSION, "NexusForge 启动");

            let host = state::HostState::init(dir.clone(), &opts)?;
            let report = host_core::crash::scan_startup(&dir);
            if report.crashed_last_run {
                tracing::warn!(?report.last_crash_file, "检测到上次运行发生崩溃");
            }
            if report.pending_ops > 0 {
                tracing::warn!(count = report.pending_ops, "存在未完成的文件操作，可在 UI 中选择继续或回滚");
            }

            app.manage(host.clone());
            state::forward_events(app.handle().clone(), host.bus.clone());

            // 模块引导放后台任务，不阻塞窗口显示（M1 验收：启动 < 1.5s）
            let host_for_boot = host.clone();
            tauri::async_runtime::spawn(async move {
                host_for_boot.bootstrap_modules().await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("NexusForge 启动失败");
}
