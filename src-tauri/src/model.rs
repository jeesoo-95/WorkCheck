// 도메인 모델 및 프론트엔드 전송용 구조체 (기획서 4장 스키마 기준)
// 프론트-백엔드 JSON 계약은 모두 camelCase 로 통일한다.

use serde::{Deserialize, Serialize};

// ── DB 엔티티 ──────────────────────────────────────────────

/// 관리 대상 반복 업무
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub name: String,
    pub memo: Option<String>,
    pub links: Option<String>,        // JSON 배열 문자열 [{title,url}]
    pub recur_type: String,           // daily/weekly/monthly/quarterly/yearly
    pub recur_param: Option<String>,  // JSON 파라미터
    pub active: i64,
    pub sort_order: Option<i64>,
    pub created_at: Option<String>,
}

/// 설정 키-값
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
}

/// 공휴일
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holiday {
    pub date: String, // 'YYYY-MM-DD'
    pub name: String,
}

// ── 입력 DTO ──────────────────────────────────────────────

/// 업무 추가/수정 입력. id 가 있으면 수정에 사용.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDto {
    pub id: Option<i64>,
    pub name: String,
    pub memo: Option<String>,
    pub links: Option<String>,
    pub recur_type: String,
    pub recur_param: Option<String>,
    pub sort_order: Option<i64>,
}

// ── 조회 응답 (규칙에서 계산된 회차) ──────────────────────

/// 특정 기한일의 1회 발생(회차). DB에 저장하지 않고 규칙에서 계산.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOccurrence {
    pub task_id: i64,
    pub name: String,
    pub memo: Option<String>,
    pub links: Option<String>,
    pub recur_type: String,
    pub recur_param: Option<String>,
    pub due_date: String,
    pub rule_label: String,             // "매주 · 금" 등 표시용 라벨
    pub checked: bool,
    pub days_late: i64,                 // 밀림 D+n (오늘/다가오는건 0)
    pub upcoming_label: Option<String>, // "분기 · 7/31 예정" 등
}

/// 오늘 탭 응답
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodayView {
    pub date: String,
    pub overdue: Vec<TaskOccurrence>,
    pub today: Vec<TaskOccurrence>,
    pub upcoming: Vec<TaskOccurrence>,
    pub week_rate: f64,
}

/// 통계 탭 히트맵 셀
#[derive(Debug, Serialize)]
pub struct HeatCell {
    pub date: String,
    pub done: i64,
    pub total: i64,
}

/// 통계 탭 응답
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub streak_days: i64,
    pub month_rate: f64,
    pub week_rate: f64,
    pub quarter_rate: f64,
    pub heatmap: Vec<HeatCell>,
}
