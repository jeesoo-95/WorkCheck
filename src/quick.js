// 빠른 추가 소형 창 — 1회성(once) 업무를 빠르게 등록
// 백엔드 통신: window.__TAURI__.core.invoke (withGlobalTauri=true).
// 모든 창 제어/이벤트는 Rust 커맨드로 래핑 호출한다(JS 직접 플러그인 호출 금지 원칙).

function hasTauri() {
  return window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
}
async function invoke(cmd, args) {
  if (!hasTauri()) throw new Error('Tauri 런타임을 찾을 수 없습니다.');
  return window.__TAURI__.core.invoke(cmd, args || {});
}

// ── 요소 ──────────────────────────────────────────────
const nameEl = document.getElementById('q-name');
const dueEl = document.getElementById('q-due');
const dateEl = document.getElementById('q-date');
const errEl = document.getElementById('q-err');
const addBtn = document.getElementById('q-add');
const closeBtn = document.getElementById('q-close');

// ── 테마 (Setting theme 읽어 적용, main.js 와 동일 규칙) ──
function resolveTheme(pref) {
  if (pref === 'light' || pref === 'dark') return pref;
  return (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light';
}
async function loadTheme() {
  try {
    const settings = await invoke('get_settings');
    const t = (settings.find(s => s.key === 'theme') || {}).value || 'system';
    document.documentElement.dataset.theme = resolveTheme(t);
  } catch { document.documentElement.dataset.theme = resolveTheme('system'); }
}

// ── 날짜 유틸 ──────────────────────────────────────────
function fmtIso(d) {
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${d.getFullYear()}-${m}-${day}`;
}
function todayIso() { return fmtIso(new Date()); }
function tomorrowIso() { const d = new Date(); d.setDate(d.getDate() + 1); return fmtIso(d); }

// 선택된 기한(YYYY-MM-DD). "날짜 지정"은 date input 값(없으면 오늘).
function selectedDate() {
  switch (dueEl.value) {
    case 'tomorrow': return tomorrowIso();
    case 'date': return dateEl.value || todayIso();
    default: return todayIso();
  }
}

// 기한 select 변경 → 날짜 지정일 때만 date input 노출
dueEl.addEventListener('change', () => {
  if (dueEl.value === 'date') {
    dateEl.style.display = '';
    if (!dateEl.value) dateEl.value = todayIso();
    dateEl.focus();
  } else {
    dateEl.style.display = 'none';
  }
});

// ── 상태 초기화 · 포커스 ──────────────────────────────
function reset() {
  nameEl.value = '';
  dueEl.value = 'today';
  dateEl.style.display = 'none';
  dateEl.value = '';
  errEl.textContent = '';
}
function focusName() { setTimeout(() => nameEl.focus(), 20); }

// ── 창 숨김 (재사용) ──────────────────────────────────
async function hideWindow() {
  try { await invoke('hide_quick_window'); } catch { /* 무시 */ }
}

// ── 등록 ──────────────────────────────────────────────
async function submit() {
  const name = nameEl.value.trim();
  if (!name) { errEl.textContent = '업무 이름을 입력하세요.'; nameEl.focus(); return; }
  const date = selectedDate();
  const dto = {
    id: null,
    name,
    memo: null,
    links: '[]',
    recurType: 'once',
    recurParam: JSON.stringify({ date }),
    sortOrder: 0,
    notifyTime: null,
    remindBefore: null,
  };
  addBtn.disabled = true;
  try {
    await invoke('add_task', { dto });
    // 메인 창이 떠 있으면 오늘/전체 탭 자동 갱신 (Rust 가 main 웹뷰에 emit)
    try { await invoke('notify_task_added'); } catch { /* 메인 없음 등 무시 */ }
    reset();
    await hideWindow();
  } catch (e) {
    errEl.textContent = '등록 실패: ' + (e.message || e);
  } finally {
    addBtn.disabled = false;
  }
}

// ── 이벤트 바인딩 ─────────────────────────────────────
addBtn.addEventListener('click', submit);
closeBtn.addEventListener('click', hideWindow);

// Enter=등록, Esc=닫기 (문서 전역 — date input 위에서도 동작)
document.addEventListener('keydown', e => {
  if (e.key === 'Enter') { e.preventDefault(); submit(); }
  else if (e.key === 'Escape') { e.preventDefault(); hideWindow(); }
});

// 창 재표시(재사용) 시 이름 입력 재포커스 — 웹뷰가 유지되므로 focus 로 처리
window.addEventListener('focus', focusName);

// ── 시작 ──────────────────────────────────────────────
window.addEventListener('DOMContentLoaded', () => {
  loadTheme();
  focusName();
});
