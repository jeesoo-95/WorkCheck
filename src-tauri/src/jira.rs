// Jira 알림 (M4) — REST 폴링 · 이벤트 분류 · 저장
//
// 백그라운드 폴링은 notify.rs 30초 틱에서 poll_tick(&app) 을 호출한다(별도 스레드 없음).
// HTTP 는 reqwest blocking(타임아웃 15s). 네트워크 호출과 DB 락을 분리해,
// 느린 네트워크 구간에는 DB 락을 잡지 않는다(기존 short-lock 패턴 준수).
//
// 이벤트 분류(기획서 3장):
//   created  : APP 이슈 생성 (creator, created >= t)
//   status   : changelog history 에 status 필드 변경
//   assignee : changelog history 에 assignee 필드 변경
//   field    : 그 외 changelog history (변경 필드명 나열, history 1건당 1알림)
//   comment  : APP 이슈 새 댓글
//   mention  : 모든 프로젝트 새 댓글 본문(ADF)에 내 accountId 멘션
//   assigned : (전 프로젝트) 담당자가 나로 지정됨 — assignee 대신 이 분류로 기록
// - 한 history 는 status > assignee > field 우선순위로 1개 알림만 생성(event_uid=cl:{historyId}).
// - APP 댓글이 멘션도 포함하면 mention 하나만 기록(comment 로 중복 기록하지 않음).
// - actor 가 내 accountId 인 이벤트는 셀프 알림 방지로 제외한다.

use crate::commands::{self, AppState};
use chrono::{DateTime, Duration, FixedOffset, Local};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration as StdDuration;
use tauri::{AppHandle, Emitter, Manager};

use crate::model::{JiraUser, PollResult};

/// HTTP 타임아웃 (기획서 4장)
const HTTP_TIMEOUT_SECS: u64 = 15;
/// 폴링 오버랩 (기획서 4장): t = 마지막 폴링시각 − 5분
const OVERLAP_MINS: i64 = 5;
/// 보존 정책 (기획서 5장): 읽음 + 90일 경과 알림 삭제
const RETENTION_DAYS: i64 = 90;
/// search maxResults / 페이징 안전 상한
const PAGE_SIZE: i64 = 50;
const MAX_ISSUES: usize = 500;

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ── 설정 스냅샷 ───────────────────────────────────────────

/// 폴링에 필요한 Setting 스냅샷 (DB 락 잠깐만 점유해 로드)
struct JiraConfig {
    enabled: bool,
    base_url: String,
    email: String,
    token: String,
    account_id: String,
    project: String,
    poll_secs: i64,
    /// 알림 받을 분류(비어 있으면 전부 on 으로 간주 → filter_enabled).
    categories: HashSet<String>,
    /// 담당자가 나인 이슈만(created·status·field 분류에만 적용).
    my_issues_only: bool,
}

/// Setting 에서 Jira 설정을 로드. 필수값(url/email/token) 비면 None.
fn load_config(app: &AppHandle) -> Option<JiraConfig> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().ok()?;
    let get = |k: &str| commands::read_setting(&conn, k).unwrap_or_default();
    let base_url = get("jira_base_url").trim_end_matches('/').to_string();
    let email = get("jira_email");
    let token = get("jira_api_token");
    if base_url.is_empty() || email.is_empty() || token.is_empty() {
        return None;
    }
    Some(JiraConfig {
        enabled: get("jira_enabled") == "1",
        base_url,
        email,
        token,
        account_id: get("jira_account_id"),
        project: {
            let p = get("jira_project");
            if p.is_empty() { "APP".to_string() } else { p }
        },
        poll_secs: get("jira_poll_secs").parse().unwrap_or(180).max(60),
        categories: get("jira_categories")
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        my_issues_only: get("jira_my_issues_only") == "1",
    })
}

// ── HTTP 클라이언트 ───────────────────────────────────────

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(StdDuration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// GET 요청 → JSON. Basic 인증 + Accept 헤더. 실패/비2xx 는 Err.
fn get_json(
    client: &reqwest::blocking::Client,
    url: &str,
    email: &str,
    token: &str,
    query: &[(&str, &str)],
) -> Result<Value, String> {
    let resp = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .query(query)
        .send()
        .map_err(e2s)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {} ({})", status.as_u16(), url));
    }
    resp.json::<Value>().map_err(e2s)
}

// ── 연결 테스트 (jira_test_connection) ────────────────────

/// /rest/api/3/myself 로 계정 확인. 성공 시 accountId/displayName 반환.
pub fn test_connection(url: &str, email: &str, token: &str) -> Result<JiraUser, String> {
    let base = url.trim().trim_end_matches('/');
    if base.is_empty() || email.trim().is_empty() || token.trim().is_empty() {
        return Err("URL·이메일·토큰을 모두 입력하세요".to_string());
    }
    let v = get_json(
        &client(),
        &format!("{}/rest/api/3/myself", base),
        email.trim(),
        token.trim(),
        &[],
    )
    .map_err(|e| format!("연결 실패: {}", e))?;
    Ok(JiraUser {
        account_id: v["accountId"].as_str().unwrap_or_default().to_string(),
        display_name: v["displayName"].as_str().unwrap_or_default().to_string(),
    })
}

// ── 시각 파싱 헬퍼 ────────────────────────────────────────

/// Jira 시각("2026-07-21T10:30:00.000+0900") 또는 RFC3339 를 파싱.
fn parse_time(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z")
        .or_else(|_| DateTime::parse_from_rfc3339(s))
        .ok()
}

/// event_time >= since_ts 여부. 파싱 실패 시 true(누락 방지, dedup 이 중복을 막음).
fn ge_since(time_str: &str, since_ts: i64) -> bool {
    match parse_time(time_str) {
        Some(t) => t.timestamp() >= since_ts,
        None => true,
    }
}

/// event_at 저장용 정규화 (파싱되면 RFC3339, 아니면 원문). 최신순 정렬 안정화.
fn norm_event_at(s: &str) -> String {
    parse_time(s).map(|t| t.to_rfc3339()).unwrap_or_else(|| s.to_string())
}

// ── 문자열 헬퍼 ───────────────────────────────────────────

