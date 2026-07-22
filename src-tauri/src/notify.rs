// 백그라운드 알림 스케줄러 (M2)
//
// std::thread 로 30초 간격 루프를 돌며:
//   1) 정시 요약 알림  — notify_time 이후 하루 1회, 미체크(오늘+밀림) 건수 요약
//   2) 밀림 즉시 알림  — 시작 시 1회 + 이후 밀림 신규 발생 시
//   3) 트레이 툴팁 갱신
// DB 접근은 AppState(Mutex<Connection>) 를 매 틱 잠깐만 점유한다.

use crate::commands::{self, AppState, PendingSummary};
use crate::tray;
use chrono::{Datelike, Local, NaiveTime};
use std::collections::HashSet;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

const TICK_SECS: u64 = 30;

/// notify_time("HH:MM") 파싱 — 실패 시 09:00 fallback
fn parse_notify_time(s: &str) -> NaiveTime {
    NaiveTime::parse_from_str(s, "%H:%M")
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(9, 0, 0).unwrap())
}

/// 토스트 발송
fn toast(app: &AppHandle, title: &str, body: &str) {
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show();
}

/// 미체크 요약 스냅샷 (DB 락 잠깐만 점유)
fn snapshot(app: &AppHandle) -> Option<PendingSummary> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().ok()?;
    commands::pending_summary(&conn).ok()
}

/// 업무별 알림·사전 리마인드 후보 스냅샷 (DB 락 잠깐만 점유)
fn per_task_snapshot(app: &AppHandle) -> Vec<commands::PerTaskNotify> {
    let state = app.state::<AppState>();
    let Ok(conn) = state.db.lock() else {
        return Vec::new();
    };
    commands::per_task_notify(&conn).unwrap_or_default()
}

/// Setting 문자열 조회
fn setting_str(app: &AppHandle, key: &str) -> Option<String> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().ok()?;
    commands::read_setting(&conn, key)
}

/// Setting bool 조회 ("1" = true), 없으면 default
fn setting_bool(app: &AppHandle, key: &str, default: bool) -> bool {
    setting_str(app, key)
        .map(|v| v == "1")
        .unwrap_or(default)
}

/// Setting 기록
fn setting_write(app: &AppHandle, key: &str, value: &str) {
    let state = app.state::<AppState>();
    let Ok(conn) = state.db.lock() else {
        return;
    };
    let _ = commands::write_setting(&conn, key, value);
}

/// 밀림 알림 본문: "밀림 N건 · 업무명, 업무명"
fn overdue_body(s: &PendingSummary) -> String {
    let mut body = format!("밀림 {}건", s.overdue);
    if !s.sample_names.is_empty() {
        body.push_str(" · ");
        body.push_str(&s.sample_names.join(", "));
    }
    body
}

/// 정시 요약 본문: "밀림 N건 · 업무명, 업무명"
fn summary_body(s: &PendingSummary) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.overdue > 0 {
        parts.push(format!("밀림 {}건", s.overdue));
    }
    if !s.sample_names.is_empty() {
        parts.push(s.sample_names.join(", "));
    }
    parts.join(" · ")
}

/// 정시 요약 알림 판정 + 발송 (하루 1회)
fn summary_tick(app: &AppHandle, s: &PendingSummary) {
    if !setting_bool(app, "notify_enabled", true) {
        return;
    }
    let target = parse_notify_time(&setting_str(app, "notify_time").unwrap_or_default());
    let now = Local::now();
    if now.time() < target {
        return;
    }
    let today = now.date_naive().to_string();
    // 이미 오늘 발송했으면 skip (앱 재시작 시 중복 방지)
    if setting_str(app, "last_notify_date").as_deref() == Some(today.as_str()) {
        return;
    }
    if s.total == 0 {
        // 0건이면 발송 생략 (발송일도 기록하지 않음 — 이후 미체크 생기면 알림)
        return;
    }
    toast(
        app,
        &format!("오늘 미체크 업무 {}건", s.total),
        &summary_body(s),
    );
    setting_write(app, "last_notify_date", &today);
}

