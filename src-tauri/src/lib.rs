//! NexusForge Tauri 壳（docs/impl/01 S7）

mod commands;
mod state;
mod winops_helper;

use automation_core::engine::ActionHandler;
use automation_core::error as auto_err;
use host_core::ports::ShellPort;
use state::StartupOptions;
use tauri::Manager;
use std::sync::Arc;

/// 独立规则执行的最小动作处理器（A4：Task Scheduler 触发 `--run-rule` 无完整宿主）
/// publish/notify 无总线收方 → 记日志；run_wasm 无插件库 → 报未开放
struct StandaloneHandler;

impl ActionHandler for StandaloneHandler {
    fn open_url(&self, url: &str) -> auto_err::Result<()> {
        win_integration::shell::ShellOps
            .shell_execute(url)
            .map_err(|e| auto_err::AutomationError::Action(e.to_string()))
    }
    fn publish(&self, topic: &str, payload: serde_json::Value) -> auto_err::Result<()> {
        tracing::info!(topic, ?payload, "--run-rule 独立进程无事件总线，publish 记日志");
        Ok(())
    }
    fn ipc_command(&self, module: &str, cmd: &str, _args: &serde_json::Value) -> auto_err::Result<()> {
        Err(auto_err::AutomationError::Action(format!("独立进程不支持 IpcCommand（{module}.{cmd}）")))
    }
    fn run_wasm(&self, path: &str, _func: &str) -> auto_err::Result<()> {
        Err(auto_err::AutomationError::Action(format!(
            "独立进程不支持 RunScript（{path}）；请在应用内触发含插件的规则"
        )))
    }
}

/// --run-rule：从 rules.json 执行规则后退出（无窗口、不启动 Tauri 壳）
fn run_standalone_rule(rule_id: &str) {
    let rules_path = match std::env::var("APPDATA") {
        Ok(base) => std::path::PathBuf::from(base)
            .join("com.nexusforge.app")
            .join("automation")
            .join("rules.json"),
        Err(_) => {
            eprintln!("[NexusForge] --run-rule：无法定位 APPDATA 目录。");
            std::process::exit(1);
        }
    };
    // 日志（独立进程也留痕；guard 泄漏至退出属预期）
    let log_guard = host_core::logging::init_tracing(&rules_path.parent().unwrap().join("log"));
    Box::leak(Box::new(log_guard));
    tracing::info!(rule_id, "Task Scheduler 触发独立规则执行");
    match automation_core::standalone::run_rule_standalone(&rules_path, rule_id, Arc::new(StandaloneHandler)) {
        Ok(true) => println!("[NexusForge] 规则 {rule_id} 已执行。"),
        Ok(false) => println!("[NexusForge] 规则 {rule_id} 不存在或已停用。"),
        Err(e) => {
            eprintln!("[NexusForge] 规则 {rule_id} 执行失败: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let opts = StartupOptions::from_env();

    // --run-rule：Task Scheduler 独立触发（A4，docs/impl/07）；执行后退出
    if let Some(rule_id) = opts.run_rule.clone() {
        run_standalone_rule(&rule_id);
        std::process::exit(0);
    }

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
        // REL1（docs/impl/07）：自动更新插件（端点/公钥见 tauri.conf.json plugins.updater；
        // 签名私钥仅存 CI Secret——REL3 接入后 latest.json 附签名，更新前双签名校验）
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            // 系统管理（M12 SY1–SY4）
            commands::sys_pkg_sources,
            commands::sys_pkg_list,
            commands::sys_pkg_cmd_preview,
            commands::sys_pkg_action,
            commands::sys_clean_targets,
            commands::sys_clean_scan,
            commands::sys_clean_execute,
            commands::sys_metrics_history,
            // 自动化与拓展（M14 A1–A3）
            commands::automation_rules_list,
            commands::automation_save_rule,
            commands::automation_delete_rule,
            commands::automation_toggle_rule,
            commands::automation_dead_letters,
            commands::automation_replay,
            // 插件管理（M14 A6）
            commands::automation_plugins_list,
            commands::automation_plugin_install,
            commands::automation_plugin_remove,
            // 跨设备同步（M15 SYNC）
            commands::sync_peers,
            commands::sync_status,
            commands::sync_now,
            // WinOps Tweak 引擎（M16 W0–W1）
            commands::winops_catalog,
            commands::winops_scan,
            commands::winops_apply,
            commands::winops_rollback,
        ])
        .setup(move |app| {
            // PERF1（docs/impl/07）：启动路径计时（目标 < 1.5s；setup 完成即窗口可见）
            let started = std::time::Instant::now();
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
            let boot_started = started;
            tauri::async_runtime::spawn(async move {
                host_for_boot.bootstrap_modules().await;
                tracing::info!(
                    elapsed_ms = boot_started.elapsed().as_millis() as u64,
                    "模块引导完成（PERF1 启动剖析）"
                );
            });
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "窗口就绪（setup 完成，PERF1 启动剖析）"
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("NexusForge 启动失败");
}
