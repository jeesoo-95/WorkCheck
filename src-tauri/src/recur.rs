// 주기 계산 로직 (핵심)
// recur_type + recur_param(JSON) 으로 기한일 목록을 계산한다.
//
// ── 공통 필드 (모든 recur_type 공용) ───────────────────────
//   holiday: "none"|"skip"|"before"|"after"  주말·공휴일 처리 (기본 "none")
//            영업일 = 토·일 아님 + Holiday 테이블에 없음
//   start:   "YYYY-MM-DD"  시작일 겸 간격(interval) 기준일. 이 날 이전에는 발생하지 않음
//   until:   "YYYY-MM-DD"  이 날짜까지만 발생
//   count:   정수 ≥1       start 부터 N회만 발생 (start 없으면 무시)
//
// ── 타입별 필드 ───────────────────────────────────────────
//   daily:     {"interval":N}                               N일마다 (기본 1 = 매일)
//   weekly:    {"weekdays":[0~6], "interval":N}              0=일, N주마다
//   monthly:   {"mode":"days","days":[1~31],"lastDay":bool}  없는 날짜는 말일로 클램프
//              {"mode":"nth","nth":1~5|-1,"weekday":0~6}     n번째 X요일 (-1=마지막)
//   quarterly: {"monthOfQuarter":1~3,"day":1~31}  분기(1~3/4~6/7~9/10~12)의 n번째 달
//   yearly:    {"month":1~12,"day":n}
//   once:      {"date":"YYYY-MM-DD"}   1회성 — 지정한 기한일 하루만
//
// ── 레거시 → 신규 읽기 규칙 (쓰기는 항상 신규 형식) ───────
//   daily   {"weekdaysOnly":true} → holiday:"skip"
//   weekly  {"weekday":5}         → weekdays:[5]
//   monthly {"day":10}            → mode:"days", days:[10]
//   interval>1 인데 start 없음    → interval=1 (기준일 없이는 위상 판정 불가)

use chrono::{Datelike, Duration, NaiveDate};
use serde_json::Value;
use std::collections::HashSet;

const WEEKDAY_KO: [&str; 7] = ["일", "월", "화", "수", "목", "금", "토"];
/// monthly nth 라벨 (1~5). -1 은 "마지막".
const NTH_KO: [&str; 6] = ["", "첫째", "둘째", "셋째", "넷째", "다섯째"];
/// 공휴일 회피 이동 상한(일). 넘으면 해당 회차 포기.
const MOVE_LIMIT: i64 = 31;
/// holiday=before/after 는 창 밖 기준일이 창 안으로 이동할 수 있어 내부 생성 구간을 넓힌다(일).
/// 이동 상한과 같은 값이어야 경계에서 누락이 없다.
const PAD_DAYS: i64 = MOVE_LIMIT;
/// count 절단 시 start 부터 재생성하는 최대 일수
const COUNT_MAX_SPAN: i64 = 20000;

/// 주말·공휴일 처리 정책
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HolidayPolicy {
    Keep,   // none  — 그대로
    Skip,   // skip  — 발생 취소
    Before, // before— 직전 영업일로 당김
    After,  // after — 다음 영업일로 미룸
}

/// recur_param 의 공통 필드 스냅샷
struct RecurCfg {
    holiday: HolidayPolicy,
    start: Option<NaiveDate>,
    until: Option<NaiveDate>,
    count: Option<usize>,
    /// start 가 없으면 항상 1 (기획서 3.4)
    interval: i64,
}

/// recur_param 문자열을 JSON 값으로. 비거나 파싱 실패 시 빈 객체.
fn param_value(recur_param: &str) -> Value {
    serde_json::from_str(recur_param).unwrap_or_else(|_| Value::Object(Default::default()))
}

/// 해당 연·월의 말일(1~31)
fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first_next = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap();
    (first_next - Duration::days(1)).day()
}

/// day 를 해당 월 말일로 클램프한 날짜
fn clamped(year: i32, month: u32, day: u32) -> NaiveDate {
    let d = day.clamp(1, last_day_of_month(year, month));
    NaiveDate::from_ymd_opt(year, month, d).unwrap()
}

/// (y,m) 을 다음 달로 진행
fn next_month(y: &mut i32, m: &mut u32) {
    if *m == 12 {
        *y += 1;
        *m = 1;
    } else {
        *m += 1;
    }
}

/// 그 날이 속한 주의 시작(일요일)
fn week_start_sun(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_sunday() as i64)
}

/// "YYYY-MM-DD" 문자열 필드 파싱
fn parse_date(p: &Value, key: &str) -> Option<NaiveDate> {
    p.get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// 정수 배열 필드. 키가 없으면 None(= 레거시 폴백 대상), 있으면 빈 배열도 그대로 Some.
fn u32_list(p: &Value, key: &str) -> Option<Vec<u32>> {
    p.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_u64())
            .map(|n| n as u32)
            .collect()
    })
}

