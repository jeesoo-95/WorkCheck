// Tauri commands — 프론트엔드가 invoke 로 호출하는 API
// 모든 command 는 Result<T, String> 로 에러를 반환한다.

use crate::model::*;
use crate::recur;
use chrono::{Datelike, Duration, Local, NaiveDate};
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::sync::Mutex;
use tauri::State;

/// 앱 전역 상태 (DB 연결)
pub struct AppState {
    pub db: Mutex<Connection>,
}

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ── 내부 로더 ────────────────────────────────────────────

/// 활성 업무 목록
fn load_tasks(conn: &Connection) -> Result<Vec<Task>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id,name,memo,links,recur_type,recur_param,active,sort_order,created_at \
             FROM Task WHERE active=1 ORDER BY sort_order, id",
        )
        .map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Task {
                id: r.get(0)?,
                name: r.get(1)?,
                memo: r.get(2)?,
                links: r.get(3)?,
                recur_type: r.get(4)?,
                recur_param: r.get(5)?,
                active: r.get(6)?,
                sort_order: r.get(7)?,
                created_at: r.get(8)?,
            })
        })
        .map_err(e2s)?;
    let mut v = Vec::new();
    for row in rows {
        v.push(row.map_err(e2s)?);
    }
    Ok(v)
}

/// 공휴일 날짜 집합
fn load_holidays(conn: &Connection) -> Result<HashSet<NaiveDate>, String> {
    let mut stmt = conn.prepare("SELECT date FROM Holiday").map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(e2s)?;
    let mut set = HashSet::new();
    for row in rows {
        let s = row.map_err(e2s)?;
        if let Ok(dt) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
            set.insert(dt);
        }
    }
    Ok(set)
}

/// (task_id, due_date) 체크 이력 집합
fn load_checks(conn: &Connection) -> Result<HashSet<(i64, String)>, String> {
    let mut stmt = conn
        .prepare("SELECT task_id, due_date FROM CheckLog")
        .map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(e2s)?;
    let mut set = HashSet::new();
    for row in rows {
        set.insert(row.map_err(e2s)?);
    }
    Ok(set)
}

/// TaskOccurrence 생성 헬퍼
fn make_occ(
    t: &Task,
    due_date: &str,
    checked: bool,
    days_late: i64,
    upcoming_label: Option<String>,
) -> TaskOccurrence {
    let param = t.recur_param.as_deref().unwrap_or("{}");
    TaskOccurrence {
        task_id: t.id,
        name: t.name.clone(),
        memo: t.memo.clone(),
        links: t.links.clone(),
        recur_type: t.recur_type.clone(),
        recur_param: t.recur_param.clone(),
        due_date: due_date.to_string(),
        rule_label: recur::rule_label(&t.recur_type, param),
        checked,
        days_late,
        upcoming_label,
    }
}

/// 업무 생성일 (파싱 실패 시 None → 클램프 없음)
fn created_date(t: &Task) -> Option<NaiveDate> {
    t.created_at
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok())
}

/// 생성일 이전은 발생 회차로 치지 않도록 from 을 클램프
fn clamp_from(t: &Task, from: NaiveDate) -> NaiveDate {
    match created_date(t) {
        Some(c) if c > from => c,
        _ => from,
    }
}

/// [from,to] 구간의 (완료수, 전체수) — 업무별로 생성일 이후만 집계
fn range_stats(
    tasks: &[Task],
    holidays: &HashSet<NaiveDate>,
    checks: &HashSet<(i64, String)>,
    from: NaiveDate,
    to: NaiveDate,
) -> (i64, i64) {
    let mut done = 0;
    let mut total = 0;
    for t in tasks {
        let param = t.recur_param.as_deref().unwrap_or("{}");
        for d in recur::occurrences_between(&t.recur_type, param, clamp_from(t, from), to, holidays) {
            total += 1;
            if checks.contains(&(t.id, d.to_string())) {
                done += 1;
            }
        }
    }
    (done, total)
}

fn rate(done: i64, total: i64) -> f64 {
    if total == 0 {
        0.0
    } else {
        done as f64 / total as f64
    }
}

fn week_start(today: NaiveDate) -> NaiveDate {
    let dow = today.weekday().num_days_from_monday();
    today - Duration::days(dow as i64)
}

