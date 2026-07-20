# 업무 체크 (WorkCheck)

개인 반복 업무(매일/매주/매월/매분기/연 1회) + 1회성(지정일) 업무 체크 관리 윈도우 앱. Tauri v2 + 바닐라 HTML/JS + SQLite(rusqlite).

- 기획서: `..\업무관리툴_기획\기획서_v1.md`
- 데이터: `%APPDATA%\com.jeesoo.workcheck\workcheck.db` (백업 = 이 파일 복사)

## 개발 실행

```
npm install        # 최초 1회
npm run dev        # 개발 모드 실행 (Rust 변경 시 자동 재빌드)
```

- 프론트(src/*.html/css/js) 수정 → 앱에서 F5 새로고침
- 백엔드(src-tauri/src/*.rs) 수정 → 자동 재빌드·재시작

## 배포 빌드

```
npm run build      # src-tauri\target\release\bundle\ 에 설치본(msi/nsis) 생성
```

## 테스트

```
cd src-tauri && cargo test    # 주기 계산(recur.rs) 단위 테스트
```

## 구조

| 파일 | 역할 |
|---|---|
| `src/` | 화면 (오늘/전체 업무/통계/설정 4탭) |
| `src-tauri/src/recur.rs` | 주기 → 기한일 계산 (평일만=주말+공휴일 제외, 말일 클램프, 1회성=지정일 하루) |
| `src-tauri/src/commands.rs` | invoke API: 오늘 뷰·체크 토글·CRUD·통계·설정·공휴일 |
| `src-tauri/src/db.rs` | 스키마 마이그레이션 + 기본 설정·2026 공휴일 시드 |

## 단축키 (설정 탭에서 변경 가능)

- `Ctrl+Alt+W` — 창 표시/숨김 토글
- `Ctrl+Alt+A` — 빠른 추가 (1회성 업무 즉시 등록)

## 로드맵

- M1 (완료): 업무 CRUD + 오늘 할 일 + 체크 + 통계 + 설정(공휴일 관리) + 1회성(지정일) 업무
- M2 (완료): 트레이 상주, 부팅 자동 시작, 토스트 알림
- M3 (완료): 스킵·완료 메모·소급 체크 / 업무별 알림·D-n 리마인드 / 링크 열기·드래그 정렬·다크 모드·중복 실행 방지 / 리포트·백업 / 전역 단축키·빠른 추가
- 보류: 미니 모드, 옵시디언 내보내기, Jira 가져오기 — 기획서 참조
