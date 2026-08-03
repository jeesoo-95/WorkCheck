// Tauri commands — 프론트엔드가 invoke 로 호출하는 API
// 모든 command 는 Result<T, String> 로 에러를 반환한다.

use crate::model::*;
use crate::recur;
use chrono::{Datelike, Duration, Local, NaiveDate};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Manager, State};

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
            "SELECT id,name,memo,links,recur_type,recur_param,active,sort_order,created_at,notify_time,remind_before,priority \
             FROM Task WHERE active=1 ORDER BY priority, sort_order, id",
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
                notify_time: r.get(9)?,
                remind_before: r.get(10)?,
                priority: r.get(11)?,
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

/// (task_id, due_date) → CheckInfo(status, memo) 체크 이력 맵.
/// 존재 여부만 필요한 곳은 contains_key, 상태 판정이 필요한 곳은 값을 본다.
fn load_checks(conn: &Connection) -> Result<HashMap<(i64, String), CheckInfo>, String> {
    let mut stmt = conn
        .prepare("SELECT task_id, due_date, status, memo FROM CheckLog")
        .map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                (r.get::<_, i64>(0)?, r.get::<_, String>(1)?),
                CheckInfo {
                    status: r.get::<_, String>(2)?,
                    memo: r.get::<_, Option<String>>(3)?,
                },
            ))
        })
        .map_err(e2s)?;
    let mut map = HashMap::new();
    for row in rows {
        let (k, v) = row.map_err(e2s)?;
        map.insert(k, v);
    }
    Ok(map)
}

/// 체크 맵에서 (task_id, due_date) 의 상태 문자열. 없으면 "none".
fn status_of(checks: &HashMap<(i64, String), CheckInfo>, task_id: i64, due: &str) -> String {
    checks
        .get(&(task_id, due.to_string()))
        .map(|c| c.status.clone())
        .unwrap_or_else(|| "none".to_string())
}

/// TaskOccurrence 생성 헬퍼.
/// status: "none"|"done"|"skip". checked 는 status=="done" 로 파생(하위호환).
fn make_occ(
    t: &Task,
    due_date: &str,
    status: &str,
    check_memo: Option<String>,
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
        status: status.to_string(),
        checked: status == "done",
        check_memo,
        days_late,
        upcoming_label,
        priority: t.priority,
    }
}

/// 1회성(once) 완료 여부: recur_type=="once" 이고 지정 기한일(param.date)에
/// 완료(status=="done") 이력이 있으면 true. 건너뜀(skip)은 완료가 아니므로 false.
/// (기존 데이터는 전부 'done' 이라 v1 동작과 동일)
fn is_done_once(t: &Task, checks: &HashMap<(i64, String), CheckInfo>) -> bool {
    if t.recur_type != "once" {
        return false;
    }
    t.recur_param
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("date").and_then(|d| d.as_str()).map(String::from))
        .map(|date| status_of(checks, t.id, &date) == "done")
        .unwrap_or(false)
}

/// 업무 생성일 (파싱 실패 시 None → 클램프 없음)
fn created_date(t: &Task) -> Option<NaiveDate> {
    t.created_at
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok())
}

/// 생성일 이전은 발생 회차로 치지 않도록 from 을 클램프.
/// 단 1회성(once)은 사용자가 의도적으로 과거 기한을 지정할 수 있어(등록 즉시 밀림 D+n)
/// 클램프 없이 from 을 그대로 쓴다.
fn clamp_from(t: &Task, from: NaiveDate) -> NaiveDate {
    if t.recur_type == "once" {
        return from;
    }
    match created_date(t) {
        Some(c) if c > from => c,
        _ => from,
    }
}

/// [from,to] 구간의 (완료수, 전체수, 건너뜀수) — 업무별로 생성일 이후만 집계.
/// 수행률 정의: skip 회차는 분모·분자 모두 제외(전체수에 넣지 않음). done 만 분자.
/// 즉 rate = done / (전체회차 - skip). skipped 는 별도로 반환(히트맵 회색 처리용).
fn range_counts(
    tasks: &[Task],
    holidays: &HashSet<NaiveDate>,
    checks: &HashMap<(i64, String), CheckInfo>,
    from: NaiveDate,
    to: NaiveDate,
) -> (i64, i64, i64) {
    let mut done = 0;
    let mut total = 0;
    let mut skipped = 0;
    for t in tasks {
        let param = t.recur_param.as_deref().unwrap_or("{}");
        for d in recur::occurrences_between(&t.recur_type, param, clamp_from(t, from), to, holidays) {
            match status_of(checks, t.id, &d.to_string()).as_str() {
                "skip" => skipped += 1, // 분모·분자 모두 제외
                "done" => {
                    total += 1;
                    done += 1;
                }
                _ => total += 1, // 미체크(none)
            }
        }
    }
    (done, total, skipped)
}

