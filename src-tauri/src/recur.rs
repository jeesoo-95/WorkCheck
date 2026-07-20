// 주기 계산 로직 (핵심)
// recur_type + recur_param(JSON) 으로 기한일 목록을 계산한다.
//   daily:     {"weekdaysOnly": bool}  평일만이면 토/일 + Holiday 제외
//   weekly:    {"weekday": 0~6}        0=일
//   monthly:   {"day": 1~31}           없는 날짜는 말일로 클램프
//   quarterly: {"monthOfQuarter":1~3,"day":1~31} 분기(1~3/4~6/7~9/10~12)의 n번째 달
//   yearly:    {"month":1~12,"day":n}

use chrono::{Datelike, Duration, NaiveDate};
use serde_json::Value;
use std::collections::HashSet;

const WEEKDAY_KO: [&str; 7] = ["일", "월", "화", "수", "목", "금", "토"];

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

/// [from, to] 구간에서 규칙이 만드는 기한일 목록(오름차순, 중복 제거)
pub fn occurrences_between(
    recur_type: &str,
    recur_param: &str,
    from: NaiveDate,
    to: NaiveDate,
    holidays: &HashSet<NaiveDate>,
) -> Vec<NaiveDate> {
    let mut out: Vec<NaiveDate> = Vec::new();
    if from > to {
        return out;
    }
    let p = param_value(recur_param);

    match recur_type {
        "daily" => {
            let weekdays_only = p
                .get("weekdaysOnly")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut d = from;
            while d <= to {
                let wd = d.weekday().num_days_from_sunday(); // 0=일 .. 6=토
                let is_weekend = wd == 0 || wd == 6;
                if !weekdays_only || (!is_weekend && !holidays.contains(&d)) {
                    out.push(d);
                }
                d += Duration::days(1);
            }
        }
        "weekly" => {
            let target = p.get("weekday").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let mut d = from;
            while d <= to {
                if d.weekday().num_days_from_sunday() == target {
                    out.push(d);
                }
                d += Duration::days(1);
            }
        }
        "monthly" => {
            let day = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let (mut y, mut m) = (from.year(), from.month());
            loop {
                let first = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
                if first > to {
                    break;
                }
                let date = clamped(y, m, day);
                if date >= from && date <= to {
                    out.push(date);
                }
                if m == 12 { y += 1; m = 1; } else { m += 1; }
            }
        }
        "quarterly" => {
            let moq = p
                .get("monthOfQuarter")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32; // 1~3
            let day = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            for yy in from.year()..=to.year() {
                for qstart in [1u32, 4, 7, 10] {
                    let month = qstart + (moq.clamp(1, 3) - 1);
                    let date = clamped(yy, month, day);
                    if date >= from && date <= to {
                        out.push(date);
                    }
                }
            }
        }
        "yearly" => {
            let month = p.get("month").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let day = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            for yy in from.year()..=to.year() {
                let date = clamped(yy, month.clamp(1, 12), day);
                if date >= from && date <= to {
                    out.push(date);
                }
            }
        }
        _ => {}
    }

    out.sort();
    out.dedup();
    out
}

/// 표시용 규칙 라벨
pub fn rule_label(recur_type: &str, recur_param: &str) -> String {
    let p = param_value(recur_param);
    match recur_type {
        "daily" => {
            let wo = p
                .get("weekdaysOnly")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if wo { "매일 · 평일만".into() } else { "매일".into() }
        }
        "weekly" => {
            let w = p.get("weekday").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
            format!("매주 · {}", WEEKDAY_KO.get(w).unwrap_or(&"?"))
        }
        "monthly" => {
            let d = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1);
            format!("매월 · {}일", d)
        }
        "quarterly" => {
            let moq = p.get("monthOfQuarter").and_then(|v| v.as_u64()).unwrap_or(1);
            let d = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1);
            format!("매분기 · {}번째 달 {}일", moq, d)
        }
        "yearly" => {
            let m = p.get("month").and_then(|v| v.as_u64()).unwrap_or(1);
            let d = p.get("day").and_then(|v| v.as_u64()).unwrap_or(1);
            format!("매년 · {}/{}", m, d)
        }
        _ => recur_type.to_string(),
    }
}

/// 다가오는 업무 배지용 짧은 라벨
pub fn short_recur(recur_type: &str) -> &'static str {
    match recur_type {
        "weekly" => "매주",
        "monthly" => "매월",
        "quarterly" => "분기",
        "yearly" => "연간",
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
}