/// 이슈 키("APP-123")에서 프로젝트 키 추출("APP"). '-' 없으면 원문.
fn project_of(issue_key: &str) -> String {
    issue_key
        .split_once('-')
        .map(|(p, _)| p.to_string())
        .unwrap_or_else(|| issue_key.to_string())
}

/// 앞 n 글자 자르고 넘치면 … 부착 (char 경계 안전)
fn truncate_chars(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{}…", t)
    } else {
        t
    }
}

/// changelog 필드명 → 표시용 한글(best-effort). 미매핑은 원문 유지.
fn field_label(field: &str) -> String {
    match field {
        "priority" => "우선순위",
        "labels" => "라벨",
        "summary" => "제목",
        "description" => "설명",
        "duedate" => "기한",
        "resolution" => "해결",
        "fixVersion" => "수정버전",
        "Component" | "components" => "컴포넌트",
        "issuetype" => "유형",
        "Sprint" => "스프린트",
        "Epic Link" => "에픽",
        "reporter" => "보고자",
        "Rank" => "순위",
        "timeestimate" | "timeoriginalestimate" => "예상시간",
        _ => field,
    }
    .to_string()
}

// ── ADF (댓글 본문) 처리 ──────────────────────────────────

/// ADF 트리에서 표시용 텍스트를 재귀 추출. mention 노드는 attrs.text 사용.
fn adf_text(node: &Value, out: &mut String) {
    if node.get("type").and_then(|v| v.as_str()) == Some("mention") {
        if let Some(t) = node
            .get("attrs")
            .and_then(|a| a.get("text"))
            .and_then(|v| v.as_str())
        {
            out.push_str(t);
            out.push(' ');
        }
    } else if let Some(t) = node.get("text").and_then(|v| v.as_str()) {
        out.push_str(t);
    }
    if let Some(content) = node.get("content").and_then(|v| v.as_array()) {
        for c in content {
            adf_text(c, out);
        }
    }
}

/// 댓글 body(ADF) 직렬화 문자열에 내 accountId 부분문자열이 있으면 멘션(기획서 4장).
fn body_mentions_me(body: &Value, my_id: &str) -> bool {
    if my_id.is_empty() {
        return false;
    }
    serde_json::to_string(body)
        .map(|s| s.contains(my_id))
        .unwrap_or(false)
}

// ── 이벤트 모델 ───────────────────────────────────────────

/// 분류된 알림 1건 (DB 삽입 전 표현). fetched_at 은 삽입 시점에 부여.
#[derive(Debug, Clone, PartialEq)]
pub struct NotifEvent {
    pub event_uid: String,
    pub issue_key: String,
    pub project_key: String,
    pub category: String,
    pub summary: String,
    pub detail: String,
    pub actor: String,
    pub event_at: String,
}

// ── 이벤트 분류 (순수 함수 — 네트워크 없이 테스트) ────────

/// APP 이슈 하나에서 created + changelog + 댓글 이벤트를 분류.
/// - issue: search 응답의 issue 객체 (fields.summary/created/creator/comment/assignee 포함)
/// - histories: changelog.histories 배열(오래된→최신). since 이후만 처리.
/// - comments: 댓글 배열(fields.comment.comments 또는 별도 조회분).
/// - since_ts: 오버랩 반영한 기준 시각(Unix). my_id: 셀프 알림 제외용.
/// - my_issues_only: true 면 created·status·field 는 "현재 담당자가 나인 이슈"만 남긴다.
///   (assignee·assigned·comment·mention 은 미적용). 판정은 이벤트 시점이 아닌
///   search 응답의 "현재 assignee" 기준이라 과거 시점 담당자와 다를 수 있는 근사이다.
pub fn classify_app_issue(
    issue: &Value,
    histories: &[Value],
    comments: &[Value],
    since_ts: i64,
    my_id: &str,
    my_issues_only: bool,
) -> Vec<NotifEvent> {
    let mut out = Vec::new();
    let key = issue["key"].as_str().unwrap_or_default().to_string();
    if key.is_empty() {
        return out;
    }
    let proj = project_of(&key);
    let summary = issue["fields"]["summary"].as_str().unwrap_or_default().to_string();
    // 현재 담당자가 나인지(my_id 빈값이면 false). my_issues_only 필터에만 사용.
    let is_mine = !my_id.is_empty()
        && issue["fields"]["assignee"]["accountId"].as_str() == Some(my_id);
    // created·status·field 에만 적용하는 담당자 필터(true 면 스킵).
    let skip_by_owner = my_issues_only && !is_mine;

    // 1) created — creator, created >= t, actor != me (my_issues_only 면 내 담당만)
    let created = issue["fields"]["created"].as_str().unwrap_or_default();
    let creator = &issue["fields"]["creator"];
    let creator_id = creator["accountId"].as_str().unwrap_or_default();
    if !skip_by_owner && !created.is_empty() && ge_since(created, since_ts) && creator_id != my_id {
        out.push(NotifEvent {
            event_uid: format!("{}:created", key),
            issue_key: key.clone(),
            project_key: proj.clone(),
            category: "created".to_string(),
            summary: summary.clone(),
            detail: "이슈 생성".to_string(),
            actor: creator["displayName"].as_str().unwrap_or_default().to_string(),
            event_at: norm_event_at(created),
        });
    }

    // 2) changelog — history 1건당 최대 1알림 (status > assignee > field)
    for h in histories {
        let h_created = h["created"].as_str().unwrap_or_default();
        if h_created.is_empty() || !ge_since(h_created, since_ts) {
            continue;
        }
        let author = &h["author"];
        if author["accountId"].as_str().unwrap_or_default() == my_id {
            continue;
        }
        let hist_id = h["id"].as_str().unwrap_or_default();
        let items = match h["items"].as_array() {
            Some(a) => a,
            None => continue,
        };
        if items.is_empty() {
            continue;
        }
        let actor = author["displayName"].as_str().unwrap_or_default().to_string();
        let ev_at = norm_event_at(h_created);
        let uid = format!("{}:cl:{}", key, hist_id);

        let find = |name: &str| items.iter().find(|it| it["field"].as_str() == Some(name));

        let (category, detail) = if let Some(it) = find("status") {
            (
                "status",
                format!(
                    "{} → {}",
                    it["fromString"].as_str().unwrap_or("-"),
                    it["toString"].as_str().unwrap_or("-")
                ),
            )
        } else if let Some(it) = find("assignee") {
            // to(accountId)가 나면 "내 담당 지정"(assigned), 아니면 일반 담당자 변경(assignee).
            if !my_id.is_empty() && it["to"].as_str() == Some(my_id) {
                (
                    "assigned",
                    format!(
                        "담당자로 지정됨: {} → {}",
                        it["fromString"].as_str().unwrap_or("없음"),
                        it["toString"].as_str().unwrap_or("없음")
                    ),
                )
            } else {
                (
                    "assignee",
                    format!(
                        "담당자: {} → {}",
                        it["fromString"].as_str().unwrap_or("없음"),
                        it["toString"].as_str().unwrap_or("없음")
                    ),
                )
            }
        } else {
            // 그 외 변경: 필드명 나열
            let names: Vec<String> = items
                .iter()
                .filter_map(|it| it["field"].as_str())
                .map(field_label)
                .collect();
            ("field", format!("변경: {}", names.join(", ")))
        };

        // 담당자 필터: status·field 만 대상(assignee·assigned 은 담당자와 무관하게 유지).
        if skip_by_owner && (category == "status" || category == "field") {
            continue;
        }

        out.push(NotifEvent {
            event_uid: uid,
            issue_key: key.clone(),
            project_key: proj.clone(),
            category: category.to_string(),
            summary: summary.clone(),
            detail,
            actor,
            event_at: ev_at,
        });
    }

    // 3) 댓글 — 멘션이면 mention, 아니면 comment (APP 이슈는 comment 도 기록)
    out.extend(classify_comments(&key, &proj, &summary, comments, since_ts, my_id, true));
    out
}