/// [from,to] 구간의 (완료수, 전체수) — skip 제외. range_counts 의 얇은 래퍼.
fn range_stats(
    tasks: &[Task],
    holidays: &HashSet<NaiveDate>,
    checks: &HashMap<(i64, String), CheckInfo>,
    from: NaiveDate,
    to: NaiveDate,
) -> (i64, i64) {
    let (done, total, _skipped) = range_counts(tasks, holidays, checks, from, to);
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
    checks: &HashMap<(i64, String), CheckInfo>,
    today: NaiveDate,
) -> (Vec<TaskOccurrence>, Vec<TaskOccurrence>) {
    let mut overdue = Vec::new();
    let mut today_list = Vec::new();

    for t in tasks {
        let param = t.recur_param.as_deref().unwrap_or("{}");

        // 오늘 기한 (완료/건너뜀 여부와 무관하게 표시 — 상태 배지·메모 포함)
        let today_occ = recur::occurrences_between(&t.recur_type, param, today, today, holidays);
        if !today_occ.is_empty() {
            let due = today.to_string();
            let info = checks.get(&(t.id, due.clone()));
            let status = info.map(|c| c.status.clone()).unwrap_or_else(|| "none".to_string());
            let memo = info.and_then(|c| c.memo.clone());
            today_list.push(make_occ(t, &due, &status, memo, 0, None));
        }

        // 밀림: 오늘 이전 가장 최근 기한일에 기록이 없으면(done/skip 어느 쪽도 아님) D+n.
        // skip 도 CheckLog 기록이 있으므로 밀림이 아님(존재 여부 기준 유지).
        let past_from = clamp_from(t, today - Duration::days(120));
        let past =
            recur::occurrences_between(&t.recur_type, param, past_from, today - Duration::days(1), holidays);
        if let Some(&last) = past.last() {
            let due = last.to_string();
            if !checks.contains_key(&(t.id, due.clone())) {
                let days = (today - last).num_days();
                overdue.push(make_occ(t, &due, "none", None, days, None));
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
                upcoming.push(make_occ(t, &due, "none", None, 0, Some(label)));
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

// ── 업무별 알림 · 사전 리마인드 (스케줄러 전용) ──────────

/// 한 업무의 알림 판정 스냅샷 (notify.rs 스케줄러가 시각 비교·발송에 사용).
pub struct PerTaskNotify {
    pub task_id: i64,
    pub name: String,
    /// notify_time 설정 + 오늘 기한 회차가 status=none 이면 Some("HH:MM").
    /// (오늘 회차가 없거나 이미 완료/건너뜀이면 None)
    pub today_notify_time: Option<String>,
    /// remind_before=N 이고 다음 기한일(오늘+1~오늘+31 첫 항목)이 정확히 오늘+N 이면
    /// Some((N, 기한일)). 그 외 None.
    pub remind: Option<(i64, NaiveDate)>,
}

/// 업무별 개별 알림·사전 리마인드 후보 목록. 발송 대상이 하나라도 있는 업무만 반환.
/// 시각(현재시각 ≥ 알림시각) 비교와 발송·중복방지는 스케줄러(notify.rs)가 담당한다.
pub fn per_task_notify(conn: &Connection) -> Result<Vec<PerTaskNotify>, String> {
    let today = Local::now().date_naive();
    let tasks = load_tasks(conn)?;
    let holidays = load_holidays(conn)?;
    let checks = load_checks(conn)?;

    let mut out = Vec::new();
    for t in &tasks {
        let param = t.recur_param.as_deref().unwrap_or("{}");

        // 업무별 알림: notify_time 설정 + 오늘 기한 회차 존재 + 아직 미체크(none)
        let today_notify_time = t.notify_time.as_ref().and_then(|nt| {
            let occ = recur::occurrences_between(&t.recur_type, param, today, today, &holidays);
            if occ.is_empty() {
                return None;
            }
            if status_of(&checks, t.id, &today.to_string()) == "none" {
                Some(nt.clone())
            } else {
                None
            }
        });

        // 사전 리마인드: 다음 기한일(오늘+1~오늘+31 첫 항목)이 정확히 오늘+N
        let remind = t.remind_before.and_then(|n| {
            if !(1..=30).contains(&n) {
                return None;
            }
            let fut = recur::occurrences_between(
                &t.recur_type,
                param,
                today + Duration::days(1),
                today + Duration::days(31),
                &holidays,
            );
            fut.first().copied().and_then(|next| {
                if (next - today).num_days() == n {
                    Some((n, next))
                } else {
                    None
                }
            })
        });

        if today_notify_time.is_some() || remind.is_some() {
            out.push(PerTaskNotify {
                task_id: t.id,
                name: t.name.clone(),
                today_notify_time,
                remind,
            });
        }
    }
    Ok(out)
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

/// 회차 상태 설정 (완료/건너뜀/해제).
/// - "none": 해당 회차 기록 삭제
/// - "done"/"skip": INSERT OR REPLACE (checked_at 갱신, memo 반영)
/// - 그 외 값: Err
#[tauri::command]
pub fn set_check_status(
    app: tauri::AppHandle,
    state: State<AppState>,
    task_id: i64,
    due_date: String,
    status: String,
    memo: Option<String>,
) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(e2s)?;
        match status.as_str() {
            "none" => {
                conn.execute(
                    "DELETE FROM CheckLog WHERE task_id=?1 AND due_date=?2",
                    params![task_id, due_date],
                )
                .map_err(e2s)?;
            }
            "done" | "skip" => {
                // memo가 None이면 기존 메모 보존 (소급 모달의 상태 변경이 메모를 지우지 않도록)
                conn.execute(
                    "INSERT INTO CheckLog(task_id, due_date, checked_at, status, memo) \
                     VALUES(?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(task_id, due_date) DO UPDATE SET \
                       checked_at=excluded.checked_at, status=excluded.status, \
                       memo=COALESCE(excluded.memo, CheckLog.memo)",
                    params![task_id, due_date, now_str(), status, memo],
                )
                .map_err(e2s)?;
            }
            _ => return Err(format!("알 수 없는 상태입니다: {}", status)),
        }
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

/// 기존 회차의 완료 메모만 갱신. 체크 기록이 없으면 Err.
#[tauri::command]
pub fn set_check_memo(
    state: State<AppState>,
    task_id: i64,
    due_date: String,
    memo: String,
) -> Result<(), String> {
    let conn = state.db.lock().map_err(e2s)?;
    let n = conn
        .execute(
            "UPDATE CheckLog SET memo=?1 WHERE task_id=?2 AND due_date=?3",
            params![memo, task_id, due_date],
        )
        .map_err(e2s)?;
    if n == 0 {
        return Err("체크된 회차가 없습니다".to_string());
    }
    Ok(())
}

/// 소급 체크용 — 지정 날짜(과거·오늘만)의 기한 회차 목록 + status/memo.
/// 미래 날짜면 빈 목록. 생성일 클램프는 다른 화면과 동일하게 적용.
#[tauri::command]
pub fn get_day_view(state: State<AppState>, date: String) -> Result<Vec<TaskOccurrence>, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let today = Local::now().date_naive();
    let target = NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(e2s)?;
    // 미래 날짜는 소급 대상이 아님
    if target > today {
        return Ok(Vec::new());
    }
    let tasks = load_tasks(&conn)?;
    let holidays = load_holidays(&conn)?;
    let checks = load_checks(&conn)?;

    let mut out = Vec::new();
    for t in &tasks {
        // 생성일 이후만 (once 제외 클램프). target 이 생성일 이전이면 회차 아님.
        if clamp_from(t, target) > target {
            continue;
        }
        let param = t.recur_param.as_deref().unwrap_or("{}");
        let occ = recur::occurrences_between(&t.recur_type, param, target, target, &holidays);
        if !occ.is_empty() {
            let due = target.to_string();
            let info = checks.get(&(t.id, due.clone()));
            let status = info.map(|c| c.status.clone()).unwrap_or_else(|| "none".to_string());
            let memo = info.and_then(|c| c.memo.clone());
            let days = (today - target).num_days();
            out.push(make_occ(t, &due, &status, memo, days, None));
        }
    }
    Ok(out)
}

// ── 주기 미리보기 (업무 등록/수정 폼) ─────────────────────

/// 주기 규칙 요약 + 다음 3회 기한일 미리보기.
/// 오늘 ~ +2년 구간에서 실제 Holiday 테이블을 적용해 계산한다(등록 화면의 오등록 방지 장치).
/// 생성되는 날짜가 하나도 없으면 next 는 비고 error 에 안내 문구를 담는다.
#[tauri::command]
pub fn preview_recur(
    state: State<AppState>,
    recur_type: String,
    recur_param: String,
) -> Result<RecurPreview, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let holidays = load_holidays(&conn)?;
    let today = Local::now().date_naive();
    let occ = recur::occurrences_between(
        &recur_type,
        &recur_param,
        today,
        today + Duration::days(730), // +2년
        &holidays,
    );
    let next: Vec<String> = occ.iter().take(3).map(|d| d.to_string()).collect();
    let error = if next.is_empty() {
        Some("선택한 조건으로 생성되는 날짜가 없습니다. 요일/일자 선택을 확인하세요.".to_string())
    } else {
        None
    };
    Ok(RecurPreview {
        summary: recur::rule_summary(&recur_type, &recur_param),
        next,
        error,
    })
}

// ── 업무 CRUD ────────────────────────────────────────────

#[tauri::command]
pub fn list_tasks(state: State<AppState>) -> Result<Vec<TaskListItem>, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let tasks = load_tasks(&conn)?;
    let checks = load_checks(&conn)?;
    let items = tasks
        .into_iter()
        .map(|t| {
            let done_once = is_done_once(&t, &checks);
            // 라벨은 백엔드가 만들어 내려준다 (프론트 재구현 금지 — make_occ 와 동일 규칙)
            let rule_label = recur::rule_label(&t.recur_type, t.recur_param.as_deref().unwrap_or("{}"));
            TaskListItem { task: t, rule_label, done_once }
        })
        .collect();
    Ok(items)
}

#[tauri::command]
pub fn add_task(app: tauri::AppHandle, state: State<AppState>, dto: TaskDto) -> Result<i64, String> {
    let id = {
        let conn = state.db.lock().map_err(e2s)?;
        conn.execute(
            "INSERT INTO Task(name,memo,links,recur_type,recur_param,active,sort_order,created_at,notify_time,remind_before,priority) \
             VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8,?9,?10)",
            params![
                dto.name,
                dto.memo,
                dto.links,
                dto.recur_type,
                dto.recur_param,
                dto.sort_order.unwrap_or(0),
                now_str(),
                dto.notify_time,
                dto.remind_before,
                dto.priority.unwrap_or(1)
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
            "UPDATE Task SET name=?1,memo=?2,links=?3,recur_type=?4,recur_param=?5,sort_order=?6,\
             notify_time=?7,remind_before=?8,priority=?9 WHERE id=?10",
            params![
                dto.name,
                dto.memo,
                dto.links,
                dto.recur_type,
                dto.recur_param,
                dto.sort_order.unwrap_or(0),
                dto.notify_time,
                dto.remind_before,
                dto.priority.unwrap_or(1),
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

/// 드래그 정렬 — 전달받은 id 순서대로 sort_order=0,1,2… 를 일괄 부여(트랜잭션).
/// 프론트에서 같은 주기 그룹의 표시 순서를 그대로 넘긴다(그룹 간 이동 없음).
/// load_tasks 는 ORDER BY sort_order,id 이고 프론트가 recur_type 로 재그룹핑하므로
/// 그룹별로 0..N 이 겹쳐도 그룹 내 상대 순서는 안정적으로 유지된다.
#[tauri::command]
pub fn set_sort_order(state: State<AppState>, ids: Vec<i64>) -> Result<(), String> {
    let mut conn = state.db.lock().map_err(e2s)?;
    let tx = conn.transaction().map_err(e2s)?;
    for (i, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE Task SET sort_order=?1 WHERE id=?2",
            params![i as i64, id],
        )
        .map_err(e2s)?;
    }
    tx.commit().map_err(e2s)?;
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
    // skip 회차는 range_stats 의 total 에서 제외되므로, 그날이 전부 skip 이면
    // total==0 이 되어 스트릭을 끊지 않고 자연히 건너뛴다.
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
        let (done, total, skipped) = range_counts(&tasks, &holidays, &checks, date, date);
        heatmap.push(HeatCell {
            date: date.to_string(),
            done,
            total,
            skipped,
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

// ── 리포트 생성 (통계 탭) ────────────────────────────────

/// 리포트 한 항목(회차): 날짜·업무명·상태·완료메모.
struct ReportItem {
    date: NaiveDate,
    name: String,
    status: String, // "done" | "skip" | "none"
    memo: Option<String>,
}

/// [from,to] 구간 회차를 모아 Markdown 리포트 문자열을 만든다(순수 함수).
/// - to 는 오늘로 클램프해 미래 회차를 제외한다.
/// - 판정은 status_of 규칙으로 단일화(get_stats·오늘 탭과 동일). skip 은 대상(분모)에서 제외.
/// - 항목은 날짜순. 완료 항목만 완료 메모를 덧붙인다.
fn build_report(
    tasks: &[Task],
    holidays: &HashSet<NaiveDate>,
    checks: &HashMap<(i64, String), CheckInfo>,
    from: NaiveDate,
    to_req: NaiveDate,
    today: NaiveDate,
) -> String {
    let to = to_req.min(today); // 미래 회차 제외

    let mut items: Vec<ReportItem> = Vec::new();
    for t in tasks {
        let param = t.recur_param.as_deref().unwrap_or("{}");
        for d in recur::occurrences_between(&t.recur_type, param, clamp_from(t, from), to, holidays) {
            let due = d.to_string();
            let info = checks.get(&(t.id, due));
            let status = info.map(|c| c.status.clone()).unwrap_or_else(|| "none".to_string());
            let memo = info.and_then(|c| c.memo.clone());
            items.push(ReportItem { date: d, name: t.name.clone(), status, memo });
        }
    }
    // 항목은 날짜순 (같은 날짜는 업무 등록순 유지 → stable sort)
    items.sort_by(|a, b| a.date.cmp(&b.date));

    // 집계 (skip 은 대상에서 제외 — range_counts 와 동일 정의)
    let done = items.iter().filter(|i| i.status == "done").count();
    let skipped = items.iter().filter(|i| i.status == "skip").count();
    let none = items.iter().filter(|i| i.status == "none").count();
    let target = done + none; // 대상 = 완료 + 미수행
    let pct = if target == 0 { 0 } else { (done * 100 + target / 2) / target }; // 반올림

    let mut out = String::new();
    out.push_str(&format!("# 업무 수행 리포트 ({} ~ {})\n\n", from, to));
    out.push_str(&format!(
        "수행률 {}% (완료 {} / 대상 {} · 건너뜀 {} · 미수행 {})\n",
        pct, done, target, skipped, none
    ));

    // 섹션: 완료 / 건너뜀 / 미수행 (항목 있는 섹션만 출력)
    let mut section = |title: &str, status: &str, with_memo: bool| {
        let rows: Vec<&ReportItem> = items.iter().filter(|i| i.status == status).collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(&format!("\n## {}\n", title));
        for it in rows {
            out.push_str(&format!(
                "- {:02}/{:02}({}) {}",
                it.date.month(),
                it.date.day(),
                recur::weekday_ko(it.date),
                it.name
            ));
            if with_memo {
                if let Some(m) = it.memo.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                    out.push_str(&format!(" — {}", m));
                }
            }
            out.push('\n');
        }
    };
    section("완료", "done", true);
    section("건너뜀", "skip", false);
    section("미수행", "none", false);

    out
}

/// 지정 기간의 Markdown 리포트 생성. from/to 는 "YYYY-MM-DD".
#[tauri::command]
pub fn generate_report(state: State<AppState>, from: String, to: String) -> Result<String, String> {
    let conn = state.db.lock().map_err(e2s)?;
    let today = Local::now().date_naive();
    let from_d = NaiveDate::parse_from_str(&from, "%Y-%m-%d").map_err(e2s)?;
    let to_d = NaiveDate::parse_from_str(&to, "%Y-%m-%d").map_err(e2s)?;
    let tasks = load_tasks(&conn)?;
    let holidays = load_holidays(&conn)?;
    let checks = load_checks(&conn)?;
    Ok(build_report(&tasks, &holidays, &checks, from_d, to_d, today))
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

/// 링크를 기본 브라우저로 연다. http/https 스킴만 허용하고 그 외는 Err.
/// (opener 플러그인을 JS 직접 호출 대신 이 커맨드로 래핑 — 스킴 화이트리스트 강제)
#[tauri::command]
pub fn open_link(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let u = url.trim();
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("http/https 링크만 열 수 있습니다".to_string());
    }
    app.opener().open_url(u, None::<&str>).map_err(e2s)
}

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

// ── M3: 백업 · 복원 (설정 탭) ─────────────────────────────
// 플러그인은 JS 직접 호출 대신 Rust 커맨드로 래핑(dialog 는 blocking API 로 Rust 에서만 사용).

/// 백업 폴더 = app_data_dir/backups (없으면 생성)
fn backups_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(e2s)?.join("backups");
    std::fs::create_dir_all(&dir).map_err(e2s)?;
    Ok(dir)
}

/// 열린 연결을 backups_dir 에 workcheck_YYYYMMDD_HHMMSS.db 로 안전 복사(rusqlite backup API).
/// 완료 후 오래된 백업 정리(최근 7개만 보관). 반환값 = 생성된 파일 경로.
/// setup(자동 백업)과 backup_now 커맨드가 공용으로 사용한다.
pub(crate) fn perform_backup(conn: &Connection, dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(e2s)?;
    let fname = format!("workcheck_{}.db", Local::now().format("%Y%m%d_%H%M%S"));
    let dest = dir.join(&fname);
    conn.backup(rusqlite::DatabaseName::Main, &dest, None)
        .map_err(e2s)?;
    prune_backups(dir, 7);
    Ok(dest.to_string_lossy().into_owned())
}

/// 정기 백업 파일(workcheck_YYYYMMDD_HHMMSS.db)만 대상으로 최근 keep 개만 남기고 삭제.
/// 복원 전 안전본(workcheck_before_restore.db)은 정리 대상에서 제외한다.
fn prune_backups(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.starts_with("workcheck_")
                        && n.ends_with(".db")
                        && n != "workcheck_before_restore.db"
                })
                .unwrap_or(false)
        })
        .collect();
    files.sort(); // 파일명에 타임스탬프 → 이름 오름차순 = 오래된 순
    if files.len() > keep {
        for old in &files[..files.len() - keep] {
            let _ = std::fs::remove_file(old);
        }
    }
}

/// 파일이 SQLite DB 인지 헤더("SQLite format 3\0")로 검증
fn is_sqlite_file(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map(|_| &header == b"SQLite format 3\x00")
        .unwrap_or(false)
}

/// 지금 즉시 백업. 성공 시 생성된 백업 파일 경로를 반환.
#[tauri::command]
pub fn backup_now(app: tauri::AppHandle, state: State<AppState>) -> Result<String, String> {
    let dir = backups_dir(&app)?;
    let conn = state.db.lock().map_err(e2s)?;
    perform_backup(&conn, &dir)
}

/// 백업 파일을 골라 현재 DB 에 복원.
/// dialog 로 .db 파일 선택(취소 시 Err "취소됨") → SQLite 헤더 검증 →
/// 현재 DB 를 workcheck_before_restore.db 로 백업 → 선택 파일을 열린 연결에 역방향 복원.
/// 성공 시 복원 원본 파일 경로 반환. (연결 교체 없이 내용만 교체 → 프론트 리로드로 즉시 반영)
#[tauri::command]
pub fn restore_backup(app: tauri::AppHandle, state: State<AppState>) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .add_filter("SQLite DB", &["db"])
        .blocking_pick_file()
        .ok_or_else(|| "취소됨".to_string())?;
    let src = picked.into_path().map_err(e2s)?;
    if !is_sqlite_file(&src) {
        return Err("올바른 SQLite 백업 파일이 아닙니다".to_string());
    }
    let dir = backups_dir(&app)?;
    let mut conn = state.db.lock().map_err(e2s)?;
    // 복원 전 현재 DB 안전본 확보
    let before = dir.join("workcheck_before_restore.db");
    conn.backup(rusqlite::DatabaseName::Main, &before, None)
        .map_err(e2s)?;
    // 선택 파일 → 열린 연결로 역방향 복원 (내용만 교체)
    conn.restore(
        rusqlite::DatabaseName::Main,
        &src,
        None::<fn(rusqlite::backup::Progress)>,
    )
    .map_err(e2s)?;
    drop(conn); // 락 해제 후 툴팁 갱신 (재잠금 데드락 방지 — 기존 패턴)
    crate::tray::update_tooltip(&app);
    Ok(src.to_string_lossy().into_owned())
}

// ── M4: Jira 알림 (네트워크·파싱은 jira.rs, 여기는 커맨드 래퍼) ──

/// 연결 테스트 — /rest/api/3/myself 호출. 성공 시 accountId 는 프론트가 Setting 에 저장.
#[tauri::command]
pub fn jira_test_connection(url: String, email: String, token: String) -> Result<JiraUser, String> {
    crate::jira::test_connection(&url, &email, &token)
}

/// 수동 새로고침 — 즉시 폴링(활성/미설정 여부는 jira::poll_now 가 판정).
#[tauri::command]
pub fn jira_poll_now(app: tauri::AppHandle) -> Result<PollResult, String> {
    Ok(crate::jira::poll_now(&app))
}

/// LIKE 패턴 특수문자(%, _, \)를 ESCAPE '\' 기준으로 이스케이프한다.
/// 사용자 검색어를 `%{escape_like(q)}%` 형태로 감싸 부분일치 검색에 사용.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 알림 피드 조회 (event_at 내림차순). only_unread·category(전체=None/"all")·query 필터 + 페이징.
#[tauri::command]
pub fn get_jira_notifications(
    state: State<AppState>,
    only_unread: bool,
    category: Option<String>,
    project: Option<String>,
    query: Option<String>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JiraNotificationRow>, String> {
    use rusqlite::types::Value as SqlValue;
    let conn = state.db.lock().map_err(e2s)?;

    // 필터를 위치 바인딩으로 동적 구성 (category·project 는 "all"/빈값이면 미적용)
    let cat = category.filter(|c| c != "all" && !c.is_empty());
    let proj = project.filter(|p| p != "all" && !p.is_empty());
    let mut where_sql = String::from(" WHERE 1=1");
    let mut binds: Vec<SqlValue> = Vec::new();
    if only_unread {
        where_sql.push_str(" AND read=0");
    }
    if let Some(c) = &cat {
        binds.push(SqlValue::Text(c.clone()));
        where_sql.push_str(&format!(" AND category=?{}", binds.len()));
    }
    if let Some(p) = &proj {
        binds.push(SqlValue::Text(p.clone()));
        where_sql.push_str(&format!(" AND project_key=?{}", binds.len()));
    }
    // 검색어(트림 후 비어있지 않으면): issue_key·summary·detail·actor 부분일치.
    if let Some(q) = query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        binds.push(SqlValue::Text(format!("%{}%", escape_like(q))));
        let p = binds.len();
        where_sql.push_str(&format!(
            " AND (issue_key LIKE ?{p} ESCAPE '\\' OR summary LIKE ?{p} ESCAPE '\\' \
             OR detail LIKE ?{p} ESCAPE '\\' OR actor LIKE ?{p} ESCAPE '\\')",
            p = p
        ));
    }
    binds.push(SqlValue::Integer(limit));
    let limit_pos = binds.len();
    binds.push(SqlValue::Integer(offset));
    let offset_pos = binds.len();

    let sql = format!(
        "SELECT id,event_uid,issue_key,project_key,category,summary,detail,actor,event_at,fetched_at,read \
         FROM JiraNotification{} ORDER BY event_at DESC LIMIT ?{} OFFSET ?{}",
        where_sql, limit_pos, offset_pos
    );
    let mut stmt = conn.prepare(&sql).map_err(e2s)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
            Ok(JiraNotificationRow {
                id: r.get(0)?,
                event_uid: r.get(1)?,
                issue_key: r.get(2)?,
                project_key: r.get(3)?,
                category: r.get(4)?,
                summary: r.get(5)?,
                detail: r.get(6)?,
                actor: r.get(7)?,
                event_at: r.get(8)?,
                fetched_at: r.get(9)?,
                read: r.get(10)?,
            })
        })
        .map_err(e2s)?;
    let mut v = Vec::new();
    for row in rows {
        v.push(row.map_err(e2s)?);
    }
    Ok(v)
}