/// 공휴일 정책 파싱. holiday 키가 없으면 레거시 weekdaysOnly:true 를 skip 으로 읽는다.
fn parse_holiday(p: &Value) -> HolidayPolicy {
    match p.get("holiday").and_then(|v| v.as_str()) {
        Some("skip") => HolidayPolicy::Skip,
        Some("before") => HolidayPolicy::Before,
        Some("after") => HolidayPolicy::After,
        Some(_) => HolidayPolicy::Keep, // "none" 및 알 수 없는 값
        None => {
            if p.get("weekdaysOnly").and_then(|v| v.as_bool()).unwrap_or(false) {
                HolidayPolicy::Skip
            } else {
                HolidayPolicy::Keep
            }
        }
    }
}

/// 공통 필드 파싱
fn parse_cfg(p: &Value) -> RecurCfg {
    let start = parse_date(p, "start");
    let raw_interval = p.get("interval").and_then(|v| v.as_i64()).unwrap_or(1).max(1);
    RecurCfg {
        holiday: parse_holiday(p),
        start,
        until: parse_date(p, "until"),
        count: p
            .get("count")
            .and_then(|v| v.as_u64())
            .filter(|n| *n >= 1)
            .map(|n| n as usize),
        // 기준일 없이는 간격 위상을 정할 수 없으므로 1 로 취급
        interval: if start.is_some() { raw_interval } else { 1 },
    }
}

/// weekly 대상 요일(0=일). 레거시 weekday 단일값 폴백. 키가 있고 빈 배열이면 빈 결과.
fn weekly_weekdays(p: &Value) -> Vec<u32> {
    u32_list(p, "weekdays")
        .unwrap_or_else(|| vec![p.get("weekday").and_then(|v| v.as_u64()).unwrap_or(1) as u32])
        .into_iter()
        .filter(|w| *w <= 6)
        .collect()
}

/// monthly days 모드의 (일자 목록, 말일 포함 여부). 레거시 day 단일값 폴백.
fn monthly_days(p: &Value) -> (Vec<u32>, bool) {
    let days: Vec<u32> = u32_list(p, "days")
        .unwrap_or_else(|| vec![p.get("day").and_then(|v| v.as_u64()).unwrap_or(1) as u32])
        .into_iter()
        .map(|d| d.clamp(1, 31))
        .collect();
    let last_day = p.get("lastDay").and_then(|v| v.as_bool()).unwrap_or(false);
    (days, last_day)
}

/// 그 달의 n번째 X요일. nth=-1 은 마지막 X요일. 해당 요일이 n번 없으면 None.
fn nth_weekday_of_month(year: i32, month: u32, nth: i64, weekday: u32) -> Option<NaiveDate> {
    let wd = weekday.min(6);
    let last = last_day_of_month(year, month);
    if nth == -1 {
        let last_date = NaiveDate::from_ymd_opt(year, month, last).unwrap();
        let back = (last_date.weekday().num_days_from_sunday() + 7 - wd) % 7;
        return Some(last_date - Duration::days(back as i64));
    }
    if nth < 1 {
        return None;
    }
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let fwd = (wd + 7 - first.weekday().num_days_from_sunday()) % 7;
    let day = 1 + fwd + 7 * (nth as u32 - 1);
    if day > last {
        None
    } else {
        NaiveDate::from_ymd_opt(year, month, day)
    }
}

/// 영업일 여부 (토·일 아님 + 공휴일 아님)
fn is_business_day(d: NaiveDate, holidays: &HashSet<NaiveDate>) -> bool {
    let wd = d.weekday().num_days_from_sunday(); // 0=일 .. 6=토
    wd != 0 && wd != 6 && !holidays.contains(&d)
}

/// step(±1) 방향으로 가장 가까운 영업일. MOVE_LIMIT 일을 넘으면 None(회차 포기).
fn shift_to_business(d: NaiveDate, step: i64, holidays: &HashSet<NaiveDate>) -> Option<NaiveDate> {
    let mut cur = d;
    for _ in 0..=MOVE_LIMIT {
        if is_business_day(cur, holidays) {
            return Some(cur);
        }
        cur += Duration::days(step);
    }
    None
}

/// 기준 발생일 목록에 공휴일 정책을 적용한다.
fn apply_holiday_policy(
    dates: Vec<NaiveDate>,
    policy: HolidayPolicy,
    holidays: &HashSet<NaiveDate>,
) -> Vec<NaiveDate> {
    match policy {
        HolidayPolicy::Keep => dates,
        HolidayPolicy::Skip => dates
            .into_iter()
            .filter(|d| is_business_day(*d, holidays))
            .collect(),
        HolidayPolicy::Before => dates
            .into_iter()
            .filter_map(|d| shift_to_business(d, -1, holidays))
            .collect(),
        HolidayPolicy::After => dates
            .into_iter()
            .filter_map(|d| shift_to_business(d, 1, holidays))
            .collect(),
    }
}