/// 밀림 + 오늘 회차 계산 (오늘 탭·알림·트레이 공용).
/// - overdue: 오늘 이전 가장 최근 기한일이 미체크인 회차(최근 120일, 생성일 이후만), D+n 큰 순 정렬
/// - today:   오늘 기한 회차(완료 여부 무관, checked 플래그 포함)
fn compute_overdue_today(
    tasks: &[Task],
    holidays: &HashSet<NaiveDate>,
    checks: &HashSet<(i64, String)>,
    today: NaiveDate,
) -> (Vec<TaskOccurrence>, Vec<TaskOccurrence>) {
    let mut overdue = Vec::new();
    let mut today_list = Vec::new();

    for t in tasks {
        let param = t.recur_param.as_deref().unwrap_or("{}");

        // 오늘 기한 (완료 여부와 무관하게 표시 — 목업의 done 행 포함)
        let today_occ = recur::occurrences_between(&t.recur_type, param, today, today, holidays);
        if !today_occ.is_empty() {
            let due = today.to_string();
            let checked = checks.contains(&(t.id, due.clone()));
            today_list.push(make_occ(t, &due, checked, 0, None));
        }

        // 밀림: 오늘 이전 가장 최근 기한일이 미체크면 D+n (최근 120일, 생성일 이후만)
        let past_from = clamp_from(t, today - Duration::days(120));
        let past =
            recur::occurrences_between(&t.recur_type, param, past_from, today - Duration::days(1), holidays);
        if let Some(&last) = past.last() {
            let due = last.to_string();
            if !checks.contains(&(t.id, due.clone())) {
                let days = (today - last).num_days();
                overdue.push(make_occ(t, &due, false, days, None));
            }
        }
    }

    // 밀림은 오래된 순(D+n 큰 순)으로
    overdue.sort_by(|a, b| b.days_late.cmp(&a.days_late));
    (overdue, today_list)
}

// ── 오늘 탭 ──────────────────────────────────────────────

#[tauri::command]
pub fn get_today_view(state: State<AppState>) -> Result<TodayView, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let today = Local::now().date_naive();
    let tasks = load_tasks(&conn)?;
    let holidays = load_holidays(&conn)?;
    let checks = load_checks(&conn)?;

    let (overdue, today_list) = compute_overdue_today(&tasks, &holidays, &checks, today);

    // 다가오는 업무: 오늘 이후 14일 내 첫 기한 (매일 주기 제외)
    let mut upcoming = Vec::new();
    for t in &tasks {
        if t.recur_type != "daily" {
            let param = t.recur_param.as_deref().unwrap_or("{}");
            let fut = recur::occurrences_between(
                &t.recur_type,
                param,
                today + Duration::days(1),
                today + Duration::days(14),
                &holidays,
            );
            if let Some(&next) = fut.first() {
                let due = next.to_string();
                let label = format!(
                    "{} · {}/{} 예정",
                    recur::short_recur(&t.recur_type),
                    next.month(),
                    next.day()
                );
                upcoming.push(make_occ(t, &due, false, 0, Some(label)));
            }
        }
    }
    // 다가오는 업무는 임박한 순
    upcoming.sort_by(|a, b| a.due_date.cmp(&b.due_date));

    let (wd, wt) = range_stats(&tasks, &holidays, &checks, week_start(today), today);
    let week_rate = rate(wd, wt);

    Ok(TodayView {
        date: today.to_string(),
        overdue,
        today: today_list,
        upcoming,
        week_rate,
    })
}

// ── 미체크 요약 (알림·트레이 공용) ────────────────────────

/// 알림·트레이용 미체크 집계 스냅샷
pub struct PendingSummary {
    pub overdue: i64,                     // 밀림 건수
    pub total: i64,                       // 오늘 미체크 + 밀림
    pub overdue_keys: Vec<(i64, String)>, // 밀림 신규 발생 감지용 (task_id, due_date)
    pub sample_names: Vec<String>,        // 대표 업무명 최대 2개 (밀림 우선)
}

/// 현재 미체크(오늘+밀림) 요약. compute_overdue_today 를 재사용한다.
pub fn pending_summary(conn: &Connection) -> Result<PendingSummary, String> {
    let today = Local::now().date_naive();
    let tasks = load_tasks(conn)?;
    let holidays = load_holidays(conn)?;
    let checks = load_checks(conn)?;
    let (overdue, today_list) = compute_overdue_today(&tasks, &holidays, &checks, today);

    let today_unchecked_occ: Vec<&TaskOccurrence> =
        today_list.iter().filter(|o| !o.checked).collect();
    let today_unchecked = today_unchecked_occ.len() as i64;
    let overdue_cnt = overdue.len() as i64;

    let overdue_keys: Vec<(i64, String)> = overdue
        .iter()
        .map(|o| (o.task_id, o.due_date.clone()))
        .collect();

    // 대표 업무명: 밀림 먼저, 그다음 오늘 미체크 — 최대 2개
    let mut sample_names: Vec<String> = Vec::new();
    for o in overdue.iter().chain(today_unchecked_occ.iter().copied()) {
        if sample_names.len() >= 2 {
            break;
        }
        sample_names.push(o.name.clone());
    }

    Ok(PendingSummary {
        overdue: overdue_cnt,
        total: today_unchecked + overdue_cnt,
        overdue_keys,
        sample_names,
    })
}

