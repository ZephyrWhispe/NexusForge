//! NexusForge Tauri 壳（docs/impl/01 S7）

mod commands;
mod state;

use state::StartupOptions;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let opts = StartupOptions::from_env();

    // --restore-proxy：紧急还原系统代理后退出（崩溃抢救通道，还原挂点之三）
    if opts.restore_proxy {
        // Tauri 壳尚未启动：手动拼 app data 目录（须与 tauri.conf.json identifier 一致）
        match std::env::var("APPDATA")
            .map(|base| std::path::PathBuf::from(base).join("com.nexusforge.app").join("proxy"))
        {
            Ok(proxy_dir) => {
                let sp = win_integration::sysproxy::WindowsSysProxy;
                // 端口读持久化状态（用于识别我们的标记值 127.0.0.1:{port}）；无记录退回默认
                let port = std::fs::read(proxy_dir.join("proxy_state.json"))
                    .ok()
                    .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                    .and_then(|v| v.get("mixed_port").and_then(|p| p.as_u64()))
                    .unwrap_or(7890) as u16;
                match proxy_core::sysproxy::restore_if_ours(&proxy_dir, &sp, port) {
                    Ok(true) => println!("[NexusForge] --restore-proxy：残留系统代理已还原。"),
                    Ok(false) => {
                        println!("[NexusForge] --restore-proxy：无残留代理（用户已自行修改），备份已清理。")
                    }
                    Err(e) => {
                        eprintln!("[NexusForge] --restore-proxy：标记值还原失败（{e}），退回备份还原。");
                        proxy_core::sysproxy::restore_quiet(&proxy_dir, &sp);
                    }
                }
            }
            Err(_) => eprintln!("[NexusForge] --restore-proxy：无法定位 APPDATA 目录。"),
        }
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
            commands::proxy_status,
            commands::proxy_kernel_install,
            commands::proxy_wintun_install,
            commands::proxy_subs,
            commands::proxy_sub_add,
            commands::proxy_sub_remove,
            commands::proxy_sub_update,
            commands::proxy_nodes,
            commands::proxy_direct_rules,
            commands::proxy_set_direct_rules,
            commands::proxy_set_mode,
            commands::proxy_delay_test,
            commands::proxy_logs,
            commands::desktop_launcher_search,
            commands::desktop_launcher_launch,
            commands::desktop_launcher_status,
            commands::desktop_tidy_plan,
            commands::desktop_tidy_apply,
            commands::desktop_tidy_restore,
            commands::desktop_tidy_status,
            commands::desktop_note_add,
            commands::desktop_note_list,
            commands::desktop_note_done,
            commands::desktop_note_remove,
            commands::desktop_notes_due,
            commands::editor_open,
            commands::editor_content,
            commands::editor_update,
            commands::editor_save,
            commands::editor_save_as,
            commands::editor_autosave,
            commands::editor_close,
            commands::editor_sessions,
            commands::pdf_info,
            commands::pdf_merge,
            commands::pdf_split,
            commands::pdf_compress,
            commands::pdf_watermark,
            // 笔记与知识（M10 N1–N5）
            commands::notes_list,
            commands::notes_read,
            commands::notes_create,
            commands::notes_write,
            commands::notes_delete,
            commands::notes_rename,
            commands::notes_links,
            commands::notes_backlinks,
            commands::notes_sync,
            commands::notes_reindex,
            commands::notes_cards,
            commands::notes_card_create,
            commands::notes_card_delete,
            commands::notes_review_queue,
            commands::notes_review_grade,
            commands::notes_canvas_get,
            commands::notes_canvas_save,
            commands::notes_canvas_dirs,
            // 终端与运维（M11 T1–T6）
            commands::term_spawn_local,
            commands::term_spawn_wsl,
            commands::term_wsl_list,
            commands::term_write,
            commands::term_resize,
            commands::term_ack,
            commands::term_kill,
            commands::term_sessions,
            commands::term_ssh_connect,
            commands::term_ssh_known_hosts,
            commands::term_ssh_forget_host,
            commands::term_sftp_list,
            commands::term_sftp_download,
            commands::term_sftp_upload,
            commands::term_docker_containers,
            commands::term_docker_lifecycle,
            commands::term_docker_logs,
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