/// 안읽음 개수 (탭 배지용)
#[tauri::command]
pub fn get_jira_unread_count(state: State<AppState>) -> Result<i64, String> {
    let conn = state.db.lock().map_err(e2s)?;
    crate::jira::unread_count(&conn)
}

/// 개별/선택 읽음 처리
#[tauri::command]
pub fn mark_jira_read(app: tauri::AppHandle, state: State<AppState>, ids: Vec<i64>) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(e2s)?;
        for id in &ids {
            conn.execute(
                "UPDATE JiraNotification SET read=1 WHERE id=?1",
                params![id],
            )
            .map_err(e2s)?;
        }
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

/// 전체 읽음 처리
#[tauri::command]
pub fn mark_all_jira_read(app: tauri::AppHandle, state: State<AppState>) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(e2s)?;
        conn.execute("UPDATE JiraNotification SET read=1 WHERE read=0", [])
            .map_err(e2s)?;
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

/// 알림 1건의 read 상태 UPDATE (커맨드에서 분리 — 단위 테스트 대상). 갱신된 행 수 반환.
/// 없는 id 면 0행(Err 아님).
fn set_jira_read_db(conn: &Connection, id: i64, read: bool) -> Result<usize, String> {
    conn.execute(
        "UPDATE JiraNotification SET read=?1 WHERE id=?2",
        params![read, id],
    )
    .map_err(e2s)
}

