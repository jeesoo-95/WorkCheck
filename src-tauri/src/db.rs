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

/// 테이블 생성 (기획서 4장 스키마)
fn migrate(conn: &Connection) -> Result<(), String> {
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
    .map_err(|e| e.to_string())
}

/// 최초 실행 시 기본 설정값 + 2026 공휴일 시드
fn seed(conn: &Connection) -> Result<(), String> {
    // 기본 설정값 (M2: 알림 스케줄러·트레이 최소화에서 사용)
    let defaults = [
        ("notify_enabled", "1"),
        ("notify_time", "09:00"),
        ("notify_on_overdue", "1"),
        ("close_to_tray", "1"),
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
