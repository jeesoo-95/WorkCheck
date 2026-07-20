// 트레이 상주 — 아이콘 · 메뉴 · 툴팁 (M2)

use crate::commands::{self, AppState};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// 트레이 아이콘 id (툴팁 갱신 시 tray_by_id 로 조회)
pub const TRAY_ID: &str = "main";

/// 메인 창 표시 + 포커스
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 트레이 아이콘 생성 (앱 시작 시 1회). 아이콘은 기본 창 아이콘 재사용.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open_i = MenuItem::with_id(app, "open", "열기", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit_i = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_i, &sep, &quit_i])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("업무 체크")
        .menu(&menu)
        // 좌클릭은 메뉴가 아니라 창 열기로 처리 (메뉴는 우클릭)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0), // 트레이 "종료"는 항상 완전 종료
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 트레이 툴팁 갱신 — "업무 체크 — 미체크 N건" (N = 오늘 미체크 + 밀림)
pub fn update_tooltip(app: &AppHandle) {
    let n = {
        let state = app.state::<AppState>();
        let Ok(conn) = state.db.lock() else {
            return;
        };
        commands::pending_summary(&conn)
            .map(|s| s.total)
            .unwrap_or(0)
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let tip = if n > 0 {
            format!("업무 체크 — 미체크 {}건", n)
        } else {
            "업무 체크 — 미체크 없음".to_string()
        };
        let _ = tray.set_tooltip(Some(tip));
    }
}