/// 알림 1건 읽음/안읽음 설정 (피드 행의 상태 점 버튼).
/// read=true → 읽음, false → 안읽음. 갱신 후 트레이 툴팁(안읽음 수)을 다시 계산한다.
/// 없는 id 는 0행 갱신으로 조용히 통과 — mark_jira_read 와 동일한 관용.
#[tauri::command]
pub fn set_jira_read(
    app: tauri::AppHandle,
    state: State<AppState>,
    id: i64,
    read: bool,
) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(e2s)?;
        set_jira_read_db(&conn, id, read)?;
    }
    crate::tray::update_tooltip(&app);
    Ok(())
}

/// 프로젝트별 안읽음/전체 수 집계 (project_key 오름차순). 커맨드에서 분리 — 단위 테스트 대상.
fn jira_project_counts_db(conn: &Connection) -> Result<Vec<JiraProjectCount>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT project_key, SUM(CASE WHEN read=0 THEN 1 ELSE 0 END) AS unread, \
             COUNT(*) AS total FROM JiraNotification GROUP BY project_key ORDER BY project_key",
        )
        .map_err(e2s)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(JiraProjectCount {
                project_key: r.get(0)?,
                unread: r.get(1)?,
                total: r.get(2)?,
            })
        })
        .map_err(e2s)?;
    let mut v = Vec::new();
    for row in rows {
        v.push(row.map_err(e2s)?);
    }
    Ok(v)
}