/// 타 프로젝트 이슈의 댓글에서 멘션 이벤트만 분류(기획서 3장 mention 스캔).
pub fn classify_mention_issue(
    issue: &Value,
    comments: &[Value],
    since_ts: i64,
    my_id: &str,
) -> Vec<NotifEvent> {
    let key = issue["key"].as_str().unwrap_or_default().to_string();
    if key.is_empty() {
        return Vec::new();
    }
    let proj = project_of(&key);
    let summary = issue["fields"]["summary"].as_str().unwrap_or_default().to_string();
    // is_app=false → 멘션 아닌 댓글은 무시
    classify_comments(&key, &proj, &summary, comments, since_ts, my_id, false)
}

/// 전 프로젝트 "assignee CHANGED TO currentUser()" 스캔 결과에서
/// "나에게 담당 지정"된 changelog history 만 assigned 이벤트로 분류.
/// - to(accountId) == my_id, created >= since, author != me 인 history 만 채택.
/// - event_uid 는 classify_app_issue 와 동일한 {key}:cl:{historyId} → INSERT OR IGNORE 자연 dedup.
pub fn classify_assigned_issue(
    issue: &Value,
    histories: &[Value],
    since_ts: i64,
    my_id: &str,
) -> Vec<NotifEvent> {
    let mut out = Vec::new();
    if my_id.is_empty() {
        return out;
    }
    let key = issue["key"].as_str().unwrap_or_default().to_string();
    if key.is_empty() {
        return out;
    }
    let proj = project_of(&key);
    let summary = issue["fields"]["summary"].as_str().unwrap_or_default().to_string();

    for h in histories {
        let h_created = h["created"].as_str().unwrap_or_default();
        if h_created.is_empty() || !ge_since(h_created, since_ts) {
            continue;
        }
        let author = &h["author"];
        if author["accountId"].as_str().unwrap_or_default() == my_id {
            continue;
        }
        let items = match h["items"].as_array() {
            Some(a) => a,
            None => continue,
        };
        let it = match items.iter().find(|it| it["field"].as_str() == Some("assignee")) {
            Some(it) => it,
            None => continue,
        };
        if it["to"].as_str() != Some(my_id) {
            continue;
        }
        let hist_id = h["id"].as_str().unwrap_or_default();
        out.push(NotifEvent {
            event_uid: format!("{}:cl:{}", key, hist_id),
            issue_key: key.clone(),
            project_key: proj.clone(),
            category: "assigned".to_string(),
            summary: summary.clone(),
            detail: format!(
                "담당자로 지정됨: {} → {}",
                it["fromString"].as_str().unwrap_or("없음"),
                it["toString"].as_str().unwrap_or("없음")
            ),
            actor: author["displayName"].as_str().unwrap_or_default().to_string(),
            event_at: norm_event_at(h_created),
        });
    }
    out
}

/// 댓글 목록 공통 분류. is_app_project=true 면 비멘션도 comment 로 기록, false 면 멘션만.
fn classify_comments(
    key: &str,
    proj: &str,
    summary: &str,
    comments: &[Value],
    since_ts: i64,
    my_id: &str,
    is_app_project: bool,
) -> Vec<NotifEvent> {
    let mut out = Vec::new();
    for c in comments {
        let created = c["created"].as_str().unwrap_or_default();
        if created.is_empty() || !ge_since(created, since_ts) {
            continue;
        }
        let author = &c["author"];
        if author["accountId"].as_str().unwrap_or_default() == my_id {
            continue;
        }
        let body = &c["body"];
        let is_mention = body_mentions_me(body, my_id);
        if !is_mention && !is_app_project {
            continue; // 타 프로젝트는 멘션만
        }
        let comment_id = c["id"].as_str().unwrap_or_default();
        let mut text = String::new();
        adf_text(body, &mut text);
        let snippet = truncate_chars(text.trim(), 80);

        out.push(NotifEvent {
            event_uid: format!("{}:cm:{}", key, comment_id),
            issue_key: key.to_string(),
            project_key: proj.to_string(),
            category: if is_mention { "mention" } else { "comment" }.to_string(),
            summary: summary.to_string(),
            detail: format!("댓글: {}", snippet),
            actor: author["displayName"].as_str().unwrap_or_default().to_string(),
            event_at: norm_event_at(created),
        });
    }
    out
}