/// 업무별 개별 알림 + 사전 리마인드 판정·발송 (각각 업무당 하루 1회).
///
/// notify_enabled 전역 스위치와 무관하게 업무별 설정만 따른다.
/// 발송 기록(`sent`/`reminded`)은 프로세스 메모리에만 유지하므로, 앱 재시작 시
/// 오늘분이 각 1회 재알림될 수 있다(개인용 앱이라 단순성 우선 — DB 기록 생략).
fn per_task_tick(
    app: &AppHandle,
    sent: &mut HashSet<(i64, String)>,
    reminded: &mut HashSet<(i64, String)>,
) {
    let now = Local::now();
    let today = now.date_naive().to_string();
    // 사전 리마인드는 전역 notify_time(기본 09:00) 이후 첫 틱부터 발송
    let global_target = parse_notify_time(&setting_str(app, "notify_time").unwrap_or_default());
    let after_global = now.time() >= global_target;

    for item in per_task_snapshot(app) {
        // ── 업무별 알림 시각: 현재시각 ≥ notify_time 이면 발송 (하루 1회) ──
        if let Some(nt) = &item.today_notify_time {
            if now.time() >= parse_notify_time(nt) {
                let key = (item.task_id, today.clone());
                if !sent.contains(&key) {
                    toast(app, &format!("「{}」 체크할 시간입니다", item.name), "");
                    sent.insert(key);
                }
            }
        }
        // ── 사전 리마인드: 다음 기한일이 오늘+N 이면 발송 (하루 1회) ──
        if after_global {
            if let Some((n, next)) = item.remind {
                let key = (item.task_id, today.clone());
                if !reminded.contains(&key) {
                    toast(
                        app,
                        &format!("「{}」 — {}일 후({}/{}) 기한", item.name, n, next.month(), next.day()),
                        "",
                    );
                    reminded.insert(key);
                }
            }
        }
    }
}

/// 스케줄러 시작 (setup 에서 1회 호출)
pub fn start(app: AppHandle) {
    thread::spawn(move || {
        // 이미 알림한 밀림 집합 (신규 발생 감지용)
        let mut known_overdue: HashSet<(i64, String)> = HashSet::new();
        // 업무별 알림·사전 리마인드 발송 기록 (task_id, 날짜) — 오늘분 중복 발송 방지
        let mut per_task_sent: HashSet<(i64, String)> = HashSet::new();
        let mut per_task_reminded: HashSet<(i64, String)> = HashSet::new();

        // ── 앱 시작 시: 밀림 즉시 알림 1회 ──
        if let Some(s) = snapshot(&app) {
            if setting_bool(&app, "notify_on_overdue", true) && s.overdue > 0 {
                toast(&app, &format!("밀린 업무 {}건", s.overdue), &overdue_body(&s));
            }
            // 시작 시점의 밀림은 모두 '알려진' 상태로 초기화 (재알림 방지)
            known_overdue = s.overdue_keys.iter().cloned().collect();
        }
        tray::update_tooltip(&app);

        loop {
            thread::sleep(Duration::from_secs(TICK_SECS));

            let Some(s) = snapshot(&app) else {
                continue;
            };
            let now_keys: HashSet<(i64, String)> = s.overdue_keys.iter().cloned().collect();

            // ── 밀림 신규 발생 알림 ──
            if setting_bool(&app, "notify_on_overdue", true) {
                let new_cnt = now_keys.difference(&known_overdue).count();
                if new_cnt > 0 {
                    toast(
                        &app,
                        &format!("새 밀린 업무 {}건", new_cnt),
                        &overdue_body(&s),
                    );
                }
            }
            known_overdue = now_keys;

            // ── 정시 요약 알림 ──
            summary_tick(&app, &s);

            // ── 업무별 알림 · 사전 리마인드 (전역 스위치와 무관) ──
            per_task_tick(&app, &mut per_task_sent, &mut per_task_reminded);

            // ── Jira 폴링 (활성·경과시간 판정은 poll_tick 내부에서) ──
            crate::jira::poll_tick(&app);

            // ── 트레이 툴팁 갱신 ──
            tray::update_tooltip(&app);
        }
    });
}