/// 프로젝트별 알림 수 (프로젝트 칩 줄·배지용).
#[tauri::command]
pub fn get_jira_project_counts(state: State<AppState>) -> Result<Vec<JiraProjectCount>, String> {
    let conn = state.db.lock().map_err(e2s)?;
    jira_project_counts_db(&conn)
}

// ── 단위 테스트 ──────────────────────────────────────────
// range_stats / range_counts 는 Connection 없이 동작하는 순수 함수라 여기서 검증한다.
#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// 매일(daily) 업무 하나 생성 (생성일은 충분히 과거).
    fn daily_task(id: i64) -> Task {
        Task {
            id,
            name: format!("t{}", id),
            memo: None,
            links: None,
            recur_type: "daily".to_string(),
            recur_param: Some("{}".to_string()),
            active: 1,
            sort_order: Some(0),
            created_at: Some("2020-01-01".to_string()),
            notify_time: None,
            remind_before: None,
            priority: 1,
        }
    }

    fn ci(status: &str) -> CheckInfo {
        CheckInfo {
            status: status.to_string(),
            memo: None,
        }
    }

    // 1) skip 회차는 분모(total)·분자(done) 모두에서 제외된다.
    #[test]
    fn range_stats_skip_excluded_from_denominator() {
        let tasks = vec![daily_task(1)];
        let holidays: HashSet<NaiveDate> = HashSet::new();
        let mut checks: HashMap<(i64, String), CheckInfo> = HashMap::new();
        // 3일 구간(20~22): 20 done, 21 skip, 22 none
        checks.insert((1, "2026-07-20".to_string()), ci("done"));
        checks.insert((1, "2026-07-21".to_string()), ci("skip"));

        let (done, total, skipped) =
            range_counts(&tasks, &holidays, &checks, d(2026, 7, 20), d(2026, 7, 22));
        // skip 1건 제외 → 전체 3회차 중 total=2(20 done, 22 none), done=1, skipped=1
        assert_eq!((done, total, skipped), (1, 2, 1));
        // range_stats 래퍼는 (done, total) 만 반환
        assert_eq!(
            range_stats(&tasks, &holidays, &checks, d(2026, 7, 20), d(2026, 7, 22)),
            (1, 2)
        );
    }

    // 2) done 집계: 모두 완료면 done==total, skipped==0. rate=100%.
    #[test]
    fn range_stats_all_done() {
        let tasks = vec![daily_task(1)];
        let holidays: HashSet<NaiveDate> = HashSet::new();
        let mut checks: HashMap<(i64, String), CheckInfo> = HashMap::new();
        checks.insert((1, "2026-07-20".to_string()), ci("done"));
        checks.insert((1, "2026-07-21".to_string()), ci("done"));

        let (done, total, skipped) =
            range_counts(&tasks, &holidays, &checks, d(2026, 7, 20), d(2026, 7, 21));
        assert_eq!((done, total, skipped), (2, 2, 0));
        assert_eq!(rate(done, total), 1.0);
    }

    // 3) 전부 skip 인 날은 total==0 → 스트릭·수행률에서 자연히 제외된다.
    #[test]
    fn range_stats_all_skip_yields_zero_total() {
        let tasks = vec![daily_task(1)];
        let holidays: HashSet<NaiveDate> = HashSet::new();
        let mut checks: HashMap<(i64, String), CheckInfo> = HashMap::new();
        checks.insert((1, "2026-07-20".to_string()), ci("skip"));

        let (done, total, skipped) =
            range_counts(&tasks, &holidays, &checks, d(2026, 7, 20), d(2026, 7, 20));
        assert_eq!((done, total, skipped), (0, 0, 1));
    }

    // 4) 리포트: 헤더 수행률·집계 + 완료 메모 + 미래 회차(오늘 이후) 제외.
    #[test]
    fn build_report_header_and_sections() {
        let mut t = daily_task(1);
        t.name = "일일점검".to_string();
        let tasks = vec![t];
        let holidays: HashSet<NaiveDate> = HashSet::new();
        let mut checks: HashMap<(i64, String), CheckInfo> = HashMap::new();
        // 20 done(메모), 21 skip, 22 none. 오늘=22 → 23(요청 to) 는 미래라 제외.
        checks.insert(
            (1, "2026-07-20".to_string()),
            CheckInfo { status: "done".to_string(), memo: Some("완료함".to_string()) },
        );
        checks.insert((1, "2026-07-21".to_string()), ci("skip"));

        let out = build_report(&tasks, &holidays, &checks, d(2026, 7, 20), d(2026, 7, 23), d(2026, 7, 22));
        // 대상=2(20 done, 22 none), 완료 1 → 수행률 50%
        assert!(out.contains("# 업무 수행 리포트 (2026-07-20 ~ 2026-07-22)"));
        assert!(out.contains("수행률 50% (완료 1 / 대상 2 · 건너뜀 1 · 미수행 1)"));
        assert!(out.contains("## 완료\n- 07/20(월) 일일점검 — 완료함"));
        assert!(out.contains("## 건너뜀\n- 07/21(화) 일일점검"));
        assert!(out.contains("## 미수행\n- 07/22(수) 일일점검"));
        // 미래(07/23) 회차는 포함되지 않음
        assert!(!out.contains("07/23"));
    }

    // 5) 리포트: 대상 0(전부 skip)이면 수행률 0%, 빈 섹션은 생략.
    #[test]
    fn build_report_all_skip_zero_rate() {
        let tasks = vec![daily_task(1)];
        let holidays: HashSet<NaiveDate> = HashSet::new();
        let mut checks: HashMap<(i64, String), CheckInfo> = HashMap::new();
        checks.insert((1, "2026-07-20".to_string()), ci("skip"));

        let out = build_report(&tasks, &holidays, &checks, d(2026, 7, 20), d(2026, 7, 20), d(2026, 7, 20));
        assert!(out.contains("수행률 0% (완료 0 / 대상 0 · 건너뜀 1 · 미수행 0)"));
        assert!(out.contains("## 건너뜀"));
        assert!(!out.contains("## 완료"));
        assert!(!out.contains("## 미수행"));
    }

    // 6) LIKE 이스케이프: %, _, \ 는 앞에 \ 를 붙이고 일반 문자는 그대로 둔다.
    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like("abc"), "abc");
        assert_eq!(escape_like("50%"), "50\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c:\\dir"), "c:\\\\dir");
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
        // 한글·일반 문자는 이스케이프하지 않음
        assert_eq!(escape_like("로그인"), "로그인");
    }

    // 7) set_jira_read_db: 안읽음↔읽음 토글, 없는 id 는 0행(Err 아님).
    #[test]
    fn set_jira_read_toggles_read_flag() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_test(&conn);
        conn.execute(
            "INSERT INTO JiraNotification \
             (event_uid, issue_key, project_key, category, summary, detail, actor, event_at, fetched_at, read) \
             VALUES('u1','APP-1','APP','comment','s','d','a','2026-07-21T10:00:00+09:00','2026-07-21 10:00:00',0)",
            [],
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM JiraNotification WHERE event_uid='u1'", [], |r| r.get(0))
            .unwrap();
        let read_of = |c: &Connection, id: i64| -> i64 {
            c.query_row("SELECT read FROM JiraNotification WHERE id=?1", params![id], |r| r.get(0))
                .unwrap()
        };
        // 안읽음(0) → 읽음(1)
        assert_eq!(set_jira_read_db(&conn, id, true).unwrap(), 1);
        assert_eq!(read_of(&conn, id), 1);
        // 읽음(1) → 안읽음(0)
        assert_eq!(set_jira_read_db(&conn, id, false).unwrap(), 1);
        assert_eq!(read_of(&conn, id), 0);
        // 없는 id → 0행 갱신, Err 아님
        assert_eq!(set_jira_read_db(&conn, 99999, true).unwrap(), 0);
    }

    // 8) jira_project_counts_db: 프로젝트별 unread/total 집계 + project_key 오름차순.
    #[test]
    fn jira_project_counts_aggregates_per_project() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_test(&conn);
        // APP: 3건(안읽음 2, 읽음 1) · AUTOPRJ: 2건(안읽음 1, 읽음 1)
        let ins = |uid: &str, proj: &str, read: i64| {
            conn.execute(
                "INSERT INTO JiraNotification \
                 (event_uid, issue_key, project_key, category, summary, detail, actor, event_at, fetched_at, read) \
                 VALUES(?1, 'K-1', ?2, 'comment', 's', 'd', 'a', '2026-07-21T10:00:00+09:00', '2026-07-21 10:00:00', ?3)",
                params![uid, proj, read],
            )
            .unwrap();
        };
        ins("a1", "APP", 0);
        ins("a2", "APP", 0);
        ins("a3", "APP", 1);
        ins("b1", "AUTOPRJ", 0);
        ins("b2", "AUTOPRJ", 1);

        let counts = jira_project_counts_db(&conn).unwrap();
        assert_eq!(counts.len(), 2);
        // project_key 오름차순: APP → AUTOPRJ
        assert_eq!(counts[0].project_key, "APP");
        assert_eq!((counts[0].unread, counts[0].total), (2, 3));
        assert_eq!(counts[1].project_key, "AUTOPRJ");
        assert_eq!((counts[1].unread, counts[1].total), (1, 2));
    }
}