// ── DB 저장 · 정리 ────────────────────────────────────────

/// enabled 분류만 남긴다(삽입 전 필터). categories 가 비어 있으면 전부 통과(설정 없음=전부 on).
fn filter_enabled(events: Vec<NotifEvent>, categories: &HashSet<String>) -> Vec<NotifEvent> {
    if categories.is_empty() {
        return events;
    }
    events
        .into_iter()
        .filter(|e| categories.contains(&e.category))
        .collect()
}

/// 이벤트 일괄 저장(INSERT OR IGNORE dedup). 실제 신규 삽입된 행 수를 반환.
pub fn insert_events(conn: &Connection, events: &[NotifEvent], fetched_at: &str) -> Result<usize, String> {
    let mut n = 0usize;
    for e in events {
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO JiraNotification \
                 (event_uid, issue_key, project_key, category, summary, detail, actor, event_at, fetched_at, read) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0)",
                params![
                    e.event_uid,
                    e.issue_key,
                    e.project_key,
                    e.category,
                    e.summary,
                    e.detail,
                    e.actor,
                    e.event_at,
                    fetched_at
                ],
            )
            .map_err(e2s)?;
        n += changed;
    }
    Ok(n)
}

/// 보존 정책: 읽음 + RETENTION_DAYS 경과 알림 삭제(테이블 무한 성장 방지).
fn cleanup_old(conn: &Connection) -> Result<(), String> {
    let cutoff = (Local::now() - Duration::days(RETENTION_DAYS)).to_rfc3339();
    conn.execute(
        "DELETE FROM JiraNotification WHERE read=1 AND event_at < ?1",
        params![cutoff],
    )
    .map_err(e2s)?;
    Ok(())
}

/// 안읽음 개수 (트레이 툴팁·배지 공용)
pub fn unread_count(conn: &Connection) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM JiraNotification WHERE read=0",
        [],
        |r| r.get(0),
    )
    .map_err(e2s)
}

// ── 네트워크 폴링 ─────────────────────────────────────────

/// search/jql 페이징 조회. 신 엔드포인트 /rest/api/3/search/jql 사용(구 /search 금지).
/// 반환: (이슈 목록, 캡 절단 여부). MAX_ISSUES 도달로 페이징을 중단했는데
/// 다음 페이지가 남아 있으면 truncated=true (장시간 오프라인 후 첫 폴링 등).
fn search_jql(
    client: &reqwest::blocking::Client,
    cfg: &JiraConfig,
    jql: &str,
    fields: &str,
) -> Result<(Vec<Value>, bool), String> {
    let url = format!("{}/rest/api/3/search/jql", cfg.base_url);
    let max = PAGE_SIZE.to_string();
    let mut issues: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    let mut truncated = false;
    loop {
        let mut query: Vec<(&str, &str)> =
            vec![("jql", jql), ("fields", fields), ("maxResults", max.as_str())];
        if let Some(t) = &next {
            query.push(("nextPageToken", t.as_str()));
        }
        let v = get_json(client, &url, &cfg.email, &cfg.token, &query)?;
        if let Some(arr) = v["issues"].as_array() {
            issues.extend(arr.iter().cloned());
        }
        match v["nextPageToken"].as_str() {
            Some(t) if !t.is_empty() => {
                if issues.len() >= MAX_ISSUES {
                    truncated = true;
                    break;
                }
                next = Some(t.to_string());
            }
            _ => break,
        }
    }
    Ok((issues, truncated))
}

/// 마지막 수신 이슈의 updated (ORDER BY updated ASC 이므로 = 수신분의 최대값).
/// 캡 절단 시 다음 폴링 기준선(워터마크)으로 쓴다.
fn last_updated_of(issues: &[Value]) -> Option<DateTime<FixedOffset>> {
    issues
        .last()
        .and_then(|i| i["fields"]["updated"].as_str())
        .and_then(parse_time)
}

