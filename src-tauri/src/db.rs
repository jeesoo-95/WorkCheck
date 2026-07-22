// SQLite 연결 · 마이그레이션 · 시드
//
// Holiday 시드는 2026년 한국 공휴일 데이터이며, 설정 화면에서 추가/삭제할 수 있다.
// (시드 데이터, 설정 화면에서 수정 가능)

use rusqlite::Connection;
use std::path::Path;

/// DB 열기 + 스키마 마이그레이션 + 최초 시드
pub fn open(db_path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    migrate(&conn)?;
    seed(&conn)?;
    Ok(conn)
}

/// 스키마 마이그레이션 — PRAGMA user_version 기반 버전 관리.
///
/// - v1: 기본 테이블(Task/CheckLog/Setting/Holiday). 버전 도입 이전 스키마이며,
///   기존 DB 와 신규 DB 모두 user_version=0 으로 시작한다(v1 로 간주).
/// - v2: CheckLog 에 status('done'|'skip') · memo 컬럼 추가 (소급 체크·스킵·완료 메모).
/// - v3: Task 에 notify_time('HH:MM') · remind_before(1~30) 컬럼 추가 (업무별 알림·사전 리마인드).
/// - v4: Task 에 priority(0=높음·1=보통·2=낮음) 컬럼 추가 (업무별 우선순위·우선순위 순 정렬).
/// - v5: JiraNotification 테이블 + 인덱스 추가 (M4 Jira 알림 피드·dedup).
///
/// 기존 v1 DB(컬럼 없음)와 신규 DB 모두 아래 CREATE(v1 기준) → v2 → v3 → v4 → v5 경로를 거친다.
fn migrate(conn: &Connection) -> Result<(), String> {
    // v1 기본 테이블 (버전 무관 idempotent)
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS Task (
            id          INTEGER PRIMARY KEY,
            name        TEXT NOT NULL,
            memo        TEXT,
            links       TEXT,
            recur_type  TEXT NOT NULL,
            recur_param TEXT,
            active      INTEGER DEFAULT 1,
            sort_order  INTEGER,
            created_at  TEXT
        );
        CREATE TABLE IF NOT EXISTS CheckLog (
            id          INTEGER PRIMARY KEY,
            task_id     INTEGER NOT NULL REFERENCES Task(id),
            due_date    TEXT NOT NULL,
            checked_at  TEXT NOT NULL,
            UNIQUE(task_id, due_date)
        );
        CREATE TABLE IF NOT EXISTS Setting (
            key   TEXT PRIMARY KEY,
            value TEXT
        );
        CREATE TABLE IF NOT EXISTS Holiday (
            date TEXT PRIMARY KEY,
            name TEXT
        );
        "#,
    )
    .map_err(|e| e.to_string())?;

    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    // v2: CheckLog.status / CheckLog.memo 추가. 기존 행은 DEFAULT 'done' 로 채워짐.
    if version < 2 {
        conn.execute_batch(
            r#"
            ALTER TABLE CheckLog ADD COLUMN status TEXT NOT NULL DEFAULT 'done';
            ALTER TABLE CheckLog ADD COLUMN memo TEXT;
            PRAGMA user_version = 2;
            "#,
        )
        .map_err(|e| e.to_string())?;
    }

    // v3: Task.notify_time / Task.remind_before 추가.
    //   notify_time  : null=개별 알림 없음, "HH:MM"=해당 시각 개별 알림
    //   remind_before: null=리마인드 없음, 1~30=기한 N일 전 예고
    if version < 3 {
        conn.execute_batch(
            r#"
            ALTER TABLE Task ADD COLUMN notify_time TEXT;
            ALTER TABLE Task ADD COLUMN remind_before INTEGER;
            PRAGMA user_version = 3;
            "#,
        )
        .map_err(|e| e.to_string())?;
    }

    // v4: Task.priority 추가 (0=높음, 1=보통, 2=낮음. 숫자 작을수록 우선).
    //   기존 행은 DEFAULT 1(보통)로 채워짐. load_tasks 정렬 키에 최우선으로 반영된다.
    if version < 4 {
        conn.execute_batch(
            r#"
            ALTER TABLE Task ADD COLUMN priority INTEGER NOT NULL DEFAULT 1;
            PRAGMA user_version = 4;
            "#,
        )
        .map_err(|e| e.to_string())?;
    }

    // v5: JiraNotification 테이블 + 인덱스 (M4 Jira 알림).
    //   event_uid UNIQUE 로 INSERT OR IGNORE dedup, idx 는 안읽음·최신순 조회용.
    if version < 5 {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS JiraNotification (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                event_uid   TEXT NOT NULL UNIQUE,
                issue_key   TEXT NOT NULL,
                project_key TEXT NOT NULL,
                category    TEXT NOT NULL,
                summary     TEXT NOT NULL,
                detail      TEXT NOT NULL,
                actor       TEXT NOT NULL,
                event_at    TEXT NOT NULL,
                fetched_at  TEXT NOT NULL,
                read        INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_jira_notif_read ON JiraNotification(read, event_at DESC);
            PRAGMA user_version = 5;
            "#,
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// 최초 실행 시 기본 설정값 + 2026 공휴일 시드
fn seed(conn: &Connection) -> Result<(), String> {
    // 기본 설정값 (M2: 알림 스케줄러·트레이 최소화에서 사용)
    let defaults = [
        ("notify_enabled", "1"),
        ("notify_time", "09:00"),
        ("notify_on_overdue", "1"),
        ("close_to_tray", "1"),
        ("theme", "system"),   // system(시스템 따름)|light|dark
        ("auto_backup", "1"),  // 1=앱 시작 시 자동 백업 1회
        // M3-B5: 전역 단축키 (빈 문자열=비활성). Setting 값으로 setup 에서 등록.
        ("hotkey_toggle", "Ctrl+Alt+W"), // 메인 창 표시/숨김 토글
        ("hotkey_quick", "Ctrl+Alt+A"),  // 빠른 추가 소형 창 열기
        // M4: Jira 알림 (기획서 6절). 토큰은 평문 저장(개인 PC 로컬 DB 한정).
        ("jira_enabled", "0"),                            // 연동 토글
        ("jira_base_url", "https://gmdsoft.atlassian.net"), // 사이트 URL
        ("jira_email", ""),                               // 계정 이메일
        ("jira_api_token", ""),                           // API 토큰 (UI 마스킹)
        ("jira_account_id", ""),                          // 연결 테스트 성공 시 자동 저장
        ("jira_project", "APP"),                          // 감시 프로젝트 키
        // 알림 받을 분류(CSV). 빈값이면 전부 on 으로 간주(jira.rs filter_enabled).
        ("jira_categories", "created,assignee,comment,mention,assigned"), // 기본: 상태·필드 변경 제외
        // 담당자가 나인 이슈만(생성·상태·필드 분류에만 적용). 기본 ON.
        ("jira_my_issues_only", "1"),
        ("jira_poll_secs", "180"),                        // 폴링 주기(초, 최소 60)
        ("jira_last_poll", ""),                           // 마지막 폴링 시각(RFC3339)
        ("jira_last_error", ""),                          // 마지막 폴링 오류(성공 시 빈값)
    ];
    for (k, v) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO Setting(key, value) VALUES(?1, ?2)",
            rusqlite::params![k, v],
        )
        .map_err(|e| e.to_string())?;
    }

    // 공휴일은 seed 플래그로 1회만 삽입 (이후 사용자 삭제분을 되살리지 않도록)
    let seeded: bool = conn
        .query_row(
            "SELECT 1 FROM Setting WHERE key='seed_holidays_2026'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !seeded {
        // 2026년 한국 공휴일 (시드 데이터, 설정 화면에서 수정 가능)
        let holidays = [
            ("2026-01-01", "신정"),
            ("2026-02-16", "설날 연휴"),
            ("2026-02-17", "설날"),
            ("2026-02-18", "설날 연휴"),
            ("2026-03-01", "삼일절"),
            ("2026-03-02", "삼일절 대체공휴일"),
            ("2026-05-05", "어린이날"),
            ("2026-05-24", "부처님오신날"),
            ("2026-05-25", "부처님오신날 대체공휴일"),
            ("2026-06-03", "전국동시지방선거"),
            ("2026-06-06", "현충일"),
            ("2026-08-15", "광복절"),
            ("2026-08-17", "광복절 대체공휴일"),
            ("2026-09-24", "추석 연휴"),
            ("2026-09-25", "추석"),
            ("2026-09-26", "추석 연휴"),
            ("2026-10-03", "개천절"),
            ("2026-10-05", "개천절 대체공휴일"),
            ("2026-10-09", "한글날"),
            ("2026-12-25", "성탄절"),
        ];
        for (date, name) in holidays {
            conn.execute(
                "INSERT OR IGNORE INTO Holiday(date, name) VALUES(?1, ?2)",
                rusqlite::params![date, name],
            )
            .map_err(|e| e.to_string())?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO Setting(key, value) VALUES('seed_holidays_2026', '1')",
            [],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// 테스트 전용: 타 모듈 단위 테스트(jira 등)에서 스키마를 준비할 때 사용.
#[cfg(test)]
pub(crate) fn migrate_for_test(conn: &Connection) {
    migrate(conn).unwrap();
}

// ── 단위 테스트 ──────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// CheckLog 에 status/memo 컬럼이 있는지 + user_version 확인
    fn has_columns(conn: &Connection) -> (bool, bool, i64) {
        let mut has_status = false;
        let mut has_memo = false;
        let mut stmt = conn.prepare("PRAGMA table_info(CheckLog)").unwrap();
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|c| c.unwrap())
            .collect::<Vec<_>>();
        for c in &cols {
            if c == "status" {
                has_status = true;
            }
            if c == "memo" {
                has_memo = true;
            }
        }
        let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        (has_status, has_memo, ver)
    }

    /// Task 에 notify_time/remind_before 컬럼이 있는지
    fn has_task_columns(conn: &Connection) -> (bool, bool) {
        let mut has_notify = false;
        let mut has_remind = false;
        let mut stmt = conn.prepare("PRAGMA table_info(Task)").unwrap();
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|c| c.unwrap())
            .collect::<Vec<_>>();
        for c in &cols {
            if c == "notify_time" {
                has_notify = true;
            }
            if c == "remind_before" {
                has_remind = true;
            }
        }
        (has_notify, has_remind)
    }

    /// Task 에 priority 컬럼이 있는지
    fn has_priority_column(conn: &Connection) -> bool {
        let mut stmt = conn.prepare("PRAGMA table_info(Task)").unwrap();
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|c| c.unwrap())
            .collect::<Vec<_>>();
        cols.iter().any(|c| c == "priority")
    }

    /// JiraNotification 테이블이 있는지 (v5)
    fn has_jira_table(conn: &Connection) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='JiraNotification'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false)
    }

    // 신규 DB: migrate 1회로 v2/v3/v4/v5 스키마 생성 + user_version=5
    #[test]
    fn migrate_fresh_db_to_v5() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert_eq!(has_columns(&conn), (true, true, 5));
        assert_eq!(has_task_columns(&conn), (true, true));
        assert!(has_priority_column(&conn));
        assert!(has_jira_table(&conn));
    }

    // 기존 v1 DB(컬럼 없음, user_version=0): migrate 로 v5 까지 자동 업그레이드
    #[test]
    fn migrate_v1_db_upgrades() {
        let conn = Connection::open_in_memory().unwrap();
        // v1 스키마 재현 (status/memo 없음, user_version 미설정=0)
        conn.execute_batch(
            r#"
            CREATE TABLE CheckLog (
                id          INTEGER PRIMARY KEY,
                task_id     INTEGER NOT NULL,
                due_date    TEXT NOT NULL,
                checked_at  TEXT NOT NULL,
                UNIQUE(task_id, due_date)
            );
            INSERT INTO CheckLog(task_id, due_date, checked_at) VALUES(1, '2026-07-01', '2026-07-01 09:00:00');
            "#,
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(has_columns(&conn), (true, true, 5));
        assert_eq!(has_task_columns(&conn), (true, true));
        assert!(has_priority_column(&conn));
        assert!(has_jira_table(&conn));
        // 기존 행은 DEFAULT 'done' 으로 채워짐
        let status: String = conn
            .query_row("SELECT status FROM CheckLog WHERE task_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "done");
    }

    // 기존 v2 DB(status/memo 있음, notify 없음): migrate 로 v3·v4·v5 스키마 추가
    #[test]
    fn migrate_v2_db_upgrades_to_v5() {
        let conn = Connection::open_in_memory().unwrap();
        // v1 CREATE → v2 ALTER 까지만 재현 (Task 는 notify 컬럼 없음)
        conn.execute_batch(
            r#"
            CREATE TABLE Task (
                id          INTEGER PRIMARY KEY,
                name        TEXT NOT NULL,
                recur_type  TEXT NOT NULL
            );
            CREATE TABLE CheckLog (
                id          INTEGER PRIMARY KEY,
                task_id     INTEGER NOT NULL,
                due_date    TEXT NOT NULL,
                checked_at  TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'done',
                memo        TEXT,
                UNIQUE(task_id, due_date)
            );
            PRAGMA user_version = 2;
            "#,
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(has_columns(&conn), (true, true, 5));
        assert_eq!(has_task_columns(&conn), (true, true));
        assert!(has_priority_column(&conn));
        assert!(has_jira_table(&conn));
    }

    // 기존 v3 DB(notify/remind 있음, priority 없음): migrate 로 v4 priority + v5 Jira 스키마 추가.
    // 기존 행은 DEFAULT 1(보통)로 채워진다.
    #[test]
    fn migrate_v3_db_upgrades_to_v5() {
        let conn = Connection::open_in_memory().unwrap();
        // v1 CREATE → v2/v3 ALTER 까지 재현 (Task 는 priority 컬럼 없음)
        conn.execute_batch(
            r#"
            CREATE TABLE Task (
                id            INTEGER PRIMARY KEY,
                name          TEXT NOT NULL,
                recur_type    TEXT NOT NULL,
                notify_time   TEXT,
                remind_before INTEGER
            );
            INSERT INTO Task(name, recur_type) VALUES('기존업무', 'daily');
            PRAGMA user_version = 3;
            "#,
        )
        .unwrap();
        migrate(&conn).unwrap();
        let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(ver, 5);
        assert!(has_priority_column(&conn));
        assert!(has_jira_table(&conn));
        // 기존 행은 DEFAULT 1(보통) 으로 채워짐
        let priority: i64 = conn
            .query_row("SELECT priority FROM Task WHERE name='기존업무'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(priority, 1);
    }

    // 재실행 idempotent: 이미 v5 인 DB 에 migrate 를 또 호출해도 안전
    #[test]
    fn migrate_idempotent_on_v5() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // 두 번째 호출은 ALTER/CREATE 건너뜀
        assert_eq!(has_columns(&conn), (true, true, 5));
        assert_eq!(has_task_columns(&conn), (true, true));
        assert!(has_priority_column(&conn));
        assert!(has_jira_table(&conn));
    }
}