/// 타입별 기준 발생일(공휴일 정책 적용 전) 을 [gen_from, gen_to] 에서 만든다.
/// start 가 있으면 그 이전은 생성하지 않는다.
fn base_occurrences(
    recur_type: &str,
    p: &Value,
    cfg: &RecurCfg,
    gen_from: NaiveDate,
    gen_to: NaiveDate,
) -> Vec<NaiveDate> {
    let mut out: Vec<NaiveDate> = Vec::new();
    // start 이전은 발생하지 않음
    let lo = match cfg.start {
        Some(s) if s > gen_from => s,
        _ => gen_from,
    };
    if lo > gen_to {
        return out;
    }

    match recur_type {
        "daily" => {
            let mut d = lo;
            while d <= gen_to {
                // interval 위상: (date - start) % interval == 0 (start 없으면 interval=1 이라 전부 통과)
                let ok = match cfg.start {
                    Some(s) if cfg.interval > 1 => (d - s).num_days() % cfg.interval == 0,
                    _ => true,
                };
                if ok {
                    out.push(d);
                }
                d += Duration::days(1);
            }
        }
        "weekly" => {
            let weekdays = weekly_weekdays(p);
            if weekdays.is_empty() {
                return out;
            }
            // 주 시작은 일요일 기준. start 가 속한 주가 0번째 주.
            let base_week = cfg.start.map(week_start_sun);
            let mut d = lo;
            while d <= gen_to {
                if weekdays.contains(&d.weekday().num_days_from_sunday()) {
                    let ok = match base_week {
                        Some(bw) if cfg.interval > 1 => {
                            ((week_start_sun(d) - bw).num_days() / 7) % cfg.interval == 0
                        }
                        _ => true,
                    };
                    if ok {
                        out.push(d);
                    }
                }
                d += Duration::days(1);
            }
        }
        "monthly" => {
            let nth_mode = p.get("mode").and_then(|v| v.as_str()) == Some("nth");
            let (days, last_flag) = monthly_days(p);
            let nth = p.get("nth").and_then(|v| v.as_i64()).unwrap_or(1);
            let nth_wd = p.get("weekday").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            if !nth_mode && days.is_empty() && !last_flag {
                return out; // 일자 미선택 → 발생 없음
            }
            let (mut y, mut m) = (lo.year(), lo.month());
            loop {
                let first = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
                if first > gen_to {
                    break;
                }
                let mut push_if_in = |date: NaiveDate| {
                    if date >= lo && date <= gen_to {
                        out.push(date);
                    }
                };
                if nth_mode {
                    if let Some(date) = nth_weekday_of_month(y, m, nth, nth_wd) {
                        push_if_in(date);
                    }
                } else {
                    for &day in &days {
                        push_if_in(clamped(y, m, day));
                    }
                    if last_flag {
                        push_if_in(clamped(y, m, 31));
                    }
                }
                next_month(&mut y, &mut m);
            }
        }
        "quarterly" => {
            let moq = p
                .get("monthOfQuarter")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32; // 1~3
            let day = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            for yy in lo.year()..=gen_to.year() {
                for qstart in [1u32, 4, 7, 10] {
                    let month = qstart + (moq.clamp(1, 3) - 1);
                    let date = clamped(yy, month, day);
                    if date >= lo && date <= gen_to {
                        out.push(date);
                    }
                }
            }
        }
        "yearly" => {
            let month = p.get("month").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let day = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            for yy in lo.year()..=gen_to.year() {
                let date = clamped(yy, month.clamp(1, 12), day);
                if date >= lo && date <= gen_to {
                    out.push(date);
                }
            }
        }
        "once" => {
            // 지정한 기한일이 구간 안이면 그 하루만. 파싱 실패 시 빈 결과.
            if let Some(date) = parse_date(p, "date") {
                if date >= lo && date <= gen_to {
                    out.push(date);
                }
            }
        }
        _ => {}
    }
    out
}

/// count 절단용 — start 부터 재생성해 앞 n개 유효 날짜를 구한다.
/// 창을 지수적으로 넓히며(366→…→COUNT_MAX_SPAN) n개를 채우면 멈춘다.
fn count_allowed_dates(
    recur_type: &str,
    p: &Value,
    cfg: &RecurCfg,
    start: NaiveDate,
    n: usize,
    holidays: &HashSet<NaiveDate>,
) -> Vec<NaiveDate> {
    let mut span = 366i64;
    loop {
        let capped = span.min(COUNT_MAX_SPAN);
        let win_to = start + Duration::days(capped);
        // 이동 상한만큼 우측 여유를 두고 만든 뒤 win_to 이하만 "완전한 prefix" 로 신뢰한다.
        let gen_to = win_to + Duration::days(MOVE_LIMIT);
        let mut v = base_occurrences(recur_type, p, cfg, start, gen_to);
        v = apply_holiday_policy(v, cfg.holiday, holidays);
        if let Some(u) = cfg.until {
            v.retain(|d| *d <= u);
        }
        v.sort();
        v.dedup();
        v.retain(|d| *d <= win_to);
        if v.len() >= n || capped >= COUNT_MAX_SPAN {
            v.truncate(n);
            return v;
        }
        span *= 4;
    }
}