/// Setting 단일 값 조회 (알림 스케줄러 등 내부용)
pub(crate) fn read_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM Setting WHERE key=?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Setting 단일 값 기록 (알림 스케줄러 등 내부용)
pub(crate) fn write_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO Setting(key, value) VALUES(?1, ?2)",
        params![key, value],
    )
    .map_err(e2s)?;
    Ok(())
}

// ── 체크 토글 ────────────────────────────────────────────

#[tauri::command]
pub fn toggle_check(
    app: tauri::AppHandle,
    state: State<AppState>,
    task_id: i64,
    due_date: String,
) -> Result<(), String> {
    // DB 락은 블록 안에서만 점유 (이후 update_tooltip 이 재락하므로 먼저 해제)
    {
        let conn = state.db.lock().map_err(e2s)?;
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM CheckLog WHERE task_id=?1 AND due_date=?2",
                params![task_id, due_date],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if exists {
            conn.execute(
                "DELETE FROM CheckLog WHERE task_id=?1 AND due_date=?2",
                params![task_id, due_date],
            )
            .map_err(e2s)?;
        } else {
            conn.execute(
                "INSERT INTO CheckLog(task_id, due_date, checked_at) VALUES(?1, ?2, ?3)",
                params![task_id, due_date, now_str()],
            )
            .map_err(e2s)?;
        }
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

// ── 업무 CRUD ────────────────────────────────────────────

#[tauri::command]
pub fn list_tasks(state: State<AppState>) -> Result<Vec<Task>, String> {
    let conn = state.db.lock().map_err(e2s)?;
    load_tasks(&conn)
}

#[tauri::command]
pub fn add_task(app: tauri::AppHandle, state: State<AppState>, dto: TaskDto) -> Result<i64, String> {
    let id = {
        let conn = state.db.lock().map_err(e2s)?;
        conn.execute(
            "INSERT INTO Task(name,memo,links,recur_type,recur_param,active,sort_order,created_at) \
             VALUES(?1,?2,?3,?4,?5,1,?6,?7)",
            params![
                dto.name,
                dto.memo,
                dto.links,
                dto.recur_type,
                dto.recur_param,
                dto.sort_order.unwrap_or(0),
                now_str()
            ],
        )
        .map_err(e2s)?;
        conn.last_insert_rowid()
    };
    crate::tray::update_tooltip(&app);
    Ok(id)
}

#[tauri::command]
pub fn update_task(app: tauri::AppHandle, state: State<AppState>, dto: TaskDto) -> Result<(), String> {
    let id = dto.id.ok_or_else(|| "업무 id가 없습니다".to_string())?;
    {
        let conn = state.db.lock().map_err(e2s)?;
        conn.execute(
            "UPDATE Task SET name=?1,memo=?2,links=?3,recur_type=?4,recur_param=?5,sort_order=?6 \
             WHERE id=?7",
            params![
                dto.name,
                dto.memo,
                dto.links,
                dto.recur_type,
                dto.recur_param,
                dto.sort_order.unwrap_or(0),
                id
            ],
        )
        .map_err(e2s)?;
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

#[tauri::command]
pub fn delete_task(app: tauri::AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(e2s)?;
        conn.execute("DELETE FROM CheckLog WHERE task_id=?1", params![id])
            .map_err(e2s)?;
        conn.execute("DELETE FROM Task WHERE id=?1", params![id])
            .map_err(e2s)?;
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

// ── 통계 탭 ──────────────────────────────────────────────

#[tauri::command]
pub fn get_stats(state: State<AppState>) -> Result<Stats, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let today = Local::now().date_naive();
    let tasks = load_tasks(&conn)?;
    let holidays = load_holidays(&conn)?;
    let checks = load_checks(&conn)?;

    // 연속 달성일: 오늘부터 과거로, 기한 있는 날 전부 완료면 +1.
    // 오늘이 미완료면 오늘은 건너뛴다(진행 중이므로 스트릭을 끊지 않음).
    let mut streak_days = 0i64;
    let mut d = today;
    for _ in 0..120 {
        let (done, total) = range_stats(&tasks, &holidays, &checks, d, d);
        if total > 0 {
            if done == total {
                streak_days += 1;
            } else if d != today {
                break;
            }
            // d == today && 미완료 → 건너뛰기
        }
        d -= Duration::days(1);
    }

    // 이번 주 / 이번 달 / 분기 수행률
    let (wd, wt) = range_stats(&tasks, &holidays, &checks, week_start(today), today);
    let month_start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
    let (md, mt) = range_stats(&tasks, &holidays, &checks, month_start, today);
    let q_start_month = ((today.month() - 1) / 3) * 3 + 1;
    let q_start = NaiveDate::from_ymd_opt(today.year(), q_start_month, 1).unwrap();
    let (qd, qt) = range_stats(&tasks, &holidays, &checks, q_start, today);

    // 이번 달 히트맵
    let last_day = {
        let (ny, nm) = if today.month() == 12 {
            (today.year() + 1, 1)
        } else {
            (today.year(), today.month() + 1)
        };
        (NaiveDate::from_ymd_opt(ny, nm, 1).unwrap() - Duration::days(1)).day()
    };
    let mut heatmap = Vec::new();
    for day in 1..=last_day {
        let date = NaiveDate::from_ymd_opt(today.year(), today.month(), day).unwrap();
        let (done, total) = range_stats(&tasks, &holidays, &checks, date, date);
        heatmap.push(HeatCell {
            date: date.to_string(),
            done,
            total,
        });
    }

    Ok(Stats {
        streak_days,
        month_rate: rate(md, mt),
        week_rate: rate(wd, wt),
        quarter_rate: rate(qd, qt),
        heatmap,
    })
}

// ── 설정 ─────────────────────────────────────────────────

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Result<Vec<Setting>, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM Setting WHERE key != 'seed_holidays_2026' ORDER BY key")
        .map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Setting {
                key: r.get(0)?,
                value: r.get(1)?,
            })
        })
        .map_err(e2s)?;
    let mut v = Vec::new();
    for row in rows {
        v.push(row.map_err(e2s)?);
    }
    Ok(v)
}

