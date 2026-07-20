// 앱 부트스트랩 · plugin/command 등록

mod commands;
mod db;
mod model;
mod notify;
mod recur;
mod tray;

use commands::AppState;
use std::sync::Mutex;
use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 플러그인은 JS 직접 호출 대신 Rust 커맨드로 래핑해 사용 (ACL 이슈 회피)
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            // DB 경로: app_data_dir(%APPDATA%\com.jeesoo.workcheck)\workcheck.db
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db_path = dir.join("workcheck.db");
            let conn = db::open(&db_path).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            app.manage(AppState {
                db: Mutex::new(conn),
            });

            // 트레이 상주 (아이콘 · 메뉴 · 툴팁)
            tray::build(app.handle())?;

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
            commands::get_settings,
            commands::set_setting,
            commands::list_holidays,
            commands::add_holiday,
            commands::delete_holiday,
            commands::test_notification,
            commands::get_autostart,
            commands::set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
