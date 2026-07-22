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
    pub notify_time: Option<String>,  // null=개별 알림 없음, "HH:MM"
    pub remind_before: Option<i64>,   // null=리마인드 없음, 1~30 (기한 N일 전 예고)
    pub priority: i64,                // 0=높음, 1=보통(기본), 2=낮음 (숫자 작을수록 우선)
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

/// CheckLog 한 회차의 상태 스냅샷 (내부 판정용, 프론트 전송 X).
/// status: 'done'(완료) | 'skip'(건너뜀). memo: 완료 메모(선택).
#[derive(Debug, Clone)]
pub struct CheckInfo {
    pub status: String,
    pub memo: Option<String>,
}

/// list_tasks 응답 항목: Task 전체 필드 + doneOnce(1회성 완료 여부).
/// DB 로딩용 Task 는 그대로 두고, flatten 으로 응답에만 doneOnce 를 덧붙인다.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListItem {
    #[serde(flatten)]
    pub task: Task,
    /// recur_type=="once" 이고 지정 기한일에 체크가 있으면 true. 그 외 주기는 항상 false.
    pub done_once: bool,
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
    pub notify_time: Option<String>, // "HH:MM" 또는 null
    pub remind_before: Option<i64>,  // 1~30 또는 null
    pub priority: Option<i64>,       // 0=높음|1=보통|2=낮음. 없으면 add_task 에서 1(보통)
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
    pub status: String,                 // "none"(미체크) | "done"(완료) | "skip"(건너뜀)
    pub checked: bool,                  // 하위호환: status=="done" 와 동일
    pub check_memo: Option<String>,     // 완료 메모(회차별, Task.memo 와 별개)
    pub days_late: i64,                 // 밀림 D+n (오늘/다가오는건 0)
    pub upcoming_label: Option<String>, // "분기 · 7/31 예정" 등
    pub priority: i64,                  // 0=높음|1=보통|2=낮음 (오늘 탭 우선순위 배지용)
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

/// 통계 탭 히트맵 셀.
/// total/done 은 skip 회차를 제외한 값(수행률 분모와 동일). skipped 는 그날 skip 회차 수.
#[derive(Debug, Serialize)]
pub struct HeatCell {
    pub date: String,
    pub done: i64,
    pub total: i64,
    pub skipped: i64,
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

// ── M4: Jira 알림 ─────────────────────────────────────────

/// 연결 테스트 응답 (jira_test_connection). /rest/api/3/myself 에서 파싱.
/// 프론트가 성공 시 account_id 를 jira_account_id Setting 에 저장한다.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraUser {
    pub account_id: String,
    pub display_name: String,
}

/// 폴링 결과 (jira_poll_now). new_count=신규 삽입 알림 수, error=실패 사유(성공 시 None).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PollResult {
    pub new_count: i64,
    pub error: Option<String>,
}

/// Jira 알림 피드 한 행 (get_jira_notifications). JiraNotification 테이블과 1:1.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraNotificationRow {
    pub id: i64,
    pub event_uid: String,
    pub issue_key: String,
    pub project_key: String,
    pub category: String, // created|status|assignee|field|comment|mention
    pub summary: String,
    pub detail: String,
    pub actor: String,
    pub event_at: String,
    pub fetched_at: String,
    pub read: i64,
}