/// [from, to] 구간에서 규칙이 만드는 기한일 목록(오름차순, 중복 제거)
pub fn occurrences_between(
    recur_type: &str,
    recur_param: &str,
    from: NaiveDate,
    to: NaiveDate,
    holidays: &HashSet<NaiveDate>,
) -> Vec<NaiveDate> {
    if from > to {
        return Vec::new();
    }
    let p = param_value(recur_param);
    let cfg = parse_cfg(&p);

    // 1. PAD 확장 — before/after 는 창 밖 기준일이 창 안으로 이동할 수 있다
    let pad = match cfg.holiday {
        HolidayPolicy::Before | HolidayPolicy::After => PAD_DAYS,
        _ => 0,
    };
    let gen_from = from.checked_sub_signed(Duration::days(pad)).unwrap_or(from);
    let gen_to = to.checked_add_signed(Duration::days(pad)).unwrap_or(to);

    // 2. 기준 발생일 생성
    let mut out = base_occurrences(recur_type, &p, &cfg, gen_from, gen_to);

    // 3. 공휴일 정책 적용
    out = apply_holiday_policy(out, cfg.holiday, holidays);

    // 4. until 필터 → count 절단
    if let Some(u) = cfg.until {
        out.retain(|d| *d <= u);
    }
    if let (Some(n), Some(s)) = (cfg.count, cfg.start) {
        let allowed: HashSet<NaiveDate> = count_allowed_dates(recur_type, &p, &cfg, s, n, holidays)
            .into_iter()
            .collect();
        out.retain(|d| allowed.contains(d));
    }

    // 5. 정렬 + 중복 제거 → [from, to] 최종 필터
    //    (before/after 이동으로 서로 다른 회차가 같은 날로 몰릴 수 있어 dedup 필수)
    out.sort();
    out.dedup();
    out.retain(|d| *d >= from && *d <= to);
    out
}

// ── 라벨 / 요약 ──────────────────────────────────────────

/// 규칙 문구 조각 (머리말, 세부). 라벨은 " · " 로, 요약은 " " 로 이어붙인다.
fn rule_parts(recur_type: &str, p: &Value, cfg: &RecurCfg) -> (String, Option<String>) {
    match recur_type {
        "daily" => {
            if cfg.interval > 1 {
                (format!("{}일마다", cfg.interval), None)
            } else {
                ("매일".to_string(), None)
            }
        }
        "weekly" => {
            let head = if cfg.interval > 1 {
                format!("{}주마다", cfg.interval)
            } else {
                "매주".to_string()
            };
            let names: Vec<&str> = weekly_weekdays(p)
                .iter()
                .filter_map(|w| WEEKDAY_KO.get(*w as usize).copied())
                .collect();
            let detail = if names.is_empty() {
                None
            } else {
                Some(names.join("·"))
            };
            (head, detail)
        }
        "monthly" => {
            let head = "매월".to_string();
            if p.get("mode").and_then(|v| v.as_str()) == Some("nth") {
                let nth = p.get("nth").and_then(|v| v.as_i64()).unwrap_or(1);
                let wd = p.get("weekday").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                let wd_ko = WEEKDAY_KO.get(wd).copied().unwrap_or("?");
                let pos = if nth == -1 {
                    "마지막".to_string()
                } else {
                    match NTH_KO.get(nth.max(0) as usize).copied().filter(|s| !s.is_empty()) {
                        Some(s) => s.to_string(),
                        None => format!("{}번째", nth),
                    }
                };
                (head, Some(format!("{} {}", pos, wd_ko)))
            } else {
                let (days, last_flag) = monthly_days(p);
                let mut parts: Vec<String> = Vec::new();
                if !days.is_empty() {
                    parts.push(format!(
                        "{}일",
                        days.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", ")
                    ));
                }
                if last_flag {
                    parts.push("말일".to_string());
                }
                let detail = if parts.is_empty() { None } else { Some(parts.join(", ")) };
                (head, detail)
            }
        }
        "quarterly" => {
            let moq = p.get("monthOfQuarter").and_then(|v| v.as_u64()).unwrap_or(1);
            let d = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1);
            ("매분기".to_string(), Some(format!("{}번째 달 {}일", moq, d)))
        }
        "yearly" => {
            let m = p.get("month").and_then(|v| v.as_u64()).unwrap_or(1);
            let d = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1);
            ("매년".to_string(), Some(format!("{}/{}", m, d)))
        }
        "once" => match parse_date(p, "date") {
            Some(date) => (
                "1회".to_string(),
                Some(format!("{}/{}", date.month(), date.day())),
            ),
            None => ("1회".to_string(), None),
        },
        _ => (recur_type.to_string(), None),
    }
}

/// 표시용 규칙 라벨 (목록·배지용 짧은 문구)
pub fn rule_label(recur_type: &str, recur_param: &str) -> String {
    let p = param_value(recur_param);
    let cfg = parse_cfg(&p);
    let (head, detail) = rule_parts(recur_type, &p, &cfg);
    let mut s = head;
    if let Some(d) = detail {
        s.push_str(" · ");
        s.push_str(&d);
    }
    // 레거시 표기 유지: 매일 + 공휴일 건너뜀 = "평일만"
    // (그 외 주기의 공휴일 정책은 라벨을 길게 만들지 않고 rule_summary 에서만 안내한다)
    if recur_type == "daily" && cfg.holiday == HolidayPolicy::Skip {
        s.push_str(" · 평일만");
    }
    s
}