/// 절단된 두 검색의 워터마크 병합 — 둘 다 있으면 이른 쪽(미수신 구간이 안 생기도록).
fn merge_watermarks(
    a: Option<DateTime<FixedOffset>>,
    b: Option<DateTime<FixedOffset>>,
) -> Option<DateTime<FixedOffset>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// 이슈의 changelog histories 조회. search 응답에는 changelog 가 없으므로 이슈별 조회.
/// total > 100 이면 마지막 페이지(startAt=total-100)를 재조회해 최신 100건을 확보한다.
fn fetch_histories(client: &reqwest::blocking::Client, cfg: &JiraConfig, key: &str) -> Vec<Value> {
    let url = format!("{}/rest/api/3/issue/{}", cfg.base_url, key);
    let v = match get_json(
        client,
        &url,
        &cfg.email,
        &cfg.token,
        &[("expand", "changelog"), ("fields", "summary")],
    ) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let total = v["changelog"]["total"].as_i64().unwrap_or(0);
    if total > 100 {
        let start = (total - 100).to_string();
        let curl = format!("{}/rest/api/3/issue/{}/changelog", cfg.base_url, key);
        if let Ok(cv) = get_json(
            client,
            &curl,
            &cfg.email,
            &cfg.token,
            &[("startAt", start.as_str()), ("maxResults", "100")],
        ) {
            if let Some(a) = cv["values"].as_array() {
                return a.clone();
            }
        }
    }
    v["changelog"]["histories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// 이슈의 댓글 목록. search 응답의 fields.comment.comments 를 쓰되,
/// comment.total 이 더 크면 /issue/{key}/comment 로 최근 50건을 보강한다.
fn fetch_comments(client: &reqwest::blocking::Client, cfg: &JiraConfig, issue: &Value) -> Vec<Value> {
    let key = issue["key"].as_str().unwrap_or_default();
    let comment = &issue["fields"]["comment"];
    let inline = comment["comments"].as_array().cloned().unwrap_or_default();
    let total = comment["total"].as_i64().unwrap_or(inline.len() as i64);
    if total as usize <= inline.len() || key.is_empty() {
        return inline;
    }
    // 보강: 최근순 50건
    let url = format!("{}/rest/api/3/issue/{}/comment", cfg.base_url, key);
    match get_json(
        client,
        &url,
        &cfg.email,
        &cfg.token,
        &[("orderBy", "-created"), ("maxResults", "50")],
    ) {
        Ok(v) => v["comments"].as_array().cloned().unwrap_or(inline),
        Err(_) => inline,
    }
}

/// 네트워크 구간: 두 JQL 을 폴링하고 이벤트를 분류해 반환(DB 락 없음).
/// 워터마크(Option): 캡 절단이 있었으면 다음 폴링이 이어받을 기준 시각.
fn fetch_and_classify(
    cfg: &JiraConfig,
    since_ts: i64,
    t_str: &str,
) -> Result<(Vec<NotifEvent>, Option<DateTime<FixedOffset>>), String> {
    let client = client();
    let fields = "summary,status,assignee,creator,reporter,created,updated,comment";
    let mut events: Vec<NotifEvent> = Vec::new();

    // APP 프로젝트: 생성·변경·댓글
    let jql_app = format!(
        "project = \"{}\" AND updated >= \"{}\" ORDER BY updated ASC",
        cfg.project, t_str
    );
    let (app_issues, app_trunc) = search_jql(&client, cfg, &jql_app, fields)?;
    for issue in &app_issues {
        let key = issue["key"].as_str().unwrap_or_default();
        let histories = fetch_histories(&client, cfg, key);
        let comments = fetch_comments(&client, cfg, issue);
        events.extend(classify_app_issue(
            issue,
            &histories,
            &comments,
            since_ts,
            &cfg.account_id,
            cfg.my_issues_only,
        ));
    }

    // 타 프로젝트: 멘션 댓글만
    let jql_men = format!(
        "project != \"{}\" AND updated >= \"{}\" ORDER BY updated ASC",
        cfg.project, t_str
    );
    let (men_issues, men_trunc) = search_jql(&client, cfg, &jql_men, fields)?;
    for issue in &men_issues {
        let comments = fetch_comments(&client, cfg, issue);
        events.extend(classify_mention_issue(issue, &comments, since_ts, &cfg.account_id));
    }

    // 전 프로젝트: 나에게 담당자 지정(assignee CHANGED TO). APP 포함이어도 uid 동일 → dedup.
    let jql_asg = format!(
        "assignee CHANGED TO currentUser() AFTER \"{}\" ORDER BY updated ASC",
        t_str
    );
    let (asg_issues, asg_trunc) = search_jql(&client, cfg, &jql_asg, fields)?;
    for issue in &asg_issues {
        let key = issue["key"].as_str().unwrap_or_default();
        let histories = fetch_histories(&client, cfg, key);
        events.extend(classify_assigned_issue(issue, &histories, since_ts, &cfg.account_id));
    }

    let watermark = merge_watermarks(
        merge_watermarks(
            if app_trunc { last_updated_of(&app_issues) } else { None },
            if men_trunc { last_updated_of(&men_issues) } else { None },
        ),
        if asg_trunc { last_updated_of(&asg_issues) } else { None },
    );
    Ok((events, watermark))
}

/// since(오버랩 반영) 계산. last_poll 있으면 −5분, 없으면 now−poll_secs 윈도우.
fn compute_since(last_poll: &str, poll_secs: i64) -> DateTime<Local> {
    match parse_time(last_poll) {
        Some(t) => (t - Duration::minutes(OVERLAP_MINS)).with_timezone(&Local),
        None => Local::now() - Duration::seconds(poll_secs),
    }
}

/// JQL 시각 문자열 (KST, "yyyy/MM/dd HH:mm")
fn jql_time_str(since: DateTime<Local>) -> String {
    let kst = FixedOffset::east_opt(9 * 3600).unwrap();
    since.with_timezone(&kst).format("%Y/%m/%d %H:%M").to_string()
}

/// account_id 미설정(연결 테스트 생략) 시 /myself 로 1회 조회해 저장.
/// 비어 있으면 멘션 감지·셀프 알림 제외가 조용히 무동작하기 때문.
fn ensure_account_id(app: &AppHandle, cfg: &mut JiraConfig) {
    if !cfg.account_id.is_empty() {
        return;
    }
    if let Ok(u) = test_connection(&cfg.base_url, &cfg.email, &cfg.token) {
        if !u.account_id.is_empty() {
            setting_write(app, "jira_account_id", &u.account_id);
            cfg.account_id = u.account_id;
        }
    }
}

/// 폴링 실행 + 결과 기록. 성공 시 last_poll 갱신·오류 초기화, 신규 있으면 emit·툴팁.
/// 반환: (신규 삽입 수, 오류 메시지 Option)
fn run_and_record(app: &AppHandle, mut cfg: JiraConfig) -> (i64, Option<String>) {
    ensure_account_id(app, &mut cfg);
    let last_poll = setting_str(app, "jira_last_poll");
    let since = compute_since(&last_poll, cfg.poll_secs);
    let since_ts = since.timestamp();
    let t_str = jql_time_str(since);

    match fetch_and_classify(&cfg, since_ts, &t_str) {
        Ok((events, watermark)) => {
            // 삽입 전 enabled 분류만 남긴다(설정 없음=전부 on).
            let events = filter_enabled(events, &cfg.categories);
            let now = Local::now();
            let inserted = {
                let state = app.state::<AppState>();
                let conn = match state.db.lock() {
                    Ok(c) => c,
                    Err(_) => return (0, Some("DB 잠금 실패".to_string())),
                };
                let n = insert_events(&conn, &events, &now.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or(0);
                let _ = cleanup_old(&conn);
                n
            };
            // 캡 절단 시 last_poll 을 워터마크로 기록해 다음 폴링이 이어서 가져간다.
            // 워터마크가 since 이하면 진행이 안 되므로(같은 구간 반복) now 로 폴백(초과분 유실 감수).
            let baseline = match watermark {
                Some(w) if w.timestamp() > since_ts => w.with_timezone(&Local),
                _ => now,
            };
            setting_write(app, "jira_last_poll", &baseline.to_rfc3339());
            setting_write(app, "jira_last_error", "");
            if inserted > 0 {
                emit_updated(app);
            }
            crate::tray::update_tooltip(app);
            (inserted as i64, None)
        }
        Err(e) => {
            setting_write(app, "jira_last_error", &e);
            (0, Some(e))
        }
    }
}

/// 안읽음 수를 담아 프론트에 jira-updated emit (capability 이미 listen 허용).
fn emit_updated(app: &AppHandle) {
    let unread = {
        let state = app.state::<AppState>();
        state
            .db
            .lock()
            .ok()
            .and_then(|c| unread_count(&c).ok())
            .unwrap_or(0)
    };
    let _ = app.emit_to("main", "jira-updated", unread);
}

// ── Setting 접근 (notify.rs 패턴과 동일한 short-lock 헬퍼) ──

fn setting_str(app: &AppHandle, key: &str) -> String {
    let state = app.state::<AppState>();
    state
        .db
        .lock()
        .ok()
        .and_then(|c| commands::read_setting(&c, key))
        .unwrap_or_default()
}

fn setting_write(app: &AppHandle, key: &str, value: &str) {
    let state = app.state::<AppState>();
    let Ok(conn) = state.db.lock() else {
        return;
    };
    let _ = commands::write_setting(&conn, key, value);
}

// ── 진입점 ────────────────────────────────────────────────

/// 백그라운드 폴링 틱 (notify.rs 30초 루프에서 호출).
/// jira_enabled + 경과시간(jira_poll_secs) 을 확인해 조건 충족 시에만 실제 폴링한다.
/// 최초(마지막 폴링 기록 없음)에는 기준선만 세우고 스캔을 건너뛴다(과거 알림 폭주 방지).
pub fn poll_tick(app: &AppHandle) {
    let cfg = match load_config(app) {
        Some(c) if c.enabled => c,
        _ => return,
    };
    let last_poll = setting_str(app, "jira_last_poll");
    if last_poll.is_empty() {
        // 최초: 기준선만 기록하고 이번 틱은 스캔 생략
        setting_write(app, "jira_last_poll", &Local::now().to_rfc3339());
        return;
    }
    // 경과시간 미달이면 skip
    if let Some(lt) = parse_time(&last_poll) {
        let elapsed = (Local::now() - lt.with_timezone(&Local)).num_seconds();
        if elapsed < cfg.poll_secs {
            return;
        }
    }
    let _ = run_and_record(app, cfg);
}

/// 수동 새로고침 (jira_poll_now 커맨드). 비활성/미설정이면 error 로 반환.
pub fn poll_now(app: &AppHandle) -> PollResult {
    let cfg = match load_config(app) {
        Some(c) => c,
        None => {
            return PollResult {
                new_count: 0,
                error: Some("Jira URL·이메일·토큰을 먼저 설정하세요".to_string()),
            }
        }
    };
    if !cfg.enabled {
        return PollResult {
            new_count: 0,
            error: Some("Jira 연동이 비활성화되어 있습니다".to_string()),
        };
    }
    let (new_count, error) = run_and_record(app, cfg);
    PollResult { new_count, error }
}

// ── 단위 테스트 (네트워크 없이 파싱·분류·dedup 검증) ──────
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ME: &str = "712020:44cbc0f7-2605-435d-8d42-9c07e7ed9487";
    const OTHER: &str = "557058:aaaa-bbbb";

    // since 기준: 2000년(항상 과거) → 모든 fixture 이벤트 통과
    fn since_all() -> i64 {
        parse_time("2000-01-01T00:00:00.000+0900").unwrap().timestamp()
    }

    /// APP 이슈 fixture (creator=OTHER, created 최근)
    fn app_issue() -> Value {
        json!({
            "key": "APP-123",
            "fields": {
                "summary": "로그인 화면 개선",
                "created": "2026-07-21T09:00:00.000+0900",
                "creator": { "accountId": OTHER, "displayName": "김창조" }
            }
        })
    }

    /// status 변경 history
    fn status_history() -> Value {
        json!({
            "id": "1001",
            "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "status", "fromString": "할 일", "toString": "진행 중" }]
        })
    }

    /// 멘션 포함 댓글 ADF (내 accountId 가 mention 노드 attrs.id 에 포함)
    fn mention_comment() -> Value {
        json!({
            "id": "5001",
            "created": "2026-07-21T11:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "body": {
                "type": "doc", "version": 1,
                "content": [{ "type": "paragraph", "content": [
                    { "type": "mention", "attrs": { "id": ME, "text": "@이지수" } },
                    { "type": "text", "text": " 이 부분 확인 부탁드립니다 정말 감사합니다" }
                ]}]
            }
        })
    }

    /// 일반 댓글(멘션 없음)
    fn plain_comment() -> Value {
        json!({
            "id": "5002",
            "created": "2026-07-21T12:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "박댓글" },
            "body": {
                "type": "doc", "version": 1,
                "content": [{ "type": "paragraph", "content": [
                    { "type": "text", "text": "수정 완료했습니다" }
                ]}]
            }
        })
    }

    // 1) created + status + comment 분류 (APP 이슈)
    #[test]
    fn classify_created_status_comment() {
        let ev = classify_app_issue(
            &app_issue(),
            &[status_history()],
            &[plain_comment()],
            since_all(),
            ME,
            false,
        );
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0].category, "created");
        assert_eq!(ev[0].event_uid, "APP-123:created");
        assert_eq!(ev[1].category, "status");
        assert_eq!(ev[1].event_uid, "APP-123:cl:1001");
        assert_eq!(ev[1].detail, "할 일 → 진행 중");
        assert_eq!(ev[2].category, "comment");
        assert_eq!(ev[2].event_uid, "APP-123:cm:5002");
        assert_eq!(ev[2].project_key, "APP");
    }

    // 2) APP 댓글이 멘션 포함이면 mention 하나만 (comment 중복 없음)
    #[test]
    fn app_comment_with_mention_is_mention_only() {
        let issue = json!({
            "key": "APP-200",
            "fields": { "summary": "제목", "created": "2020-01-01T00:00:00.000+0900",
                        "creator": { "accountId": OTHER, "displayName": "김창조" } }
        });
        let ev = classify_app_issue(&issue, &[], &[mention_comment()], since_all(), ME, false);
        // created 는 since_all 보다 이후이므로 1건 + 멘션 1건 = 2건. 댓글은 mention 만.
        let cats: Vec<&str> = ev.iter().map(|e| e.category.as_str()).collect();
        assert!(cats.contains(&"mention"));
        assert!(!cats.contains(&"comment"));
        let m = ev.iter().find(|e| e.category == "mention").unwrap();
        assert_eq!(m.event_uid, "APP-200:cm:5001");
        assert!(m.detail.contains("@이지수"));
    }

    // 3) 타 프로젝트: 멘션 댓글만 기록, 일반 댓글은 무시
    #[test]
    fn other_project_only_mentions() {
        let issue = json!({
            "key": "DEV-9",
            "fields": { "summary": "타프로젝트 이슈" }
        });
        let ev = classify_mention_issue(&issue, &[mention_comment(), plain_comment()], since_all(), ME);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].category, "mention");
        assert_eq!(ev[0].project_key, "DEV");
        assert_eq!(ev[0].issue_key, "DEV-9");
    }

    // 4) 셀프 알림 제외: actor 가 나면 created/changelog/comment 모두 스킵
    #[test]
    fn self_actor_excluded() {
        let issue = json!({
            "key": "APP-1",
            "fields": { "summary": "s", "created": "2026-07-21T09:00:00.000+0900",
                        "creator": { "accountId": ME, "displayName": "이지수" } }
        });
        let my_hist = json!({
            "id": "1", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": ME, "displayName": "이지수" },
            "items": [{ "field": "status", "fromString": "a", "toString": "b" }]
        });
        let my_comment = json!({
            "id": "9", "created": "2026-07-21T11:00:00.000+0900",
            "author": { "accountId": ME, "displayName": "이지수" },
            "body": { "type": "doc", "content": [] }
        });
        let ev = classify_app_issue(&issue, &[my_hist], &[my_comment], since_all(), ME, false);
        assert!(ev.is_empty());
    }

    // 5) history 우선순위: assignee 만 있으면 assignee, status 없고 그 외면 field
    #[test]
    fn changelog_category_priority() {
        let issue = app_issue();
        let assignee_h = json!({
            "id": "2001", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "fromString": "홍길동", "toString": "이지수" }]
        });
        let field_h = json!({
            "id": "2002", "created": "2026-07-21T10:05:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [
                { "field": "priority", "fromString": "Medium", "toString": "High" },
                { "field": "labels", "fromString": "", "toString": "urgent" }
            ]
        });
        // created 는 검증에서 제외하려고 since 를 created 이후로 설정
        let since = parse_time("2026-07-21T09:30:00.000+0900").unwrap().timestamp();
        let ev = classify_app_issue(&issue, &[assignee_h, field_h], &[], since, ME, false);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].category, "assignee");
        assert_eq!(ev[1].category, "field");
        assert_eq!(ev[1].detail, "변경: 우선순위, 라벨");
    }

    // 6) since 필터: 기준 이전 이벤트는 제외
    #[test]
    fn since_filter_excludes_old() {
        let issue = app_issue(); // created 09:00
        // since = 09:30 → created(09:00) 제외, status(10:00) 포함
        let since = parse_time("2026-07-21T09:30:00.000+0900").unwrap().timestamp();
        let ev = classify_app_issue(&issue, &[status_history()], &[], since, ME, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].category, "status");
    }

    // 7) dedup: 같은 이벤트를 두 번 삽입해도 INSERT OR IGNORE 로 1건만 저장
    #[test]
    fn dedup_insert_or_ignore() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_test(&conn);
        let events = classify_app_issue(
            &app_issue(),
            &[status_history()],
            &[plain_comment()],
            since_all(),
            ME,
            false,
        );
        let first = insert_events(&conn, &events, "2026-07-21 12:00:00").unwrap();
        let second = insert_events(&conn, &events, "2026-07-21 12:03:00").unwrap();
        assert_eq!(first, 3); // created + status + comment
        assert_eq!(second, 0); // 중복 → 모두 무시
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM JiraNotification", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3);
        assert_eq!(unread_count(&conn).unwrap(), 3);
    }

    // 9) 캡 절단 워터마크: 마지막 수신 이슈의 updated, 병합은 이른 쪽
    #[test]
    fn watermark_last_updated_and_merge() {
        let issues = vec![
            json!({ "key": "APP-1", "fields": { "updated": "2026-07-21T10:00:00.000+0900" } }),
            json!({ "key": "APP-2", "fields": { "updated": "2026-07-21T11:00:00.000+0900" } }),
        ];
        let w = last_updated_of(&issues).unwrap();
        assert_eq!(w, parse_time("2026-07-21T11:00:00.000+0900").unwrap());
        // updated 없는 이슈만 있으면 None (run_and_record 에서 now 폴백)
        assert!(last_updated_of(&[json!({ "key": "X-1", "fields": {} })]).is_none());
        assert!(last_updated_of(&[]).is_none());

        let early = parse_time("2026-07-21T09:00:00.000+0900");
        let late = parse_time("2026-07-21T11:00:00.000+0900");
        assert_eq!(merge_watermarks(early, late), early); // 둘 다 절단 → 이른 쪽
        assert_eq!(merge_watermarks(None, late), late);   // 한쪽만 절단 → 그 쪽
        assert_eq!(merge_watermarks(None, None), None);
    }

    // 8) ADF 텍스트 추출 + 80자 절단
    #[test]
    fn adf_text_and_truncate() {
        let mut s = String::new();
        adf_text(&mention_comment()["body"], &mut s);
        assert!(s.contains("@이지수"));
        assert!(s.contains("확인 부탁드립니다"));
        assert_eq!(truncate_chars("가나다라마", 3), "가나다…");
        assert_eq!(truncate_chars("가나다", 3), "가나다");
    }

    // 10) assignee 변경의 to 가 나면 category=assigned, 아니면 assignee (classify_app_issue)
    #[test]
    fn assignee_to_me_becomes_assigned() {
        let issue = app_issue();
        let since = parse_time("2026-07-21T09:30:00.000+0900").unwrap().timestamp();
        // 나에게 지정 → assigned
        let to_me = json!({
            "id": "3001", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "from": null, "to": ME,
                        "fromString": "없음", "toString": "이지수" }]
        });
        let ev = classify_app_issue(&issue, &[to_me], &[], since, ME, false);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].category, "assigned");
        assert_eq!(ev[0].event_uid, "APP-123:cl:3001");
        assert_eq!(ev[0].detail, "담당자로 지정됨: 없음 → 이지수");
        // 남에게 지정 → 여전히 assignee
        let to_other = json!({
            "id": "3002", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "to": OTHER,
                        "fromString": "없음", "toString": "박담당" }]
        });
        let ev2 = classify_app_issue(&issue, &[to_other], &[], since, ME, false);
        assert_eq!(ev2.len(), 1);
        assert_eq!(ev2[0].category, "assignee");
    }

    // 11) classify_assigned_issue: to==me 만 생성, author==me 제외, since 이전 제외
    #[test]
    fn classify_assigned_only_to_me() {
        let issue = json!({ "key": "DEV-5", "fields": { "summary": "타프로젝트 이슈" } });
        let to_me = json!({
            "id": "4001", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "to": ME, "fromString": "없음", "toString": "이지수" }]
        });
        let to_other = json!({ // 남에게 지정 → 제외
            "id": "4002", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "to": OTHER, "fromString": "없음", "toString": "박담당" }]
        });
        let by_me = json!({ // 내가 나를 지정(author==me) → 제외
            "id": "4003", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": ME, "displayName": "이지수" },
            "items": [{ "field": "assignee", "to": ME, "fromString": "없음", "toString": "이지수" }]
        });
        let old = json!({ // since 이전 → 제외
            "id": "4004", "created": "2026-07-21T08:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "to": ME, "fromString": "없음", "toString": "이지수" }]
        });
        let since = parse_time("2026-07-21T09:30:00.000+0900").unwrap().timestamp();
        let ev = classify_assigned_issue(&issue, &[to_me, to_other, by_me, old], since, ME);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].category, "assigned");
        assert_eq!(ev[0].event_uid, "DEV-5:cl:4001");
        assert_eq!(ev[0].project_key, "DEV");
    }

    // 12) 분류 필터: enabled set 에 없는 category 는 걸러지고, 빈 set 은 전부 통과
    #[test]
    fn filter_enabled_drops_disabled_categories() {
        let mk = |cat: &str| NotifEvent {
            event_uid: format!("X-1:{}", cat),
            issue_key: "X-1".to_string(),
            project_key: "X".to_string(),
            category: cat.to_string(),
            summary: "s".to_string(),
            detail: "d".to_string(),
            actor: "a".to_string(),
            event_at: "2026-07-21T10:00:00+09:00".to_string(),
        };
        let events = vec![mk("created"), mk("status"), mk("comment")];
        let mut set = HashSet::new();
        set.insert("created".to_string());
        set.insert("comment".to_string());
        let out = filter_enabled(events.clone(), &set);
        let cats: Vec<&str> = out.iter().map(|e| e.category.as_str()).collect();
        assert_eq!(cats, vec!["created", "comment"]);
        // 빈 set → 전부 통과(설정 없음=전부 on)
        assert_eq!(filter_enabled(events, &HashSet::new()).len(), 3);
    }

    // 13) my_issues_only + 담당자 타인: created/status/field 스킵, assignee·comment 유지
    #[test]
    fn my_issues_only_other_assignee_skips_owned_cats() {
        let issue = json!({
            "key": "APP-500",
            "fields": {
                "summary": "제목",
                "created": "2026-07-21T09:00:00.000+0900",
                "creator": { "accountId": OTHER, "displayName": "김창조" },
                "assignee": { "accountId": OTHER, "displayName": "박담당" }
            }
        });
        let status_h = json!({
            "id": "6001", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "status", "fromString": "할 일", "toString": "진행 중" }]
        });
        let assignee_h = json!({ // 담당자 변경(→타인) → assignee, 필터 미적용
            "id": "6002", "created": "2026-07-21T10:05:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "assignee", "to": OTHER, "fromString": "없음", "toString": "박담당" }]
        });
        let field_h = json!({
            "id": "6003", "created": "2026-07-21T10:10:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "priority", "fromString": "Medium", "toString": "High" }]
        });
        let ev = classify_app_issue(
            &issue,
            &[status_h, assignee_h, field_h],
            &[plain_comment()],
            since_all(),
            ME,
            true,
        );
        let cats: Vec<&str> = ev.iter().map(|e| e.category.as_str()).collect();
        assert!(!cats.contains(&"created"));
        assert!(!cats.contains(&"status"));
        assert!(!cats.contains(&"field"));
        assert!(cats.contains(&"assignee"));
        assert!(cats.contains(&"comment"));
        assert_eq!(ev.len(), 2);
    }

    // 14) my_issues_only + 담당자 나: created/status/field 정상 생성
    #[test]
    fn my_issues_only_my_assignee_keeps_owned_cats() {
        let issue = json!({
            "key": "APP-501",
            "fields": {
                "summary": "제목",
                "created": "2026-07-21T09:00:00.000+0900",
                "creator": { "accountId": OTHER, "displayName": "김창조" },
                "assignee": { "accountId": ME, "displayName": "이지수" }
            }
        });
        let status_h = json!({
            "id": "7001", "created": "2026-07-21T10:00:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "status", "fromString": "할 일", "toString": "진행 중" }]
        });
        let field_h = json!({
            "id": "7002", "created": "2026-07-21T10:10:00.000+0900",
            "author": { "accountId": OTHER, "displayName": "김창조" },
            "items": [{ "field": "priority", "fromString": "Medium", "toString": "High" }]
        });
        let ev = classify_app_issue(&issue, &[status_h, field_h], &[], since_all(), ME, true);
        let cats: Vec<&str> = ev.iter().map(|e| e.category.as_str()).collect();
        assert_eq!(ev.len(), 3);
        assert!(cats.contains(&"created"));
        assert!(cats.contains(&"status"));
        assert!(cats.contains(&"field"));
    }
}