#[tauri::command]
pub fn set_setting(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(e2s)?;
    conn.execute(
        "INSERT OR REPLACE INTO Setting(key, value) VALUES(?1, ?2)",
        params![key, value],
    )
    .map_err(e2s)?;
    Ok(())
}

// ── 공휴일 ───────────────────────────────────────────────

#[tauri::command]
pub fn list_holidays(state: State<AppState>) -> Result<Vec<Holiday>, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let mut stmt = conn
        .prepare("SELECT date, name FROM Holiday ORDER BY date")
        .map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Holiday {
                date: r.get(0)?,
                name: r.get(1)?,
            })
        })
        .map_err(e2s)?;
    let mut v = Vec::new();
    for row in rows {
        v.push(row.map_err(e2s)?);
    }
    Ok(v)
}

#[tauri::command]
pub fn add_holiday(state: State<AppState>, date: String, name: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(e2s)?;
    conn.execute(
        "INSERT OR REPLACE INTO Holiday(date, name) VALUES(?1, ?2)",
        params![date, name],
    )
    .map_err(e2s)?;
    Ok(())
}

#[tauri::command]
pub fn delete_holiday(state: State<AppState>, date: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(e2s)?;
    conn.execute("DELETE FROM Holiday WHERE date=?1", params![date])
        .map_err(e2s)?;
    Ok(())
}

// ── M2: 알림 · 자동 시작 (플러그인은 JS 직접 호출 대신 커맨드로 래핑) ──

/// 즉시 샘플 토스트 발송 (설정 화면의 알림 테스트용)
#[tauri::command]
pub fn test_notification(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title("업무 체크")
        .body("알림 테스트 — 정상적으로 표시됩니다.")
        .show()
        .map_err(e2s)
}

/// 부팅 시 자동 시작 여부 조회
#[tauri::command]
pub fn get_autostart(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(e2s)
}

/// 부팅 시 자동 시작 설정
#[tauri::command]
pub fn set_autostart(app: tauri::AppHandle, enable: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let m = app.autolaunch();
    if enable {
        m.enable().map_err(e2s)
    } else {
        m.disable().map_err(e2s)
    }
}