/// 미리보기용 요약 문장 (라벨 + 공휴일 정책 + 종료 조건)
/// 예) "매주 화·목 · 공휴일이면 다음 영업일 · 2026-12-31까지"
pub fn rule_summary(recur_type: &str, recur_param: &str) -> String {
    let p = param_value(recur_param);
    let cfg = parse_cfg(&p);
    let (head, detail) = rule_parts(recur_type, &p, &cfg);

    let mut parts: Vec<String> = Vec::new();
    parts.push(match detail {
        Some(d) => format!("{} {}", head, d),
        None => head,
    });
    match cfg.holiday {
        HolidayPolicy::Keep => {}
        HolidayPolicy::Skip => parts.push("공휴일이면 건너뜀".to_string()),
        HolidayPolicy::Before => parts.push("공휴일이면 직전 영업일".to_string()),
        HolidayPolicy::After => parts.push("공휴일이면 다음 영업일".to_string()),
    }
    if let Some(u) = cfg.until {
        parts.push(format!("{}까지", u));
    }
    if let Some(n) = cfg.count {
        if cfg.start.is_some() {
            parts.push(format!("{}회", n));
        }
    }
    parts.join(" · ")
}

/// 날짜의 한글 요일 1글자 (리포트 등 표시용). 위 WEEKDAY_KO 를 재사용한다.
pub fn weekday_ko(date: NaiveDate) -> &'static str {
    WEEKDAY_KO[date.weekday().num_days_from_sunday() as usize]
}

/// 다가오는 업무 배지용 짧은 라벨
pub fn short_recur(recur_type: &str) -> &'static str {
    match recur_type {
        "weekly" => "매주",
        "monthly" => "매월",
        "quarterly" => "분기",
        "yearly" => "연간",
        "once" => "1회",
        _ => "예정",
    }
}

