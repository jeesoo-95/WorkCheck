// 업무 체크 — 프론트엔드 로직
// 백엔드 통신: window.__TAURI__.core.invoke (tauri.conf: withGlobalTauri=true)

const WEEKDAY_KO = ['일', '월', '화', '수', '목', '금', '토'];
/// 공휴일 처리 값 (recur.rs parse_holiday 와 동일)
const HOLIDAY_VALUES = ['none', 'skip', 'before', 'after'];

// ── invoke 래퍼 (에러 배너 처리) ──────────────────────────
function hasTauri() {
  return window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
}
async function invoke(cmd, args) {
  if (!hasTauri()) throw new Error('Tauri 런타임을 찾을 수 없습니다 (앱으로 실행하세요).');
  return window.__TAURI__.core.invoke(cmd, args || {});
}
function showError(msg, retryFn) {
  const b = document.getElementById('err-banner');
  document.getElementById('err-msg').textContent = '오류: ' + msg;
  b.classList.add('show');
  const btn = document.getElementById('err-retry');
  btn.onclick = () => { b.classList.remove('show'); if (retryFn) retryFn(); };
}
function clearError() { document.getElementById('err-banner').classList.remove('show'); }

// ── 테마(다크 모드) ──────────────────────────────────────
// pref: 'system'|'light'|'dark'. system 은 prefers-color-scheme 로 해석.
// <html data-theme> 를 항상 light/dark 로 확정해 style.css 오버라이드가 적용된다.
let themeMedia = null;
function resolveTheme(pref) {
  if (pref === 'light' || pref === 'dark') return pref;
  return (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light';
}
function applyTheme(pref) {
  document.documentElement.dataset.theme = resolveTheme(pref);
  // 시스템 따름일 때만 OS 테마 변경 리스너 유지, 그 외엔 해제
  if (themeMedia) { themeMedia.onchange = null; themeMedia = null; }
  if (pref === 'system' && window.matchMedia) {
    themeMedia = window.matchMedia('(prefers-color-scheme: dark)');
    themeMedia.onchange = () => { document.documentElement.dataset.theme = resolveTheme('system'); };
  }
}
async function loadTheme() {
  try {
    const settings = await invoke('get_settings');
    const t = (settings.find(s => s.key === 'theme') || {}).value || 'system';
    applyTheme(t);
  } catch { applyTheme('system'); }
}

// ── 유틸 ──────────────────────────────────────────────
function esc(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}
function pct(r) { return Math.round((r || 0) * 100); }
function fmtIso(d) {
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${d.getFullYear()}-${m}-${day}`;
}
function todayIso() { return fmtIso(new Date()); }
// 1 이상 정수로 정규화. 값이 없거나 잘못되면 def.
function intOr(v, def) {
  const n = Number(v);
  return Number.isFinite(n) && n >= 1 ? Math.trunc(n) : def;
}
function parseLinks(raw) {
  if (!raw) return [];
  try { const a = JSON.parse(raw); return Array.isArray(a) ? a : []; } catch { return []; }
}
async function copyToClipboard(text) {
  try {
    if (navigator.clipboard) await navigator.clipboard.writeText(text);
    else { const t = document.createElement('textarea'); t.value = text; document.body.appendChild(t); t.select(); document.execCommand('copy'); t.remove(); }
    return true;
  } catch { return false; }
}

// ── 탭 전환 ──────────────────────────────────────────────
const loaders = {
  'p-today': loadToday,
  'p-manage': loadManage,
  'p-stats': loadStats,
  'p-jira': loadJira,
  'p-settings': loadSettings,
};
document.querySelectorAll('.tab').forEach(t => t.addEventListener('click', () => {
  document.querySelectorAll('.tab').forEach(x => x.classList.remove('active'));
  document.querySelectorAll('.page').forEach(x => x.classList.remove('active'));
  t.classList.add('active');
  document.getElementById(t.dataset.page).classList.add('active');
  const fn = loaders[t.dataset.page];
  if (fn) fn();
}));

// 우선순위 배지 HTML. 높음(0)=빨강 계열 테두리형, 낮음(2)=회색, 보통(1)=배지 없음.
function priorityBadgeHtml(priority) {
  if (priority === 0) return '<span class="badge prio-high">높음</span>';
  if (priority === 2) return '<span class="badge prio-low">낮음</span>';
  return '';
}

// ── 업무 행 렌더 (오늘 탭) ──────────────────────────────
function taskRowHtml(occ, opts) {
  opts = opts || {};
  const links = parseLinks(occ.links);
  const hasDetail = !!(occ.memo || links.length);
  const badges = [];
  // 우선순위(높음/낮음) 배지를 먼저 표시 — 밀림 D+n·건너뜀 배지와 공존
  const prio = priorityBadgeHtml(occ.priority);
  if (prio) badges.push(prio);
  // 건너뜀 상태는 이름 옆 회색 배지로 먼저 표시
  if (occ.status === 'skip') badges.push('<span class="badge skip">건너뜀</span>');
  // 예정 라벨이 있으면 규칙 배지와 내용이 겹치므로 예정 라벨만 표시
  if (!occ.upcomingLabel) badges.push(`<span class="badge">${esc(occ.ruleLabel)}</span>`);
  if (occ.daysLate > 0) badges.push(`<span class="badge late">D+${occ.daysLate}</span>`);
  if (occ.upcomingLabel) badges.push(`<span class="badge">${esc(occ.upcomingLabel)}</span>`);

  const cls = ['task'];
  if (occ.checked) cls.push('done');
  if (occ.status === 'skip') cls.push('skip');
  if (opts.overdue) cls.push('overdue');
  if (opts.upcoming) cls.push('upcoming');

  const chkDisabled = opts.upcoming ? ' disabled' : '';
  const clip = hasDetail ? '<span class="clip">📎</span>' : '';

  return `<div class="${cls.join(' ')}" data-task-id="${occ.taskId}" data-due="${esc(occ.dueDate)}" data-status="${esc(occ.status)}" data-upcoming="${opts.upcoming ? 1 : 0}">
    <div class="task-row" tabindex="0" role="button" aria-label="${esc(occ.name)}">
      <div class="chk${chkDisabled}">✓</div>
      <div class="task-name">${esc(occ.name)}</div>
      ${clip}
      ${badges.join('')}
    </div>
    <div class="task-detail">${detailHtml(occ, links, opts)}</div>
  </div>`;
}
function detailHtml(occ, links, opts) {
  opts = opts || {};
  // 1) 업무 메모/링크
  let h;
  if (!occ.memo && !links.length) {
    h = '<span style="color:var(--txt-faint)">메모 없음 — 전체 업무 탭에서 수정할 수 있습니다</span>';
  } else {
    h = occ.memo ? '📝 ' + esc(occ.memo) : '';
    if (links.length) {
      h += '<div class="links">' + links.map((l) =>
        `<a data-url="${esc(l.url || '')}" title="클릭=열기 · Shift+클릭=복사">🔗 ${esc(l.title || l.url || '링크')}</a>`
      ).join('') + '</div>';
    }
  }
  // 2) 회차 제어 (오늘 탭 실제 회차만 — 다가오는 업무 제외)
  if (!opts.upcoming) {
    let ctl = '<div class="chk-controls">';
    if (occ.status === 'skip') {
      ctl += '<button class="btn-mini btn-skip-off">건너뛰기 해제</button>';
    } else {
      ctl += '<button class="btn-mini btn-skip">건너뛰기</button>';
    }
    // 완료(done) 회차에만 완료 메모 입력칸
    if (occ.status === 'done') {
      ctl += `<span class="memo-line"><input class="check-memo" type="text" placeholder="완료 메모 (선택)" value="${esc(occ.checkMemo || '')}"><span class="memo-ok">✓ 저장됨</span></span>`;
    }
    ctl += '</div>';
    h += ctl;
  }
  return h;
}

// 업무 행 이벤트 위임 (오늘 탭 컨테이너들)
function wireTaskEvents(container) {
  container.querySelectorAll('.task').forEach(task => {
    const row = task.querySelector('.task-row');
    const isUpcoming = task.dataset.upcoming === '1';
    const chk = task.querySelector('.chk');

    if (!isUpcoming) {
      chk.addEventListener('click', e => { e.stopPropagation(); doToggle(task); });
    }
    row.addEventListener('click', () => task.classList.toggle('open'));
    row.addEventListener('keydown', e => {
      if (e.key === ' ') { e.preventDefault(); if (!isUpcoming) doToggle(task); }
      if (e.key === 'Enter') { e.preventDefault(); task.classList.toggle('open'); }
    });

    // 건너뛰기 / 건너뛰기 해제
    const skipBtn = task.querySelector('.btn-skip');
    if (skipBtn) skipBtn.addEventListener('click', e => { e.stopPropagation(); doSetStatus(task, 'skip'); });
    const skipOff = task.querySelector('.btn-skip-off');
    if (skipOff) skipOff.addEventListener('click', e => { e.stopPropagation(); doSetStatus(task, 'none'); });

    // 완료 메모 입력 (Enter 또는 blur 시 저장)
    const memoInput = task.querySelector('.check-memo');
    if (memoInput) {
      memoInput.addEventListener('click', e => e.stopPropagation());
      memoInput.addEventListener('keydown', e => {
        e.stopPropagation();
        if (e.key === 'Enter') { e.preventDefault(); memoInput.blur(); }
      });
      memoInput.addEventListener('blur', () => saveCheckMemo(task, memoInput));
    }
  });
  // 링크 클릭 → 브라우저로 열기. Shift+클릭 → 클립보드 복사(기존 동작 유지)
  container.querySelectorAll('.task-detail a').forEach(a => {
    a.addEventListener('click', async e => {
      e.stopPropagation();
      const url = a.dataset.url;
      if (!url) return;
      if (e.shiftKey) {
        const ok = await copyToClipboard(url);
        const orig = a.textContent;
        a.textContent = ok ? '✓ 복사됨' : '복사 실패';
        setTimeout(() => { a.textContent = orig; }, 1200);
      } else {
        try { await invoke('open_link', { url }); clearError(); }
        catch (err) { showError(err.message || err, null); }
      }
    });
  });
}
async function doToggle(task) {
  const taskId = Number(task.dataset.taskId);
  const due = task.dataset.due;
  try {
    // 건너뜀 상태에서 체크박스 클릭 → 완료(done)로 전환.
    // 그 외(none/done)는 기존 토글(none↔done) 유지.
    if (task.dataset.status === 'skip') {
      await invoke('set_check_status', { taskId, dueDate: due, status: 'done', memo: null });
    } else {
      await invoke('toggle_check', { taskId, dueDate: due });
    }
    clearError();
    loadToday();
  } catch (e) { showError(e.message || e, () => doToggle(task)); }
}
// 회차 상태 설정 (건너뛰기/해제) 후 오늘 탭 새로고침
async function doSetStatus(task, status) {
  const taskId = Number(task.dataset.taskId);
  const due = task.dataset.due;
  try {
    await invoke('set_check_status', { taskId, dueDate: due, status, memo: null });
    clearError();
    loadToday();
  } catch (e) { showError(e.message || e, () => doSetStatus(task, status)); }
}
// 완료 메모 저장 (변경 시에만). 저장되면 ✓ 잠깐 표시.
async function saveCheckMemo(task, input) {
  if (input.value === input.defaultValue) return; // 변경 없음 → 저장 생략
  const taskId = Number(task.dataset.taskId);
  const due = task.dataset.due;
  try {
    await invoke('set_check_memo', { taskId, dueDate: due, memo: input.value });
    clearError();
    input.defaultValue = input.value; // 다음 blur 중복 저장 방지
    const ok = input.nextElementSibling; // .memo-ok
    if (ok) { ok.classList.add('show'); setTimeout(() => ok.classList.remove('show'), 1400); }
  } catch (e) { showError(e.message || e, null); }
}

// ── 오늘 탭 ──────────────────────────────────────────────
async function loadToday() {
  let v;
  try { v = await invoke('get_today_view'); clearError(); }
  catch (e) { showError(e.message || e, loadToday); return; }

  // 날짜 헤더
  const dt = new Date(v.date + 'T00:00:00');
  document.getElementById('today-date').textContent =
    `${dt.getMonth() + 1}월 ${dt.getDate()}일 (${WEEKDAY_KO[dt.getDay()]})`;

  // 요약 (스킵 회차는 분모에서 제외 — 수행률 계산과 동일 기준)
  const skipped = v.today.filter(t => t.status === 'skip').length;
  const total = v.today.length - skipped;
  const done = v.today.filter(t => t.status === 'done').length;
  const late = v.overdue.length;
  document.getElementById('summary').innerHTML =
    `오늘 <b>${total}건 중 ${done}건 완료</b>` +
    (skipped ? ` · 건너뜀 ${skipped}건` : '') +
    (late ? ` · <span class="overdue-cnt">밀림 ${late}건</span>` : (total || skipped ? ' · 밀림 없음 👍' : ''));

  // 밀림
  const secOver = document.getElementById('sec-overdue');
  const listOver = document.getElementById('list-overdue');
  if (v.overdue.length) {
    secOver.style.display = '';
    listOver.innerHTML = v.overdue.map(o => taskRowHtml(o, { overdue: true })).join('');
    wireTaskEvents(listOver);
  } else { secOver.style.display = 'none'; listOver.innerHTML = ''; }

  // 오늘
  const listToday = document.getElementById('list-today');
  if (v.today.length) {
    listToday.innerHTML = v.today.map(o => taskRowHtml(o, {})).join('');
  } else {
    listToday.innerHTML = '<div class="empty">오늘 할 일이 없습니다. <a data-goto="p-manage">＋ 업무 추가</a>로 등록하세요.</div>';
    listToday.querySelector('[data-goto]')?.addEventListener('click', () => switchTo('p-manage'));
  }
  wireTaskEvents(listToday);

  // 다가오는
  const secUp = document.getElementById('sec-upcoming');
  const listUp = document.getElementById('list-upcoming');
  if (v.upcoming.length) {
    secUp.style.display = '';
    listUp.innerHTML = v.upcoming.map(o => taskRowHtml(o, { upcoming: true })).join('');
    wireTaskEvents(listUp);
  } else { secUp.style.display = 'none'; listUp.innerHTML = ''; }

  // 주간 수행률
  const p = pct(v.weekRate);
  document.getElementById('wp-pct').textContent = p + '%';
  document.getElementById('wp-bar').style.width = p + '%';
}
function switchTo(pageId) {
  document.querySelector(`.tab[data-page="${pageId}"]`).click();
}

// ── 전체 업무 탭 ──────────────────────────────────────────
const GROUPS = [
  { type: 'once', label: '1회성' },
  { type: 'daily', label: '매일' },
  { type: 'weekly', label: '매주' },
  { type: 'monthly', label: '매월' },
  { type: 'quarterly', label: '매분기' },
  { type: 'yearly', label: '연 1회' },
];
// 접힌(collapsed) 그룹 type 집합. Setting 키 'collapsed_groups' 에 JSON 배열로 저장/복원.
let collapsedGroups = new Set();
// get_settings 결과에서 접힌 그룹 목록을 읽는다. 값 없음/파싱 실패 시 빈 배열(모두 펼침).
function readCollapsedGroups(settings) {
  const raw = (settings.find(s => s.key === 'collapsed_groups') || {}).value;
  if (!raw) return [];
  try { const a = JSON.parse(raw); return Array.isArray(a) ? a : []; } catch { return []; }
}
// 접힘 상태 저장 — 사소한 UI 상태이므로 실패해도 조용히 무시(에러 배너 없음).
function persistCollapsed() {
  invoke('set_setting', { key: 'collapsed_groups', value: JSON.stringify([...collapsedGroups]) })
    .catch(() => {});
}
// 그룹 헤더 접기/펼치기 토글. 헤더 다음 형제(.group-body) 표시를 전환하고 상태를 저장.
function toggleGroup(header) {
  const type = header.dataset.groupType;
  const body = header.nextElementSibling; // .group-body
  const nowCollapsed = !header.classList.contains('collapsed');
  header.classList.toggle('collapsed', nowCollapsed);
  header.setAttribute('aria-expanded', nowCollapsed ? 'false' : 'true');
  if (body) body.style.display = nowCollapsed ? 'none' : '';
  if (nowCollapsed) collapsedGroups.add(type); else collapsedGroups.delete(type);
  persistCollapsed();
}
async function loadManage() {
  let tasks;
  try { tasks = await invoke('list_tasks'); clearError(); }
  catch (e) { showError(e.message || e, loadManage); return; }

  // 접힌 그룹 상태 로드 (UI 상태 → 실패해도 조용히 무시: 모두 펼침으로 진행)
  try {
    const settings = await invoke('get_settings');
    collapsedGroups = new Set(readCollapsedGroups(settings));
  } catch { /* 무시 */ }

  const wrap = document.getElementById('manage-groups');
  wrap.innerHTML = GROUPS.map(g => {
    const items = tasks.filter(t => t.recurType === g.type);
    if (!items.length) {
      // 빈 그룹: 큰 카드 대신 한 줄로 압축(접기 대상 아님). 우측에 '+ 추가' 링크.
      return `<div class="group-label empty-line">${g.label} (0) <span class="none-note">— 없음</span>` +
             `<a class="add-here" role="button" tabindex="0" data-group-type="${g.type}">＋ 추가</a></div>`;
    }
    const collapsed = collapsedGroups.has(g.type);
    // 1회성 그룹만: 완료(doneOnce) 업무는 기본 숨김
    const hidden = g.type === 'once' ? items.filter(t => t.doneOnce) : [];
    const visible = g.type === 'once' ? items.filter(t => !t.doneOnce) : items;
    let body = visible.map(t => mTaskRow(t, false)).join('');
    if (hidden.length) {
      // 숨겨진 완료 1회성: 접힌 컨테이너 + 펼침 링크
      body += `<div class="done-once-wrap" style="display:none">${hidden.map(t => mTaskRow(t, true)).join('')}</div>`;
      body += `<a class="show-done" data-count="${hidden.length}">완료된 1회성 ${hidden.length}건 보기</a>`;
    }
    // 접기 가능한 헤더(캐럿 + 라벨) + 접힘 시 숨기는 .group-body 컨테이너
    return `<div class="group-label collapsible${collapsed ? ' collapsed' : ''}" data-group-type="${g.type}"` +
           ` role="button" tabindex="0" aria-expanded="${collapsed ? 'false' : 'true'}">` +
           `<span class="caret" aria-hidden="true">▾</span>${g.label} (${items.length})</div>` +
           `<div class="group-body"${collapsed ? ' style="display:none"' : ''}>${body}</div>`;
  }).join('');

  // 이벤트 바인딩
  wrap.querySelectorAll('.m-task').forEach(row => {
    const id = Number(row.dataset.id);
    const task = tasks.find(t => t.id === id);
    const editBtn = row.querySelector('.edit'); // 완료 1회성 행에는 없음
    if (editBtn) editBtn.addEventListener('click', () => openModal(task));
    row.querySelector('.del').addEventListener('click', () => confirmDelete(task));
  });
  // 그룹 접기/펼치기 (비어있지 않은 그룹만 .collapsible). 클릭 + Enter/Space 지원.
  wrap.querySelectorAll('.group-label.collapsible').forEach(h => {
    h.addEventListener('click', () => toggleGroup(h));
    h.addEventListener('keydown', e => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleGroup(h); }
    });
  });
  // 완료 1회성 펼침/접힘 토글 (탭 재진입 시 loadManage 재실행으로 다시 접힘)
  wrap.querySelectorAll('.show-done').forEach(link => {
    const count = Number(link.dataset.count);
    link.addEventListener('click', () => {
      const w = link.previousElementSibling; // .done-once-wrap
      const open = w.style.display === 'none';
      w.style.display = open ? '' : 'none';
      link.textContent = `완료된 1회성 ${count}건 ${open ? '숨기기' : '보기'}`;
    });
  });
  wrap.querySelectorAll('.add-here').forEach(a => {
    const preset = a.dataset.groupType || null; // 빈 그룹의 '+추가'는 그 주기를 프리셋으로
    a.addEventListener('click', () => openModal(null, preset));
    a.addEventListener('keydown', e => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openModal(null, preset); }
    });
  });

  // 드래그 순서 정렬 바인딩 (같은 주기 그룹 안에서만) — .group-body 내부 행에도 그대로 적용
  wireDrag(wrap);
}

// ── 드래그 순서 정렬 (전체 업무 탭) ──────────────────────────
// 같은 주기 그룹의 행끼리만 재정렬. 드롭 시 그룹의 표시 순서를 set_sort_order 로 저장.
let dragEl = null;
function wireDrag(wrap) {
  wrap.querySelectorAll('.m-task[draggable="true"]').forEach(row => {
    row.addEventListener('dragstart', e => {
      dragEl = row;
      row.classList.add('dragging');
      if (e.dataTransfer) { e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', row.dataset.id); }
    });
    row.addEventListener('dragend', () => {
      row.classList.remove('dragging');
      dragEl = null;
    });
    row.addEventListener('dragover', e => {
      // 같은 주기 그룹 + 같은 우선순위의 다른 행 위에서만 삽입 위치 미리보기.
      // 다른 우선순위 행 위로는 드롭을 막는다(preventDefault 생략 → 드롭 불가).
      if (!dragEl || dragEl === row) return;
      if (dragEl.dataset.group !== row.dataset.group) return;
      if (dragEl.dataset.priority !== row.dataset.priority) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
      const rect = row.getBoundingClientRect();
      const before = (e.clientY - rect.top) < rect.height / 2;
      row.parentNode.insertBefore(dragEl, before ? row : row.nextSibling);
    });
    row.addEventListener('drop', async e => {
      if (!dragEl) return;
      if (dragEl.dataset.group !== row.dataset.group) return;
      if (dragEl.dataset.priority !== row.dataset.priority) return;
      e.preventDefault();
      await persistOrder(wrap, row.dataset.group, row.dataset.priority);
    });
  });
}
// 해당 그룹+우선순위의 현재 DOM 순서를 읽어 sort_order 로 저장. 실패 시 재로딩으로 원복.
// (같은 priority 형제들의 새 순서만 저장 — 다른 우선순위 항목은 건드리지 않음)
async function persistOrder(wrap, group, priority) {
  const ids = [...wrap.querySelectorAll(`.m-task[data-group="${group}"][data-priority="${priority}"]`)]
    .map(el => Number(el.dataset.id));
  try { await invoke('set_sort_order', { ids }); clearError(); }
  catch (e) { showError(e.message || e, null); loadManage(); }
}

// 전체 업무 탭의 업무 행 (done=true 는 완료된 1회성: 흐리게 + 완료 배지 + 수정 숨김)
// 완료 1회성(done)은 드래그 대상 제외 — draggable·data-group 미부여.
function mTaskRow(t, done) {
  const prio = (t.priority != null ? t.priority : 1);
  // 드래그 정렬은 "같은 주기 그룹 + 같은 우선순위"끼리만 → data-priority 로 구분
  const drag = done ? '' : ` draggable="true" data-group="${esc(t.recurType)}" data-priority="${prio}"`;
  return `<div class="m-task${done ? ' done' : ''}" data-id="${t.id}"${drag}>
           <div class="task-name">${esc(t.name)}</div>
           ${priorityBadgeHtml(prio)}
           <span class="rule">${esc(t.ruleLabel)}</span>
           ${t.notifyTime ? `<span class="notify-note">🔔 ${esc(t.notifyTime)}</span>` : ''}
           ${t.remindBefore ? `<span class="badge remind">D-${t.remindBefore} 예고</span>` : ''}
           ${done ? '<span class="badge">완료</span>' : ''}
           <div class="actions">
             ${done ? '' : '<button class="edit">수정</button>'}
             <button class="del">삭제</button>
           </div>
         </div>`;
}
// recur_param 파싱. 파싱 실패는 물론 "null"·"123" 같은 비객체 JSON 도 빈 객체로 (recur.rs param_value 와 동일)
function safeParam(raw) {
  try {
    const v = raw ? JSON.parse(raw) : {};
    return (typeof v === 'object' && v !== null) ? v : {};
  } catch { return {}; }
}
// recur_param 의 공휴일 처리 값. holiday 키가 없으면 레거시 weekdaysOnly:true 를 skip 으로 읽는다.
// (recur.rs parse_holiday 와 동일 — 알 수 없는 holiday 값은 none)
function paramHoliday(p) {
  if (p.holiday !== undefined) return HOLIDAY_VALUES.includes(p.holiday) ? p.holiday : 'none';
  return p.weekdaysOnly ? 'skip' : 'none';
}

async function confirmDelete(task) {
  if (!confirm(`"${task.name}" 업무를 삭제할까요?\n체크 이력도 함께 삭제됩니다.`)) return;
  try { await invoke('delete_task', { id: task.id }); clearError(); loadManage(); }
  catch (e) { showError(e.message || e, () => confirmDelete(task)); }
}

// ── 모달 (추가/수정) ──────────────────────────────────────
const modalBack = document.getElementById('modal-back');
const RECUR_KINDS = ['once', 'daily', 'weekly', 'monthly', 'quarterly', 'yearly'];

// 매월 '날짜 지정' 1~31 일자 칩 생성 (최초 1회)
function buildMonthDayChips() {
  const wrap = document.getElementById('f-month-days');
  if (!wrap || wrap.childElementCount) return;
  let html = '';
  for (let d = 1; d <= 31; d++) {
    html += `<button type="button" class="mday" data-day="${d}" aria-pressed="false">${d}</button>`;
  }
  wrap.innerHTML = html;
}
buildMonthDayChips();

// 토글 버튼(요일·일자) 공통 처리 — 선택값 읽기/쓰기
function toggleChip(btn) {
  const on = btn.classList.toggle('on');
  btn.setAttribute('aria-pressed', on ? 'true' : 'false');
}
function chipValues(wrapId, attr) {
  return Array.from(document.getElementById(wrapId).children)
    .filter(b => b.classList.contains('on'))
    .map(b => Number(b.dataset[attr]))
    .sort((a, b) => a - b);
}
function setChipValues(wrapId, attr, values) {
  const set = new Set((values || []).map(Number));
  Array.from(document.getElementById(wrapId).children).forEach(b => {
    const on = set.has(Number(b.dataset[attr]));
    b.classList.toggle('on', on);
    b.setAttribute('aria-pressed', on ? 'true' : 'false');
  });
}
// 숫자 입력값 (1 이상 정수, 아니면 def)
function numField(id, def) { return intOr(document.getElementById(id).value, def); }
// 매월 지정 방식 ('days' | 'nth')
function monthMode() { return document.querySelector('input[name="f-month-mode"]:checked').value; }

// 주기별 파라미터 블록 표시 전환
function showParamFields(type) {
  RECUR_KINDS.forEach(k => {
    document.getElementById('p-' + k).style.display = (k === type) ? '' : 'none';
  });
  // 매일 업무는 사전 예고가 무의미 → "N일 전 미리 알림" 입력 숨김
  document.getElementById('f-remind-wrap').style.display = (type === 'daily') ? 'none' : '';
  syncRecurFields();
}

// 선택 상태에 따른 조건부 입력 노출 (매월 모드 · 종료 상세 · 시작일)
function syncRecurFields() {
  const type = document.getElementById('f-recur-type').value;
  // 매월: 날짜 지정 / 요일 지정
  const mode = monthMode();
  document.getElementById('p-month-days').style.display = (mode === 'days') ? '' : 'none';
  document.getElementById('p-month-nth').style.display = (mode === 'nth') ? '' : 'none';
  // 종료 조건 — 1회성은 무의미하므로 영역 자체를 숨긴다
  const isOnce = (type === 'once');
  const endMode = document.getElementById('f-end-mode').value;
  document.getElementById('p-end').style.display = isOnce ? 'none' : '';
  document.getElementById('p-until').style.display = (!isOnce && endMode === 'until') ? '' : 'none';
  document.getElementById('p-count').style.display = (!isOnce && endMode === 'count') ? '' : 'none';
  // 시작일 — 간격>1(매일·매주) 이거나 'N회' 종료일 때만 필요(백엔드가 위상·회차 기준으로 사용)
  const interval = (type === 'daily') ? numField('f-daily-interval', 1)
    : (type === 'weekly') ? numField('f-weekly-interval', 1) : 1;
  const needStart = (interval > 1) || (!isOnce && endMode === 'count');
  document.getElementById('p-start').style.display = needStart ? '' : 'none';
  const startInput = document.getElementById('f-recur-start');
  if (needStart && !startInput.value) startInput.value = todayIso(); // 기준일 없으면 간격·N회가 무시됨
}

// 주기 입력 변화 → 조건부 노출 갱신 + 미리보기(디바운스)
function onRecurChanged() { syncRecurFields(); schedulePreview(); }
const recurArea = document.getElementById('recur-area');
recurArea.addEventListener('input', onRecurChanged);
recurArea.addEventListener('change', onRecurChanged);
recurArea.addEventListener('click', e => {
  const btn = e.target.closest('.wd, .mday');
  if (!btn) return;
  toggleChip(btn);
  onRecurChanged();
});
document.getElementById('f-recur-type').addEventListener('change', e => {
  showParamFields(e.target.value);
  schedulePreview();
});

// ── 주기 미리보기 (요약 문장 + 다음 발생일) ────────────────
// 요약은 백엔드 rule_summary 문자열을 그대로 출력한다(라벨 단일 소스).
let previewTimer = null;
let previewSeq = 0;
function schedulePreview() {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(runPreview, 250); // 연속 입력은 마지막 것만 조회
}
function setSaveEnabled(ok) { document.getElementById('modal-save').disabled = !ok; }
// "YYYY-MM-DD" → "MM-DD(요일)"
function fmtPreviewDay(iso) {
  const d = new Date(iso + 'T00:00:00');
  return iso.slice(5) + (isNaN(d) ? '' : `(${WEEKDAY_KO[d.getDay()]})`);
}
async function runPreview() {
  clearTimeout(previewTimer);
  const type = document.getElementById('f-recur-type').value;
  const card = document.getElementById('recur-preview');
  const seq = ++previewSeq;
  let r;
  try {
    r = await invoke('preview_recur', { recurType: type, recurParam: JSON.stringify(buildRecurParam(type)) });
  } catch (e) {
    if (seq !== previewSeq) return;
    // 미리보기 호출 실패는 규칙 오류가 아니므로 저장은 막지 않는다
    card.classList.add('err');
    document.getElementById('rp-summary').textContent = '미리보기를 불러오지 못했습니다.';
    document.getElementById('rp-next').textContent = String(e.message || e);
    setSaveEnabled(true);
    return;
  }
  if (seq !== previewSeq) return; // 늦게 도착한 이전 응답 무시
  const next = r.next || [];
  card.classList.toggle('err', !!r.error);
  document.getElementById('rp-summary').textContent = r.summary || '';
  document.getElementById('rp-next').textContent = r.error
    ? r.error
    : (next.length > 1 ? `다음 ${next.length}회: ` : '다음: ') + next.map(fmtPreviewDay).join(' · ');
  setSaveEnabled(!r.error); // 생성 날짜가 0개면 저장 불가
}

// 레거시 recur_param 을 신규 형식으로 변환해 폼에 채운다 (저장은 항상 신규 형식)
//   daily   {"weekdaysOnly":true} → 공휴일 처리 '건너뜀'
//   weekly  {"weekday":5}         → 요일 토글 [금]
//   monthly {"day":10}            → 날짜 지정 모드, 10일 선택
function fillRecurForm(type, p) {
  // 1회성
  document.getElementById('f-once-date').value = p.date || todayIso();
  // 매일 / 매주 — 간격
  document.getElementById('f-daily-interval').value = intOr(p.interval, 1);
  document.getElementById('f-weekly-interval').value = intOr(p.interval, 1);
  // 매주 — 요일 (레거시 weekday 단일값 폴백)
  let wds = Array.isArray(p.weekdays)
    ? p.weekdays.map(Number).filter(w => w >= 0 && w <= 6)
    : (type === 'weekly' && p.weekday != null ? [Number(p.weekday)] : []);
  if (!wds.length) wds = [1]; // 미선택이면 월요일 기본
  setChipValues('f-weekdays', 'wd', wds);
  // 매월 — 지정 방식 + 일자/주차·요일 (레거시 day 단일값 폴백)
  const isNth = (type === 'monthly' && p.mode === 'nth');
  document.querySelector(`input[name="f-month-mode"][value="${isNth ? 'nth' : 'days'}"]`).checked = true;
  let days = Array.isArray(p.days)
    ? p.days.map(d => intOr(d, 1)).filter(d => d >= 1 && d <= 31)
    : (type === 'monthly' && !isNth && p.day != null ? [intOr(p.day, 1)] : []);
  if (!days.length) days = [1];
  setChipValues('f-month-days', 'day', days);
  document.getElementById('f-month-lastday').checked = !!p.lastDay;
  // 주차 select 는 1~4·-1 만 제공 → 그 외 값은 '첫째'로
  const nthRaw = isNth ? String(p.nth ?? 1) : '1';
  document.getElementById('f-month-nth').value = ['1', '2', '3', '4', '-1'].includes(nthRaw) ? nthRaw : '1';
  document.getElementById('f-month-weekday').value = String(isNth ? (p.weekday ?? 1) : 1);
  // 매분기 / 연1회 (기존 유지)
  document.getElementById('f-moq').value = String(p.monthOfQuarter ?? 1);
  document.getElementById('f-q-day').value = p.day ?? 1;
  document.getElementById('f-y-month').value = p.month ?? 1;
  document.getElementById('f-y-day').value = p.day ?? 1;
  // 공통 — 공휴일 처리 · 시작일 · 종료 조건
  document.getElementById('f-holiday').value = paramHoliday(p);
  document.getElementById('f-recur-start').value = typeof p.start === 'string' ? p.start : '';
  // until 과 count 가 함께 있으면 select 특성상 until 우선
  document.getElementById('f-end-mode').value = p.until ? 'until' : (p.count ? 'count' : 'none');
  document.getElementById('f-until').value = typeof p.until === 'string' ? p.until : '';
  document.getElementById('f-count').value = intOr(p.count, 10);
}

function openModal(task, presetType) {
  document.getElementById('modal-title').textContent = task ? '업무 수정' : '업무 추가';
  document.getElementById('f-id').value = task ? task.id : '';
  document.getElementById('f-name').value = task ? task.name : '';
  document.getElementById('f-memo').value = task && task.memo ? task.memo : '';
  // 링크 → 텍스트 (title|url)
  const links = task ? parseLinks(task.links) : [];
  document.getElementById('f-links').value = links.map(l => (l.title ? l.title + '|' : '') + (l.url || '')).join('\n');

  // 신규 추가 시 presetType(빈 그룹 '+추가'가 넘긴 주기)이 있으면 그 주기로 시작
  const type = task ? task.recurType : (presetType || 'daily');
  document.getElementById('f-recur-type').value = type;
  const p = task ? safeParam(task.recurParam) : {};
  fillRecurForm(type, p);
  showParamFields(type);   // 내부에서 syncRecurFields() 로 조건부 입력 노출까지 맞춘다
  setSaveEnabled(true);    // 직전 모달의 오류 상태가 남지 않도록 초기화
  runPreview();

  // 우선순위 (없으면 보통=1)
  document.getElementById('f-priority').value = String(task && task.priority != null ? task.priority : 1);

  // 알림 설정 (선택)
  document.getElementById('f-notify-time').value = task && task.notifyTime ? task.notifyTime : '';
  document.getElementById('f-remind-before').value = task && task.remindBefore ? task.remindBefore : '';

  modalBack.classList.add('show');
  setTimeout(() => document.getElementById('f-name').focus(), 30);
}
function closeModal() { clearTimeout(previewTimer); modalBack.classList.remove('show'); }
document.getElementById('btn-add-task').addEventListener('click', () => openModal(null));
document.getElementById('modal-cancel').addEventListener('click', closeModal);
modalBack.addEventListener('click', e => { if (e.target === modalBack) closeModal(); });

// UI → recur_param JSON (항상 신규 형식으로 저장)
function buildRecurParam(type) {
  const p = {};
  switch (type) {
    case 'once':
      p.date = document.getElementById('f-once-date').value;
      break;
    case 'daily':
      p.interval = numField('f-daily-interval', 1);
      break;
    case 'weekly':
      p.weekdays = chipValues('f-weekdays', 'wd');
      p.interval = numField('f-weekly-interval', 1);
      break;
    case 'monthly':
      if (monthMode() === 'nth') {
        p.mode = 'nth';
        p.nth = Number(document.getElementById('f-month-nth').value);
        p.weekday = Number(document.getElementById('f-month-weekday').value);
      } else {
        p.mode = 'days';
        p.days = chipValues('f-month-days', 'day');
        p.lastDay = document.getElementById('f-month-lastday').checked;
      }
      break;
    case 'quarterly':
      p.monthOfQuarter = Number(document.getElementById('f-moq').value);
      p.day = Number(document.getElementById('f-q-day').value);
      break;
    case 'yearly':
      p.month = Number(document.getElementById('f-y-month').value);
      p.day = Number(document.getElementById('f-y-day').value);
      break;
    default: return p;
  }
  return applyCommonParam(p, type);
}
// 공통 필드(공휴일·시작일·종료) 부착. 화면에 노출된 입력만 반영한다.
function applyCommonParam(p, type) {
  p.holiday = document.getElementById('f-holiday').value;
  if (document.getElementById('p-start').style.display !== 'none') {
    const start = document.getElementById('f-recur-start').value;
    if (start) p.start = start;
  }
  if (type === 'once') return p; // 1회성은 종료 조건 무의미
  const endMode = document.getElementById('f-end-mode').value;
  if (endMode === 'until') {
    const until = document.getElementById('f-until').value;
    if (until) p.until = until;
  } else if (endMode === 'count') {
    p.count = numField('f-count', 1);
  }
  return p;
}
function buildLinks() {
  const raw = document.getElementById('f-links').value.trim();
  if (!raw) return '[]';
  const arr = raw.split('\n').map(line => line.trim()).filter(Boolean).map(line => {
    const i = line.indexOf('|');
    if (i >= 0) return { title: line.slice(0, i).trim(), url: line.slice(i + 1).trim() };
    return { title: line, url: line };
  });
  return JSON.stringify(arr);
}
document.getElementById('modal-save').addEventListener('click', async () => {
  const name = document.getElementById('f-name').value.trim();
  if (!name) { alert('업무 이름을 입력하세요.'); document.getElementById('f-name').focus(); return; }
  const type = document.getElementById('f-recur-type').value;
  const idVal = document.getElementById('f-id').value;
  // 알림 (선택): 빈 값 → null. 매일 업무는 사전 예고 무의미 → remindBefore 강제 null.
  const notifyTime = document.getElementById('f-notify-time').value || null;
  const remindRaw = document.getElementById('f-remind-before').value;
  const remindBefore = (type === 'daily' || !remindRaw) ? null : Number(remindRaw);
  const dto = {
    id: idVal ? Number(idVal) : null,
    name,
    memo: document.getElementById('f-memo').value.trim() || null,
    links: buildLinks(),
    recurType: type,
    recurParam: JSON.stringify(buildRecurParam(type)),
    sortOrder: 0,
    notifyTime,
    remindBefore,
    priority: Number(document.getElementById('f-priority').value),
  };
  try {
    if (dto.id) await invoke('update_task', { dto });
    else await invoke('add_task', { dto });
    clearError();
    closeModal();
    loadManage();
  } catch (e) { showError(e.message || e, null); }
});

// ── 통계 탭 ──────────────────────────────────────────────
async function loadStats() {
  let s, today;
  try {
    [s, today] = await Promise.all([invoke('get_stats'), invoke('get_today_view')]);
    clearError();
  } catch (e) { showError(e.message || e, loadStats); return; }

  document.getElementById('st-streak').textContent = s.streakDays + '일';
  document.getElementById('st-month').textContent = pct(s.monthRate) + '%';
  document.getElementById('st-overdue').textContent = today.overdue.length + '건';

  setRate('st-week', s.weekRate);
  setRate('st-month', s.monthRate);
  setRate('st-quarter', s.quarterRate);

  // 히트맵
  const todayStr = today.date; // 백엔드 기준 오늘 (YYYY-MM-DD)
  const heat = document.getElementById('heat');
  heat.innerHTML = '';
  WEEKDAY_KO.forEach(d => heat.insertAdjacentHTML('beforeend', `<div class="cell dow">${d}</div>`));
  if (s.heatmap.length) {
    const first = new Date(s.heatmap[0].date + 'T00:00:00');
    for (let i = 0; i < first.getDay(); i++) heat.insertAdjacentHTML('beforeend', '<div class="cell" style="background:none"></div>');
    s.heatmap.forEach(c => {
      const day = Number(c.date.slice(8, 10));
      const skipped = c.skipped || 0;
      const hasOcc = (c.total + skipped) > 0;      // skip 만 있는 날도 회차 있음
      const isPast = c.date <= todayStr;
      let cls = '';
      if (c.date > todayStr) cls = 'future';       // 미래 날짜는 중립 표시
      else if (c.total === 0) cls = '';            // 회차 없음 또는 전부 skip → 중립(회색)
      else if (c.done === 0) cls = 'miss';
      else {
        const r = c.done / c.total;
        cls = r >= 1 ? 'c3' : (r >= 0.5 ? 'c2' : 'c1');
      }
      // 과거·오늘이면서 회차가 있는 날만 소급 체크 가능
      const clickable = isPast && hasOcc;
      if (clickable) cls += ' clickable';
      heat.insertAdjacentHTML('beforeend', `<div class="cell ${cls.trim()}" data-date="${c.date}"${clickable ? ' title="클릭하면 소급 체크"' : ''}>${day}</div>`);
    });
    // 소급 체크: 클릭 가능한 셀 → 회차 모달
    heat.querySelectorAll('.cell.clickable').forEach(cell => {
      cell.addEventListener('click', () => openDayModal(cell.dataset.date));
    });
  }
}
function setRate(prefix, r) {
  const p = pct(r);
  document.getElementById(prefix + '-bar').style.width = p + '%';
  document.getElementById(prefix + '-rv').textContent = p + '%';
}

// ── 리포트 생성 모달 ──────────────────────────────────────
// 프리셋 → [from, to] (YYYY-MM-DD). 미래는 백엔드가 오늘로 클램프한다.
function reportRange(preset) {
  const now = new Date(); now.setHours(0, 0, 0, 0);
  const dow = (now.getDay() + 6) % 7; // 월=0 기준 (백엔드 week_start 와 동일)
  const monday = new Date(now); monday.setDate(now.getDate() - dow);
  let from, to;
  switch (preset) {
    case 'lastWeek': {
      from = new Date(monday); from.setDate(monday.getDate() - 7);
      to = new Date(from); to.setDate(from.getDate() + 6);
      break;
    }
    case 'thisMonth':
      from = new Date(now.getFullYear(), now.getMonth(), 1); to = now; break;
    case 'lastMonth':
      from = new Date(now.getFullYear(), now.getMonth() - 1, 1);
      to = new Date(now.getFullYear(), now.getMonth(), 0); break; // 지난달 말일
    case 'thisWeek':
    default:
      from = monday; to = now; break;
  }
  return { from: fmtIso(from), to: fmtIso(to) };
}
const reportModalBack = document.getElementById('report-modal-back');
function closeReportModal() { reportModalBack.classList.remove('show'); }
async function refreshReport() {
  const preset = document.getElementById('report-preset').value;
  const { from, to } = reportRange(preset);
  const ta = document.getElementById('report-preview');
  try {
    ta.value = await invoke('generate_report', { from, to });
    clearError();
  } catch (e) { showError(e.message || e, null); }
}
document.getElementById('btn-report').addEventListener('click', () => {
  reportModalBack.classList.add('show');
  refreshReport();
});
document.getElementById('report-preset').addEventListener('change', refreshReport);
document.getElementById('report-close').addEventListener('click', closeReportModal);
reportModalBack.addEventListener('click', e => { if (e.target === reportModalBack) closeReportModal(); });
document.getElementById('report-copy').addEventListener('click', async () => {
  const ok = await copyToClipboard(document.getElementById('report-preview').value);
  const fb = document.getElementById('report-copy-ok');
  if (ok) { fb.classList.add('show'); setTimeout(() => fb.classList.remove('show'), 1500); }
  else showError('클립보드 복사에 실패했습니다', null);
});

// ── 소급 체크(회차) 모달 ──────────────────────────────────
const dayModalBack = document.getElementById('day-modal-back');
function closeDayModal() { dayModalBack.classList.remove('show'); }
document.getElementById('day-modal-close').addEventListener('click', closeDayModal);
dayModalBack.addEventListener('click', e => { if (e.target === dayModalBack) closeDayModal(); });

async function openDayModal(date) {
  let occ;
  try { occ = await invoke('get_day_view', { date }); clearError(); }
  catch (e) { showError(e.message || e, null); return; }
  document.getElementById('day-modal-title').textContent = `${date} 회차`;
  renderDayModal(date, occ);
  dayModalBack.classList.add('show');
}
function dayBadge(status) {
  if (status === 'done') return '<span class="badge done">완료</span>';
  if (status === 'skip') return '<span class="badge skip">건너뜀</span>';
  return '<span class="badge">미완료</span>';
}
function renderDayModal(date, occ) {
  const body = document.getElementById('day-modal-body');
  if (!occ.length) {
    body.innerHTML = '<div class="empty">이 날짜에 예정된 회차가 없습니다.</div>';
    return;
  }
  body.innerHTML = occ.map(o => `
    <div class="day-occ" data-task-id="${o.taskId}" data-due="${esc(o.dueDate)}">
      <div class="day-occ-head"><span class="task-name">${esc(o.name)}</span>${dayBadge(o.status)}</div>
      <div class="day-occ-btns">
        <button class="dob" data-st="done"${o.status === 'done' ? ' data-active="1"' : ''}>완료</button>
        <button class="dob" data-st="skip"${o.status === 'skip' ? ' data-active="1"' : ''}>건너뜀</button>
        <button class="dob" data-st="none"${o.status === 'none' ? ' data-active="1"' : ''}>해제</button>
      </div>
    </div>`).join('');

  body.querySelectorAll('.day-occ').forEach(row => {
    const taskId = Number(row.dataset.taskId);
    const due = row.dataset.due;
    row.querySelectorAll('.dob').forEach(btn => {
      btn.addEventListener('click', async () => {
        try {
          await invoke('set_check_status', { taskId, dueDate: due, status: btn.dataset.st, memo: null });
          clearError();
          const fresh = await invoke('get_day_view', { date }); // 모달 목록 갱신
          renderDayModal(date, fresh);
          loadStats(); // 통계·히트맵 갱신
        } catch (e) { showError(e.message || e, null); }
      });
    });
  });
}

// ── Jira 알림 탭 ──────────────────────────────────────────
// 필터 칩 정의. 'all' 은 전체(카테고리 미적용).
const JIRA_CATS = [
  { k: 'all', label: '전체' },
  { k: 'created', label: '생성' },
  { k: 'status', label: '상태' },
  { k: 'assignee', label: '담당자' },
  { k: 'field', label: '필드' },
  { k: 'comment', label: '댓글' },
  { k: 'mention', label: '멘션' },
  { k: 'assigned', label: '내 담당' },
];
const JIRA_CAT_LABEL = { created: '생성', status: '상태', assignee: '담당자', field: '필드', comment: '댓글', mention: '멘션', assigned: '내 담당' };

let jiraFilter = 'all';      // 현재 카테고리 필터
let jiraProject = 'all';     // 현재 프로젝트 필터 ('all'=전체)
let jiraOnlyUnread = false;  // 안읽음만 토글
let jiraQuery = '';          // 피드 검색어 (이슈키·내용·작성자 부분일치)
let jiraBaseUrl = '';        // 행 클릭 시 브라우저 열기용 (loadJira 에서 갱신)

// 상대 시각 표기 (RFC3339 → "방금/N분 전/N시간 전/N일 전/MM/DD")
function relTime(iso) {
  const t = new Date(iso);
  if (isNaN(t.getTime())) return iso || '';
  const diff = Math.floor((Date.now() - t.getTime()) / 1000);
  if (diff < 60) return '방금';
  if (diff < 3600) return Math.floor(diff / 60) + '분 전';
  if (diff < 86400) return Math.floor(diff / 3600) + '시간 전';
  if (diff < 86400 * 7) return Math.floor(diff / 86400) + '일 전';
  return `${t.getMonth() + 1}/${t.getDate()}`;
}

// 탭 배지 갱신 (안읽음 수). 0이면 숨김.
function setJiraBadge(count) {
  const b = document.getElementById('jira-tab-badge');
  if (!b) return;
  if (count > 0) { b.textContent = count; b.style.display = ''; }
  else b.style.display = 'none';
}
async function updateJiraBadge() {
  try { setJiraBadge(await invoke('get_jira_unread_count')); } catch { /* 조용히 무시 */ }
}

// 필터 칩 렌더 + 클릭 바인딩
function renderJiraFilters() {
  const wrap = document.getElementById('jira-filters');
  wrap.innerHTML = JIRA_CATS.map(c =>
    `<button class="jchip${jiraFilter === c.k ? ' active' : ''}" data-cat="${c.k}">${c.label}</button>`
  ).join('');
  wrap.querySelectorAll('.jchip').forEach(chip => {
    chip.addEventListener('click', () => { jiraFilter = chip.dataset.cat; loadJira(); });
  });
}

// 프로젝트 칩 렌더 + 클릭 바인딩. counts: [{projectKey, unread, total}] (project_key 오름차순).
// - [전체] + 프로젝트별 칩. 배지 = 프로젝트별 안읽음 수(0이면 배지 숨김, 칩은 표시).
// - 프로젝트가 1개 이하면 칩 줄 자체를 숨겨 공간 절약.
// - 선택된 프로젝트가 목록에서 사라지면 'all' 로 폴백(호출부에서 project 전달 전에 반영됨).
function renderJiraProjects(counts) {
  const wrap = document.getElementById('jira-projects');
  if (!wrap) return;
  const keys = counts.map(c => c.projectKey);
  if (jiraProject !== 'all' && !keys.includes(jiraProject)) jiraProject = 'all';
  // 프로젝트 1개 이하 → 칩 줄 숨김 (필터 의미 없음)
  if (counts.length <= 1) { wrap.style.display = 'none'; wrap.innerHTML = ''; return; }
  wrap.style.display = '';
  const chips = [{ k: 'all', label: '전체', unread: 0 }]
    .concat(counts.map(c => ({ k: c.projectKey, label: c.projectKey, unread: c.unread })));
  wrap.innerHTML = chips.map(c => {
    const active = jiraProject === c.k ? ' active' : '';
    const badge = (c.k !== 'all' && c.unread > 0) ? `<span class="jchip-badge">${c.unread}</span>` : '';
    return `<button class="jchip${active}" data-proj="${esc(c.k)}" aria-pressed="${jiraProject === c.k}">${esc(c.label)}${badge}</button>`;
  }).join('');
  wrap.querySelectorAll('.jchip').forEach(chip => {
    chip.addEventListener('click', () => { jiraProject = chip.dataset.proj; loadJira(); });
  });
}

// 알림 행 HTML
function jiraRowHtml(n) {
  const unread = n.read === 0;
  return `<div class="jira-item${unread ? ' unread' : ''}" data-id="${n.id}" data-key="${esc(n.issueKey)}" tabindex="0" role="button">
    <span class="jbadge jcat-${esc(n.category)}">${JIRA_CAT_LABEL[n.category] || esc(n.category)}</span>
    <div class="jira-item-body">
      <div class="jira-item-top"><span class="jkey">${esc(n.issueKey)}</span><span class="jsummary">${esc(n.summary)}</span></div>
      <div class="jira-item-detail">${esc(n.detail)}</div>
      <div class="jira-item-meta">${esc(n.actor)} · ${relTime(n.eventAt)}</div>
    </div>
    <button type="button" class="jread-dot${unread ? ' unread' : ''}" data-read="${unread ? '0' : '1'}" title="${unread ? '읽음 처리' : '안읽음 처리'}" aria-label="${unread ? '읽음 처리' : '안읽음 처리'}"></button>
  </div>`;
}

async function loadJira() {
  let settings;
  try { settings = await invoke('get_settings'); clearError(); }
  catch (e) { showError(e.message || e, loadJira); return; }
  const map = {};
  settings.forEach(s => map[s.key] = s.value);
  jiraBaseUrl = (map.jira_base_url || '').replace(/\/+$/, '');

  const configured = !!(map.jira_api_token && map.jira_email && map.jira_base_url);
  const enabled = map.jira_enabled === '1';
  const feed = document.getElementById('jira-feed');
  const toolbar = document.getElementById('jira-toolbar');
  const actions = document.getElementById('jira-actions');
  const status = document.getElementById('jira-status');

  // 미설정/비활성 → 안내 + 설정 이동 버튼
  if (!configured || !enabled) {
    toolbar.style.display = 'none';
    actions.style.display = 'none';
    status.textContent = '';
    const msg = !configured
      ? 'Jira 연동이 설정되지 않았습니다. 설정 탭에서 URL·이메일·API 토큰을 입력하세요.'
      : 'Jira 알림이 꺼져 있습니다. 설정 탭에서 "Jira 알림 사용"을 켜세요.';
    feed.innerHTML = `<div class="empty">${msg}<br><a data-goto="p-settings">설정으로 이동</a></div>`;
    feed.querySelector('[data-goto]')?.addEventListener('click', () => switchTo('p-settings'));
    updateJiraBadge();
    return;
  }

  toolbar.style.display = '';
  actions.style.display = '';
  renderJiraFilters();
  document.getElementById('jira-only-unread').checked = jiraOnlyUnread;
  // 검색어 입력값 동기화 (커서 튐 방지 위해 다를 때만 반영)
  const searchBox = document.getElementById('jira-search');
  if (searchBox && searchBox.value !== jiraQuery) searchBox.value = jiraQuery;

  // 상태줄: 마지막 폴링·오류
  if (map.jira_last_error) {
    status.innerHTML = `<span class="jira-err">⚠ 폴링 오류: ${esc(map.jira_last_error)}</span>`;
  } else if (map.jira_last_poll) {
    status.textContent = `마지막 폴링: ${relTime(map.jira_last_poll)}`;
  } else {
    status.textContent = '아직 폴링 이력이 없습니다.';
  }

  // 프로젝트 칩: 프로젝트별 안읽음/전체 수를 조회해 동적 렌더 (실패해도 피드는 동작).
  // renderJiraProjects 내부에서 사라진 선택 프로젝트를 'all' 로 폴백하므로 피드 조회보다 먼저 호출.
  let projCounts = [];
  try { projCounts = await invoke('get_jira_project_counts'); }
  catch { /* 조용히 무시 — 프로젝트 칩 없이도 피드는 표시 */ }
  renderJiraProjects(projCounts);

  // 피드 조회 (event_at DESC, 50개)
  let rows;
  try {
    rows = await invoke('get_jira_notifications', {
      onlyUnread: jiraOnlyUnread,
      category: jiraFilter,
      project: jiraProject,
      query: jiraQuery,
      limit: 50,
      offset: 0,
    });
    clearError();
  } catch (e) { showError(e.message || e, loadJira); return; }

  if (!rows.length) {
    const emptyMsg = jiraQuery.trim() ? '검색 결과 없음' : '표시할 알림이 없습니다.';
    feed.innerHTML = `<div class="empty">${emptyMsg}</div>`;
  } else {
    feed.innerHTML = rows.map(jiraRowHtml).join('');
    feed.querySelectorAll('.jira-item').forEach(item => {
      const open = () => openJiraItem(item);
      item.addEventListener('click', open);
      item.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); open(); } });
      // 상태 점 버튼: 행 클릭(브라우저 열기)과 분리해 read 만 토글
      const dot = item.querySelector('.jread-dot');
      dot?.addEventListener('click', e => { e.stopPropagation(); toggleJiraRead(item, dot); });
    });
  }
  updateJiraBadge();
}

// 상태 점 버튼 → read 토글. data-read '0'(안읽음)이면 읽음으로, '1'(읽음)이면 안읽음으로.
// 토글 후 loadJira 재호출로 피드·배지 갱신(안읽음만 필터 시 읽은 행은 목록에서 빠짐).
async function toggleJiraRead(item, dot) {
  const id = Number(item.dataset.id);
  const makeRead = dot.dataset.read === '0';
  try {
    await invoke('set_jira_read', { id, read: makeRead });
    clearError();
  } catch (e) { showError(e.message || e, null); return; }
  loadJira();
}

// 행 클릭 → 읽음 처리 + 브라우저로 이슈 열기
async function openJiraItem(item) {
  const id = Number(item.dataset.id);
  const key = item.dataset.key;
  try {
    await invoke('mark_jira_read', { ids: [id] });
    item.classList.remove('unread');
    updateJiraBadge();
    if (jiraBaseUrl && key) await invoke('open_link', { url: `${jiraBaseUrl}/browse/${key}` });
    clearError();
    // 안읽음만 보기 중이면 방금 읽은 항목이 목록에서 빠지도록 갱신
    if (jiraOnlyUnread) loadJira();
  } catch (e) { showError(e.message || e, null); }
}

// 새로고침 (수동 폴링)
document.getElementById('jira-refresh').addEventListener('click', async () => {
  const status = document.getElementById('jira-status');
  status.textContent = '새로고침 중…';
  try {
    const r = await invoke('jira_poll_now');
    if (r.error) showError(r.error, null); else clearError();
  } catch (e) { showError(e.message || e, null); }
  loadJira();
});
// 전체 읽음
document.getElementById('jira-read-all').addEventListener('click', async () => {
  try { await invoke('mark_all_jira_read'); clearError(); } catch (e) { showError(e.message || e, null); }
  loadJira();
});
// 안읽음만 토글
document.getElementById('jira-only-unread').addEventListener('change', e => {
  jiraOnlyUnread = e.target.checked;
  loadJira();
});
// 피드 검색 (입력 디바운스 250ms → loadJira 재조회). type=search 의 X 클릭도 input 이벤트로 처리.
let jiraSearchTimer = null;
const jiraSearchEl = document.getElementById('jira-search');
jiraSearchEl.addEventListener('input', e => {
  jiraQuery = e.target.value;
  clearTimeout(jiraSearchTimer);
  jiraSearchTimer = setTimeout(loadJira, 250);
});
// Esc → 검색어 비우고 즉시 전체 복귀
jiraSearchEl.addEventListener('keydown', e => {
  if (e.key === 'Escape' && jiraQuery) {
    e.preventDefault();
    jiraQuery = '';
    e.target.value = '';
    clearTimeout(jiraSearchTimer);
    loadJira();
  }
});

// jira-updated 이벤트: 백엔드 폴링이 새 알림을 넣으면 배지·피드 갱신
function wireJiraEvent() {
  if (window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen) {
    window.__TAURI__.event.listen('jira-updated', e => {
      setJiraBadge(e.payload || 0);
      const active = document.querySelector('.tab.active');
      if (active && active.dataset.page === 'p-jira') loadJira();
    });
  }
}

// ── 설정 탭 ──────────────────────────────────────────────
async function loadSettings() {
  let settings, holidays;
  try {
    [settings, holidays] = await Promise.all([invoke('get_settings'), invoke('list_holidays')]);
    clearError();
  } catch (e) { showError(e.message || e, loadSettings); return; }

  const map = {};
  settings.forEach(s => map[s.key] = s.value);
  document.getElementById('set-theme').value = map.theme || 'system';
  document.getElementById('set-notify-enabled').checked = map.notify_enabled === '1';
  document.getElementById('set-notify-time').value = map.notify_time || '09:00';
  document.getElementById('set-notify-overdue').checked = map.notify_on_overdue === '1';
  document.getElementById('set-close-to-tray').checked = map.close_to_tray !== '0'; // 기본 1
  document.getElementById('set-auto-backup').checked = map.auto_backup !== '0';     // 기본 1

  // 단축키 (Setting 값 그대로 표기, 빈 값=비활성)
  document.getElementById('hk-toggle').value = map.hotkey_toggle || '';
  document.getElementById('hk-quick').value = map.hotkey_quick || '';

  // Jira 연동 (토큰은 저장돼 있으면 자리표시 마스킹만, 실제 값은 노출 안 함)
  document.getElementById('jira-enabled').checked = map.jira_enabled === '1';
  document.getElementById('jira-base-url').value = map.jira_base_url || '';
  document.getElementById('jira-email').value = map.jira_email || '';
  const tokenInput = document.getElementById('jira-api-token');
  tokenInput.value = '';
  tokenInput.placeholder = map.jira_api_token ? '••••••••(저장됨) — 변경 시 새 토큰 입력' : '토큰 입력';
  document.getElementById('jira-project').value = map.jira_project || 'APP';
  document.getElementById('jira-poll-secs').value = map.jira_poll_secs || '180';
  // 담당자가 나인 이슈만 (값 없으면 기본 ON)
  document.getElementById('jira-my-issues-only').checked = map.jira_my_issues_only !== '0';
  // 윈도우 토스트 알림 (값 없으면 기본 ON)
  document.getElementById('jira-toast').checked = map.jira_toast !== '0';
  // 내 댓글도 알림 (값 없으면 기본 ON)
  document.getElementById('jira-include-my-comments').checked = map.jira_include_my_comments !== '0';
  // 알림 받을 분류 (CSV, 값 없음=전부 on). 체크 상태로 반영.
  const catSet = map.jira_categories
    ? new Set(map.jira_categories.split(',').map(s => s.trim()).filter(Boolean))
    : null;
  document.querySelectorAll('.jira-cat').forEach(cb => {
    cb.checked = catSet ? catSet.has(cb.value) : true;
  });

  // 자동 시작 상태는 플러그인에서 조회 (Setting 아님)
  try { document.getElementById('set-autostart').checked = await invoke('get_autostart'); }
  catch (e) { /* 조회 실패 시 토글 기본값 유지 */ }

  // 공휴일 목록
  const list = document.getElementById('hol-list');
  if (holidays.length) {
    list.innerHTML = holidays.map(h =>
      `<div class="hol-row" data-date="${esc(h.date)}">
         <span class="hol-date">${esc(h.date)}</span>
         <span class="hol-name">${esc(h.name)}</span>
         <button class="del">삭제</button>
       </div>`
    ).join('');
    list.querySelectorAll('.hol-row').forEach(row => {
      row.querySelector('.del').addEventListener('click', () => deleteHoliday(row.dataset.date));
    });
  } else {
    list.innerHTML = '<div class="empty">등록된 공휴일이 없습니다.</div>';
  }
}
async function saveSetting(key, value) {
  try { await invoke('set_setting', { key, value }); clearError(); }
  catch (e) { showError(e.message || e, null); }
}
// 테마 변경: 저장 + 즉시 적용
document.getElementById('set-theme').addEventListener('change', e => {
  saveSetting('theme', e.target.value);
  applyTheme(e.target.value);
});
document.getElementById('set-notify-enabled').addEventListener('change', e => saveSetting('notify_enabled', e.target.checked ? '1' : '0'));
document.getElementById('set-notify-overdue').addEventListener('change', e => saveSetting('notify_on_overdue', e.target.checked ? '1' : '0'));
document.getElementById('set-notify-time').addEventListener('change', e => saveSetting('notify_time', e.target.value));
document.getElementById('set-close-to-tray').addEventListener('change', e => saveSetting('close_to_tray', e.target.checked ? '1' : '0'));
document.getElementById('set-auto-backup').addEventListener('change', e => saveSetting('auto_backup', e.target.checked ? '1' : '0'));

// 지금 백업 → 저장 경로 표시
document.getElementById('set-backup-now').addEventListener('click', async () => {
  const msg = document.getElementById('backup-msg');
  try {
    const path = await invoke('backup_now');
    clearError();
    msg.textContent = '✓ 백업 완료: ' + path;
    msg.classList.add('show');
  } catch (e) { showError(e.message || e, null); }
});

// 백업에서 복원 → 파일 선택 dialog → 복원 → 즉시 리로드
document.getElementById('set-restore').addEventListener('click', async () => {
  const msg = document.getElementById('backup-msg');
  try {
    const path = await invoke('restore_backup');
    clearError();
    msg.textContent = '✓ 복원 완료 (' + path + '). 앱을 다시 시작하면 완전히 적용됩니다.';
    msg.classList.add('show');
    loadToday();     // 오늘 탭 즉시 반영
    loadSettings();  // 설정·공휴일도 복원본 기준으로 갱신
  } catch (e) {
    if ((e.message || e) === '취소됨') return; // 사용자가 취소 → 조용히 무시
    showError(e.message || e, null);
  }
});

// 자동 시작 토글 (플러그인 커맨드 경유). 실패 시 토글 원복.
document.getElementById('set-autostart').addEventListener('change', async e => {
  try { await invoke('set_autostart', { enable: e.target.checked }); clearError(); }
  catch (err) { showError(err.message || err, null); e.target.checked = !e.target.checked; }
});

// 단축키 적용 — 두 단축키를 각각 등록(set_hotkey). 실패한 항목만 사유 표시.
// 성공 항목은 즉시 반영·저장되고, 실패 항목은 기존 단축키가 유지된다(백엔드 규약).
document.getElementById('hk-apply').addEventListener('click', async () => {
  const msg = document.getElementById('hk-msg');
  const toggle = document.getElementById('hk-toggle').value.trim();
  const quick = document.getElementById('hk-quick').value.trim();
  const errs = [];
  try { await invoke('set_hotkey', { kind: 'toggle', accel: toggle }); }
  catch (e) { errs.push('창 토글: ' + (e.message || e)); }
  try { await invoke('set_hotkey', { kind: 'quick', accel: quick }); }
  catch (e) { errs.push('빠른 추가: ' + (e.message || e)); }
  if (errs.length) {
    msg.textContent = '✕ ' + errs.join(' / ');
    msg.className = 'hk-msg err show';
  } else {
    msg.textContent = '✓ 적용됨';
    msg.className = 'hk-msg ok show';
    clearError();
  }
});

// 알림 테스트 → 즉시 토스트 발송
document.getElementById('set-notify-test').addEventListener('click', async () => {
  const ok = document.getElementById('set-notify-test-ok');
  try {
    await invoke('test_notification');
    clearError();
    ok.classList.add('show');
    setTimeout(() => ok.classList.remove('show'), 1500);
  } catch (e) { showError(e.message || e, null); }
});

// ── Jira 연동 설정 저장/테스트 ────────────────────────────
// 토큰 입력칸이 비어 있으면(마스킹 상태 그대로) 기존 저장값을 덮어쓰지 않는다.
document.getElementById('jira-enabled').addEventListener('change', e => saveSetting('jira_enabled', e.target.checked ? '1' : '0'));
document.getElementById('jira-base-url').addEventListener('change', e => saveSetting('jira_base_url', e.target.value.trim()));
document.getElementById('jira-email').addEventListener('change', e => saveSetting('jira_email', e.target.value.trim()));
document.getElementById('jira-project').addEventListener('change', e => saveSetting('jira_project', e.target.value.trim() || 'APP'));
document.getElementById('jira-poll-secs').addEventListener('change', e => {
  const n = Math.max(60, Number(e.target.value) || 180);
  e.target.value = n; // 최소 60초로 보정 표시
  saveSetting('jira_poll_secs', String(n));
});
document.getElementById('jira-api-token').addEventListener('change', e => {
  const v = e.target.value.trim();
  if (v) saveSetting('jira_api_token', v); // 빈칸이면 기존 토큰 유지
});
// 담당자가 나인 이슈만 토글 (created·status·field 분류에만 적용)
document.getElementById('jira-my-issues-only').addEventListener('change', e =>
  saveSetting('jira_my_issues_only', e.target.checked ? '1' : '0'));
// 윈도우 토스트 알림 토글
document.getElementById('jira-toast').addEventListener('change', e =>
  saveSetting('jira_toast', e.target.checked ? '1' : '0'));
// 내 댓글도 알림 토글
document.getElementById('jira-include-my-comments').addEventListener('change', e =>
  saveSetting('jira_include_my_comments', e.target.checked ? '1' : '0'));
// 알림 받을 분류 토글 → 체크된 것만 CSV 로 저장(전부 해제=빈값=백엔드에서 전부 on).
document.querySelectorAll('.jira-cat').forEach(cb => {
  cb.addEventListener('change', () => {
    const csv = [...document.querySelectorAll('.jira-cat')]
      .filter(c => c.checked).map(c => c.value).join(',');
    saveSetting('jira_categories', csv);
  });
});
// 토큰 발급 페이지 열기
document.getElementById('jira-token-link').addEventListener('click', async () => {
  try { await invoke('open_link', { url: 'https://id.atlassian.com/manage-profile/security/api-tokens' }); clearError(); }
  catch (e) { showError(e.message || e, null); }
});
// 연결 테스트 → 성공 시 accountId 자동 저장 + 이름 표시
document.getElementById('jira-test').addEventListener('click', async () => {
  const msg = document.getElementById('jira-test-msg');
  const url = document.getElementById('jira-base-url').value.trim();
  const email = document.getElementById('jira-email').value.trim();
  const token = document.getElementById('jira-api-token').value.trim();
  if (!url || !email || !token) {
    msg.textContent = '✕ URL·이메일·토큰을 모두 입력하세요';
    msg.className = 'jira-test-msg err';
    return;
  }
  msg.textContent = '확인 중…';
  msg.className = 'jira-test-msg';
  try {
    const u = await invoke('jira_test_connection', { url, email, token });
    await invoke('set_setting', { key: 'jira_account_id', value: u.accountId });
    msg.textContent = `✓ ${u.displayName || u.accountId} 님 연결됨`;
    msg.className = 'jira-test-msg ok';
    clearError();
  } catch (e) {
    msg.textContent = '✕ ' + (e.message || e);
    msg.className = 'jira-test-msg err';
  }
});

document.getElementById('hol-add-btn').addEventListener('click', async () => {
  const date = document.getElementById('hol-date').value;
  const name = document.getElementById('hol-name').value.trim();
  if (!date) { alert('날짜를 선택하세요.'); return; }
  try {
    await invoke('add_holiday', { date, name: name || '공휴일' });
    clearError();
    document.getElementById('hol-date').value = '';
    document.getElementById('hol-name').value = '';
    loadSettings();
  } catch (e) { showError(e.message || e, null); }
});
async function deleteHoliday(date) {
  try { await invoke('delete_holiday', { date }); clearError(); loadSettings(); }
  catch (e) { showError(e.message || e, () => deleteHoliday(date)); }
}

// ── 빠른 추가 창 → 메인 갱신 ──────────────────────────
// 빠른 추가 창에서 업무 등록 시 Rust 가 'task-added' 이벤트를 emit → 현재 탭만 새로고침.
function reloadActiveTab() {
  const active = document.querySelector('.tab.active');
  const fn = active && loaders[active.dataset.page];
  if (fn) fn();
}
function wireTaskAddedEvent() {
  if (window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen) {
    window.__TAURI__.event.listen('task-added', () => reloadActiveTab());
  }
}

// 자정 롤오버·재활성 시 날짜 자동 동기화.
// 앱을 자정 넘겨 계속 켜두면 오늘 탭이 어제 날짜에 고정되던 문제 방지.
let lastKnownDate = todayIso();
function syncDateIfChanged() {
  const now = todayIso();
  if (now !== lastKnownDate) { lastKnownDate = now; reloadActiveTab(); }
}

// ── 시작 ──────────────────────────────────────────────
window.addEventListener('DOMContentLoaded', () => {
  if (!hasTauri()) {
    showError('Tauri 런타임을 찾을 수 없습니다. `npm run dev` 로 앱에서 실행하세요.', null);
  }
  loadTheme(); // 저장된 테마 로드→적용 (실패 시 시스템 따름)
  loadToday();
  wireTaskAddedEvent(); // 빠른 추가 등록 시 현재 탭 자동 갱신
  wireJiraEvent();      // Jira 폴링 알림 수신 시 배지·피드 갱신
  updateJiraBadge();    // 시작 시 Jira 안읽음 배지 초기화

  // 날짜 자동 동기화: 1분마다 확인(자정 후 최대 1분 내 갱신) + 창 재활성 시 즉시
  lastKnownDate = todayIso();
  setInterval(syncDateIfChanged, 60000);
  window.addEventListener('focus', syncDateIfChanged);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) syncDateIfChanged(); });
});
