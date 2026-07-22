// 앱 부트스트랩 · plugin/command 등록

mod commands;
mod db;
mod hotkey;
mod jira;
mod model;
mod notify;
mod recur;
mod tray;

use commands::AppState;
use hotkey::HotkeyState;
use std::sync::Mutex;
use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

/// 무설치 실행 시 토스트 알림 신원(AppUserModelId) 등록.
/// 설치본은 인스톨러가 등록하지만, exe 직접 실행은 이 등록이 없으면
/// Windows가 알림 주체를 몰라 배너 표시가 누락될 수 있다.
#[cfg(windows)]
fn register_notification_identity(icon_dir: &std::path::Path) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok((key, _)) =
        hkcu.create_subkey("Software\\Classes\\AppUserModelId\\com.jeesoo.workcheck")
    {
        let _ = key.set_value("DisplayName", &"업무 체크");
        // 토스트에 표시할 아이콘: 내장 png 를 데이터 폴더에 풀어 참조
        let icon_path = icon_dir.join("notify-icon.png");
        if !icon_path.exists() {
            let _ = std::fs::write(&icon_path, include_bytes!("../icons/128x128.png"));
        }
        if let Some(p) = icon_path.to_str() {
            let _ = key.set_value("IconUri", &p);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 중복 실행 방지: 두 번째 인스턴스 실행 시 기존 창을 표시·복원·포커스.
        // (Builder 최상단에 등록해야 다른 플러그인 초기화 전에 단일화가 적용됨)
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        // 플러그인은 JS 직접 호출 대신 Rust 커맨드로 래핑해 사용 (ACL 이슈 회피)
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_dialog::init())
        // 전역 단축키: 콜백은 공통 하나(with_handler)로 등록, 분기는 hotkey::handle_shortcut 이 담당
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    hotkey::handle_shortcut(app, shortcut, event);
                })
                .build(),
        )
        .setup(|app| {
            // DB 경로: app_data_dir(%APPDATA%\com.jeesoo.workcheck)\workcheck.db
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            #[cfg(windows)]
            register_notification_identity(&dir);
            let db_path = dir.join("workcheck.db");
            let conn = db::open(&db_path).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            app.manage(AppState {
                db: Mutex::new(conn),
            });
            // 전역 단축키 상태 (현재 등록된 Shortcut 보관)
            app.manage(HotkeyState::default());

            // 트레이 상주 (아이콘 · 메뉴 · 툴팁)
            tray::build(app.handle())?;

            // 전역 단축키 초기 등록 (Setting 값 기준)
            hotkey::init(app.handle());

            // 창 닫기(X): close_to_tray=1 이면 종료 대신 트레이로 최소화(hide)
            if let Some(window) = app.get_webview_window("main") {
                let handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let close_to_tray = handle
                            .state::<AppState>()
                            .db
                            .lock()
                            .ok()
                            .and_then(|c| commands::read_setting(&c, "close_to_tray"))
                            .map(|v| v == "1")
                            .unwrap_or(true);
                        if close_to_tray {
                            api.prevent_close();
                            if let Some(w) = handle.get_webview_window("main") {
                                let _ = w.hide();
                            }
                        }
                    }
                });
            }

            // 자동 백업: auto_backup=1 이면 앱 시작 시 1회 (실패해도 앱 시작은 계속)
            {
                let state = app.state::<AppState>();
                let guard = state.db.lock();
                if let Ok(conn) = guard {
                    let enabled = commands::read_setting(&conn, "auto_backup")
                        .map(|v| v == "1")
                        .unwrap_or(true);
                    if enabled {
                        let _ = commands::perform_backup(&conn, &dir.join("backups"));
                    }
                }
            }

            // 알림 스케줄러 시작 (시작 시 밀림 알림 + 툴팁 초기화 포함)
            notify::start(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_today_view,
            commands::toggle_check,
            commands::set_check_status,
            commands::set_check_memo,
            commands::get_day_view,
            commands::list_tasks,
            commands::add_task,
            commands::update_task,
            commands::delete_task,
            commands::get_stats,
            commands::generate_report,
            commands::set_sort_order,
            commands::open_link,
            commands::get_settings,
            commands::set_setting,
            commands::list_holidays,
            commands::add_holiday,
            commands::delete_holiday,
            commands::test_notification,
            commands::get_autostart,
            commands::set_autostart,
            commands::backup_now,
            commands::restore_backup,
            commands::jira_test_connection,
            commands::jira_poll_now,
            commands::get_jira_notifications,
            commands::get_jira_unread_count,
            commands::mark_jira_read,
            commands::mark_all_jira_read,
            hotkey::set_hotkey,
            hotkey::hide_quick_window,
            hotkey::notify_task_added,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