// ── 단위 테스트 ──────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn empty() -> HashSet<NaiveDate> {
        HashSet::new()
    }

    // 1. 매월 31일 → 2월은 말일(28)로 클램프
    #[test]
    fn monthly_clamp_february() {
        let occ = occurrences_between("monthly", r#"{"day":31}"#, d(2026, 2, 1), d(2026, 2, 28), &empty());
        assert_eq!(occ, vec![d(2026, 2, 28)]);
    }

    // 2. 매월 31일 → 4월은 30일로 클램프
    #[test]
    fn monthly_clamp_april() {
        let occ = occurrences_between("monthly", r#"{"day":31}"#, d(2026, 4, 1), d(2026, 4, 30), &empty());
        assert_eq!(occ, vec![d(2026, 4, 30)]);
    }

    // 3. 매일 평일만 → 주말 제외
    #[test]
    fn daily_weekdays_only_skips_weekend() {
        // 2026-07-20(월) ~ 07-26(일). 토(25)/일(26) 제외 → 20~24 (5일)
        let occ = occurrences_between("daily", r#"{"weekdaysOnly":true}"#, d(2026, 7, 20), d(2026, 7, 26), &empty());
        assert_eq!(occ, vec![d(2026,7,20), d(2026,7,21), d(2026,7,22), d(2026,7,23), d(2026,7,24)]);
    }

    // 4. 매일 평일만 → 공휴일도 제외
    #[test]
    fn daily_weekdays_only_skips_holiday() {
        let mut h = empty();
        h.insert(d(2026, 8, 17)); // 광복절 대체(월)
        let occ = occurrences_between("daily", r#"{"weekdaysOnly":true}"#, d(2026, 8, 17), d(2026, 8, 17), &h);
        assert!(occ.is_empty());
    }

    // 5. 매주 금요일(weekday=5)
    #[test]
    fn weekly_friday() {
        let occ = occurrences_between("weekly", r#"{"weekday":5}"#, d(2026, 7, 1), d(2026, 7, 31), &empty());
        // 2026년 7월 금요일: 3,10,17,24,31
        assert_eq!(occ, vec![d(2026,7,3), d(2026,7,10), d(2026,7,17), d(2026,7,24), d(2026,7,31)]);
    }

    // 6. 매분기 3번째 달 31일 → 각 분기 말달(3/6/9/12) 말일로 클램프
    #[test]
    fn quarterly_last_month_end() {
        let occ = occurrences_between("quarterly", r#"{"monthOfQuarter":3,"day":31}"#, d(2026, 1, 1), d(2026, 12, 31), &empty());
        assert_eq!(occ, vec![d(2026,3,31), d(2026,6,30), d(2026,9,30), d(2026,12,31)]);
    }

    // 7. 연 1회 윤년 2/29
    #[test]
    fn yearly_leap_day_clamp() {
        // 2026(평년) 2/29 → 2/28 클램프
        let occ = occurrences_between("yearly", r#"{"month":2,"day":29}"#, d(2026, 1, 1), d(2026, 12, 31), &empty());
        assert_eq!(occ, vec![d(2026, 2, 28)]);
    }

    // 8. 라벨 포맷
    #[test]
    fn labels() {
        assert_eq!(rule_label("daily", r#"{"weekdaysOnly":true}"#), "매일 · 평일만");
        assert_eq!(rule_label("weekly", r#"{"weekday":5}"#), "매주 · 금");
        assert_eq!(rule_label("monthly", r#"{"day":1}"#), "매월 · 1일");
    }

    // 9. 1회성 — 기한일이 구간 안이면 그 하루만
    #[test]
    fn once_within_range() {
        let occ = occurrences_between("once", r#"{"date":"2026-07-25"}"#, d(2026, 7, 1), d(2026, 7, 31), &empty());
        assert_eq!(occ, vec![d(2026, 7, 25)]);
    }

    // 10. 1회성 — 기한일이 구간 밖이면 빈 결과
    #[test]
    fn once_outside_range() {
        let occ = occurrences_between("once", r#"{"date":"2026-08-25"}"#, d(2026, 7, 1), d(2026, 7, 31), &empty());
        assert!(occ.is_empty());
    }

    // 11. 1회성 — date 파싱 실패 시 빈 결과 + 라벨은 "1회"
    #[test]
    fn once_bad_param() {
        let occ = occurrences_between("once", r#"{"date":"not-a-date"}"#, d(2026, 7, 1), d(2026, 7, 31), &empty());
        assert!(occ.is_empty());
        let occ2 = occurrences_between("once", r#"{}"#, d(2026, 7, 1), d(2026, 7, 31), &empty());
        assert!(occ2.is_empty());
        assert_eq!(rule_label("once", r#"{"date":"2026-07-25"}"#), "1회 · 7/25");
        assert_eq!(rule_label("once", r#"{}"#), "1회");
    }

    // ── 주기 고도화 (기획서 v1) ──────────────────────────

    // 12. 매주 복수 요일 (화·목) 한 달치
    #[test]
    fn weekly_multi_weekdays() {
        let occ = occurrences_between(
            "weekly",
            r#"{"weekdays":[2,4]}"#,
            d(2026, 8, 1),
            d(2026, 8, 31),
            &empty(),
        );
        // 2026-08 화: 4,11,18,25 / 목: 6,13,20,27
        assert_eq!(
            occ,
            vec![
                d(2026, 8, 4), d(2026, 8, 6), d(2026, 8, 11), d(2026, 8, 13),
                d(2026, 8, 18), d(2026, 8, 20), d(2026, 8, 25), d(2026, 8, 27),
            ]
        );
    }

    // 13. 격주(interval=2) 위상 — start 가 속한 주가 발생 주
    #[test]
    fn weekly_biweekly_phase() {
        // start=2026-08-03(월) → 그 주(일요일 시작 8/2)가 0번째 주 → 8/2, 8/16, 8/30 주에 발생
        let occ = occurrences_between(
            "weekly",
            r#"{"weekdays":[2,4],"interval":2,"start":"2026-08-03"}"#,
            d(2026, 8, 1),
            d(2026, 8, 31),
            &empty(),
        );
        assert_eq!(occ, vec![d(2026, 8, 4), d(2026, 8, 6), d(2026, 8, 18), d(2026, 8, 20)]);
    }

    // 14. N일마다 (interval=3)
    #[test]
    fn daily_interval_three() {
        let occ = occurrences_between(
            "daily",
            r#"{"interval":3,"start":"2026-08-03"}"#,
            d(2026, 8, 1),
            d(2026, 8, 15),
            &empty(),
        );
        assert_eq!(
            occ,
            vec![d(2026, 8, 3), d(2026, 8, 6), d(2026, 8, 9), d(2026, 8, 12), d(2026, 8, 15)]
        );
    }

    // 15. 매월 복수 일자 [10,25] + 말일
    #[test]
    fn monthly_days_with_last_day() {
        let occ = occurrences_between(
            "monthly",
            r#"{"mode":"days","days":[10,25],"lastDay":true}"#,
            d(2026, 2, 1),
            d(2026, 3, 31),
            &empty(),
        );
        assert_eq!(
            occ,
            vec![
                d(2026, 2, 10), d(2026, 2, 25), d(2026, 2, 28),
                d(2026, 3, 10), d(2026, 3, 25), d(2026, 3, 31),
            ]
        );
    }

    // 16. 매월 마지막 주 금요일 (nth=-1)
    #[test]
    fn monthly_nth_last_friday() {
        let occ = occurrences_between(
            "monthly",
            r#"{"mode":"nth","nth":-1,"weekday":5}"#,
            d(2026, 1, 1),
            d(2026, 3, 31),
            &empty(),
        );
        // 2026-01 마지막 금 30, 02 마지막 금 27, 03 마지막 금 27
        assert_eq!(occ, vec![d(2026, 1, 30), d(2026, 2, 27), d(2026, 3, 27)]);
    }

    // 17. 매월 첫째 주 월요일 (nth=1)
    #[test]
    fn monthly_nth_first_monday() {
        let occ = occurrences_between(
            "monthly",
            r#"{"mode":"nth","nth":1,"weekday":1}"#,
            d(2026, 8, 1),
            d(2026, 9, 30),
            &empty(),
        );
        assert_eq!(occ, vec![d(2026, 8, 3), d(2026, 9, 7)]);
    }

    // 18. nth=5 — 다섯째 월요일이 있는 달만 발생
    #[test]
    fn monthly_nth_fifth_may_be_absent() {
        // 2026-08 월: 3,10,17,24,31 → 다섯째 있음
        let aug = occurrences_between(
            "monthly", r#"{"mode":"nth","nth":5,"weekday":1}"#,
            d(2026, 8, 1), d(2026, 8, 31), &empty());
        assert_eq!(aug, vec![d(2026, 8, 31)]);
        // 2026-09 월: 7,14,21,28 → 다섯째 없음
        let sep = occurrences_between(
            "monthly", r#"{"mode":"nth","nth":5,"weekday":1}"#,
            d(2026, 9, 1), d(2026, 9, 30), &empty());
        assert!(sep.is_empty());
    }

    // 19. holiday=skip — 공휴일에 걸린 회차 제거
    #[test]
    fn holiday_skip_drops_occurrence() {
        let mut h = empty();
        h.insert(d(2026, 8, 14)); // 금요일 공휴일
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"holiday":"skip"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &h);
        assert_eq!(occ, vec![d(2026, 8, 7), d(2026, 8, 21), d(2026, 8, 28)]);
    }

    // 20. holiday=before — 직전 영업일로 당김
    #[test]
    fn holiday_before_moves_backward() {
        let mut h = empty();
        h.insert(d(2026, 8, 14)); // 금
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"holiday":"before"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &h);
        // 8/14 → 8/13(목)
        assert_eq!(occ, vec![d(2026, 8, 7), d(2026, 8, 13), d(2026, 8, 21), d(2026, 8, 28)]);
    }

    // 21. holiday=after — 연휴(공휴일+주말)로 2일 이상 밀리는 케이스
    #[test]
    fn holiday_after_moves_over_long_weekend() {
        let mut h = empty();
        h.insert(d(2026, 8, 14)); // 금 공휴일 → 15(토) 16(일) 17(월,공휴일) → 18(화)
        h.insert(d(2026, 8, 17));
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"holiday":"after"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &h);
        assert_eq!(occ, vec![d(2026, 8, 7), d(2026, 8, 18), d(2026, 8, 21), d(2026, 8, 28)]);
    }

    // 22. PAD — before 로 창 밖(8/1 토) 회차가 창 안(7/31 금)으로 들어온다
    #[test]
    fn holiday_before_pulls_outside_date_into_window() {
        let occ = occurrences_between(
            "monthly", r#"{"mode":"days","days":[1],"holiday":"before"}"#,
            d(2026, 7, 1), d(2026, 7, 31), &empty());
        // 7/1(수)은 영업일 그대로, 8/1(토)은 7/31(금)로 당겨져 창 안으로 들어옴
        assert_eq!(occ, vec![d(2026, 7, 1), d(2026, 7, 31)]);
    }

    // 23. PAD — after 로 창 밖(7/31 공휴일) 회차가 창 안(8/3 월)으로 들어온다
    #[test]
    fn holiday_after_pushes_outside_date_into_window() {
        let mut h = empty();
        h.insert(d(2026, 7, 31)); // 금 공휴일 → 8/1(토) 8/2(일) → 8/3(월)
        let occ = occurrences_between(
            "monthly", r#"{"mode":"days","days":[31],"holiday":"after"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &h);
        assert_eq!(occ, vec![d(2026, 8, 3), d(2026, 8, 31)]);
    }

    // 24. until 종료
    #[test]
    fn until_stops_generation() {
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"until":"2026-08-14"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &empty());
        assert_eq!(occ, vec![d(2026, 8, 7), d(2026, 8, 14)]);
    }

    // 25. count 종료 — start 부터 N회
    #[test]
    fn count_stops_generation() {
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"start":"2026-08-03","count":2}"#,
            d(2026, 8, 1), d(2026, 12, 31), &empty());
        assert_eq!(occ, vec![d(2026, 8, 7), d(2026, 8, 14)]);
    }

    // 26. count 는 start 없으면 무시된다
    #[test]
    fn count_without_start_is_ignored() {
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"count":2}"#,
            d(2026, 8, 1), d(2026, 8, 31), &empty());
        assert_eq!(occ, vec![d(2026,8,7), d(2026,8,14), d(2026,8,21), d(2026,8,28)]);
    }

    // 27. until + count 동시 — 둘 다 만족하는 범위
    #[test]
    fn until_and_count_together() {
        let occ = occurrences_between(
            "weekly",
            r#"{"weekdays":[5],"start":"2026-08-03","count":3,"until":"2026-08-14"}"#,
            d(2026, 8, 1), d(2026, 12, 31), &empty());
        // count 는 3회지만 until 이 8/14 라 2회만 남는다
        assert_eq!(occ, vec![d(2026, 8, 7), d(2026, 8, 14)]);
    }

    // 28. start 이전에는 발생하지 않는다
    #[test]
    fn start_excludes_earlier_dates() {
        let occ = occurrences_between(
            "weekly", r#"{"weekdays":[5],"start":"2026-08-10"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &empty());
        assert_eq!(occ, vec![d(2026, 8, 14), d(2026, 8, 21), d(2026, 8, 28)]);
    }

    // 29. 레거시 파라미터 3종이 신규 형식과 동일 결과를 낸다
    #[test]
    fn legacy_params_match_new_format() {
        let (f, t) = (d(2026, 8, 1), d(2026, 10, 31));
        let mut h = empty();
        h.insert(d(2026, 8, 17));

        // daily: weekdaysOnly:true ≡ holiday:"skip"
        assert_eq!(
            occurrences_between("daily", r#"{"weekdaysOnly":true}"#, f, t, &h),
            occurrences_between("daily", r#"{"holiday":"skip"}"#, f, t, &h)
        );
        // weekly: weekday:5 ≡ weekdays:[5]
        assert_eq!(
            occurrences_between("weekly", r#"{"weekday":5}"#, f, t, &h),
            occurrences_between("weekly", r#"{"weekdays":[5]}"#, f, t, &h)
        );
        // monthly: day:10 ≡ mode:"days", days:[10]
        assert_eq!(
            occurrences_between("monthly", r#"{"day":10}"#, f, t, &h),
            occurrences_between("monthly", r#"{"mode":"days","days":[10]}"#, f, t, &h)
        );
    }

    // 30. interval>1 인데 start 가 없으면 interval=1 로 취급
    #[test]
    fn interval_without_start_falls_back_to_one() {
        let occ = occurrences_between("daily", r#"{"interval":3}"#, d(2026, 8, 1), d(2026, 8, 5), &empty());
        assert_eq!(
            occ,
            vec![d(2026,8,1), d(2026,8,2), d(2026,8,3), d(2026,8,4), d(2026,8,5)]
        );
        assert_eq!(rule_label("daily", r#"{"interval":3}"#), "매일");
    }

    // 31. 요일/일자 미선택이면 발생 없음 (미리보기 error 조건)
    #[test]
    fn empty_selection_yields_nothing() {
        let occ = occurrences_between("weekly", r#"{"weekdays":[]}"#, d(2026, 8, 1), d(2026, 8, 31), &empty());
        assert!(occ.is_empty());
        let occ2 = occurrences_between(
            "monthly", r#"{"mode":"days","days":[],"lastDay":false}"#,
            d(2026, 8, 1), d(2026, 8, 31), &empty());
        assert!(occ2.is_empty());
    }

    // 32. before/after 이동으로 같은 날에 몰린 회차는 dedup 된다
    #[test]
    fn shifted_duplicates_are_deduped() {
        let mut h = empty();
        h.insert(d(2026, 8, 11)); // 화 공휴일 → after → 8/12(수)
        let occ = occurrences_between(
            "monthly", r#"{"mode":"days","days":[11,12],"holiday":"after"}"#,
            d(2026, 8, 1), d(2026, 8, 31), &h);
        assert_eq!(occ, vec![d(2026, 8, 12)]);
    }

    // 33. 확장 라벨 포맷 (기획서 5장)
    #[test]
    fn extended_labels() {
        assert_eq!(rule_label("weekly", r#"{"weekdays":[2,4]}"#), "매주 · 화·목");
        assert_eq!(
            rule_label("weekly", r#"{"weekdays":[1],"interval":2,"start":"2026-08-03"}"#),
            "2주마다 · 월"
        );
        assert_eq!(
            rule_label("daily", r#"{"interval":3,"start":"2026-08-03"}"#),
            "3일마다"
        );
        assert_eq!(
            rule_label("monthly", r#"{"mode":"nth","nth":-1,"weekday":5}"#),
            "매월 · 마지막 금"
        );
        assert_eq!(
            rule_label("monthly", r#"{"mode":"nth","nth":1,"weekday":1}"#),
            "매월 · 첫째 월"
        );
        assert_eq!(
            rule_label("monthly", r#"{"mode":"days","days":[10,25],"lastDay":true}"#),
            "매월 · 10, 25일, 말일"
        );
    }

    // 34. rule_summary — 라벨 + 공휴일 정책 + 종료 조건
    #[test]
    fn summaries() {
        assert_eq!(
            rule_summary("weekly", r#"{"weekdays":[2,4],"holiday":"after","until":"2026-12-31"}"#),
            "매주 화·목 · 공휴일이면 다음 영업일 · 2026-12-31까지"
        );
        assert_eq!(rule_summary("daily", r#"{}"#), "매일");
        assert_eq!(
            rule_summary("daily", r#"{"weekdaysOnly":true}"#),
            "매일 · 공휴일이면 건너뜀"
        );
        assert_eq!(
            rule_summary("monthly", r#"{"mode":"nth","nth":-1,"weekday":5,"holiday":"before"}"#),
            "매월 마지막 금 · 공휴일이면 직전 영업일"
        );
        assert_eq!(
            rule_summary("weekly", r#"{"weekdays":[5],"start":"2026-08-03","count":5}"#),
            "매주 금 · 5회"
        );
        // count 는 start 없으면 요약에도 표기하지 않는다
        assert_eq!(rule_summary("weekly", r#"{"weekdays":[5],"count":5}"#), "매주 금");
    }
}
