// 전역 단축키 + 빠른 추가 소형 창 (M3-B5)
//
// - global-shortcut 플러그인 콜백은 with_handler 로 "공통 하나"만 등록되므로,
//   현재 등록된 단축키 객체(Shortcut)를 HotkeyState 에 보관해 핸들러에서 dispatch 한다.
// - set_hotkey 로 런타임 변경 시 기존 단축키 unregister 후 새로 register 하고,
//   실패하면 기존 단축키를 복구(재등록)해 상태·Setting 을 바꾸지 않는다.
// - 빠른 추가 창은 label "quick" 으로 재사용(닫기=hide). 등록 후 메인 창에 이벤트를
//   emit 해 오늘/전체 탭을 자동 갱신한다(emit 은 Rust 커맨드로 우회 — JS 직접 호출 금지 원칙).

use crate::commands::{self, AppState};
use std::str::FromStr;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

/// 단축키 기본값 (Setting 미존재 시 fallback)
pub const DEFAULT_TOGGLE: &str = "Ctrl+Alt+W";
pub const DEFAULT_QUICK: &str = "Ctrl+Alt+A";

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 현재 등록된 단축키 객체 (핸들러 dispatch·재등록 복구에 사용). 비활성이면 None.
#[derive(Default)]
pub struct HotkeyState {
    pub toggle: Mutex<Option<Shortcut>>,
    pub quick: Mutex<Option<Shortcut>>,
}

/// 공통 단축키 핸들러 — 눌림(Pressed)만 처리하고, 어떤 단축키인지 상태와 비교해 분기.
pub fn handle_shortcut(app: &AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state() != ShortcutState::Pressed {
        return;
    }
    let hk = app.state::<HotkeyState>();
    let is_toggle = hk
        .toggle
        .lock()
        .ok()
        .map(|g| g.as_ref() == Some(shortcut))
        .unwrap_or(false);
    if is_toggle {
        toggle_main(app);
        return;
    }
    let is_quick = hk
        .quick
        .lock()
        .ok()
        .map(|g| g.as_ref() == Some(shortcut))
        .unwrap_or(false);
    if is_quick {
        open_quick_window(app);
    }
}

/// 메인 창 토글: 보이고 포커스면 hide, 아니면 show+unminimize+set_focus.
fn toggle_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let visible = w.is_visible().unwrap_or(false);
        let focused = w.is_focused().unwrap_or(false);
        if visible && focused {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }
}

/// 빠른 추가 소형 창 열기. 이미 있으면 show+focus(재사용), 없으면 생성.
/// 생성 시 닫기(CloseRequested)는 파괴 대신 hide 로 가로채 재사용 가능하게 한다.
pub fn open_quick_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("quick") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "quick", WebviewUrl::App("quick.html".into()))
        .title("빠른 추가")
        .inner_size(420.0, 180.0)
        .resizable(false)
        .always_on_top(true)
        .decorations(false)
        .skip_taskbar(true)
        .center()
        .build();
    if let Ok(win) = built {
        let handle = app.clone();
        win.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(w) = handle.get_webview_window("quick") {
                    let _ = w.hide();
                }
            }
        });
        let _ = win.set_focus();
    }
}

/// setup 에서 1회 호출 — Setting(hotkey_toggle/hotkey_quick) 값으로 초기 등록.
pub fn init(app: &AppHandle) {
    let (toggle, quick) = {
        let state = app.state::<AppState>();
        let Ok(conn) = state.db.lock() else {
            return;
        };
        (
            commands::read_setting(&conn, "hotkey_toggle")
                .unwrap_or_else(|| DEFAULT_TOGGLE.to_string()),
            commands::read_setting(&conn, "hotkey_quick")
                .unwrap_or_else(|| DEFAULT_QUICK.to_string()),
        )
    };
    register_initial(app, "toggle", &toggle);
    register_initial(app, "quick", &quick);
}

/// 초기 등록 헬퍼 — 빈 문자열/파싱 실패/등록 실패는 조용히 비활성으로 둔다.
fn register_initial(app: &AppHandle, kind: &str, accel: &str) {
    let accel = accel.trim();
    if accel.is_empty() {
        return;
    }
    let Ok(sc) = Shortcut::from_str(accel) else {
        return;
    };
    if app.global_shortcut().register(sc.clone()).is_err() {
        return;
    }
    let hk = app.state::<HotkeyState>();
    let mut guard = if kind == "toggle" {
        hk.toggle.lock()
    } else {
        hk.quick.lock()
    };
    if let Ok(g) = guard.as_mut() {
        **g = Some(sc);
    }
}

// ── 커맨드 ───────────────────────────────────────────────

/// 단축키 등록/변경. kind: "toggle"|"quick", accel: "Ctrl+Alt+W" 형식(빈 문자열=비활성).
/// 기존 unregister → 새 register. 실패 시 기존 복구 후 Err(설정 저장 안 함).
#[tauri::command]
pub fn set_hotkey(app: AppHandle, kind: String, accel: String) -> Result<(), String> {
    let key = match kind.as_str() {
        "toggle" => "hotkey_toggle",
        "quick" => "hotkey_quick",
        _ => return Err(format!("알 수 없는 단축키 종류: {}", kind)),
    };
    let accel = accel.trim().to_string();

    // 새 단축키 파싱 (빈 문자열=비활성)
    let new_sc: Option<Shortcut> = if accel.is_empty() {
        None
    } else {
        Some(
            Shortcut::from_str(&accel)
                .map_err(|_| format!("단축키 형식이 올바르지 않습니다: {}", accel))?,
        )
    };

    let gs = app.global_shortcut();
    let hk = app.state::<HotkeyState>();
    let slot = if kind == "toggle" { &hk.toggle } else { &hk.quick };

    // 기존 값 확보 후 해제
    let old_sc = slot.lock().map_err(e2s)?.clone();
    if let Some(old) = &old_sc {
        let _ = gs.unregister(old.clone());
    }

    // 새로 등록 — 실패 시 기존 복구 후 Err (Setting 저장 안 함)
    if let Some(new) = &new_sc {
        if let Err(e) = gs.register(new.clone()) {
            if let Some(old) = &old_sc {
                let _ = gs.register(old.clone());
            }
            return Err(format!("단축키 등록 실패: {}", e));
        }
    }

    // 상태 갱신 + Setting 저장
    *slot.lock().map_err(e2s)? = new_sc;
    let state = app.state::<AppState>();
    let conn = state.db.lock().map_err(e2s)?;
    commands::write_setting(&conn, key, &accel)?;
    Ok(())
}

/// 빠른 추가 창 숨김 (quick.js 의 닫기/Esc/등록완료 시 호출).
#[tauri::command]
pub fn hide_quick_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("quick") {
        let _ = w.hide();
    }
}

/// 메인 창에 "task-added" 이벤트 emit — 빠른 추가 등록 후 오늘/전체 탭 자동 갱신용.
/// (프론트 emit 대신 Rust 에서 emit 해 이벤트 emit capability 불필요)
#[tauri::command]
pub fn notify_task_added(app: AppHandle) -> Result<(), String> {
    app.emit_to("main", "task-added", ()).map_err(e2s)
}
