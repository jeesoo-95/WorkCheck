// 업무 체크 — 프론트엔드 로직
// 백엔드 통신: window.__TAURI__.core.invoke (tauri.conf: withGlobalTauri=true)

const WEEKDAY_KO = ['일', '월', '화', '수', '목', '금', '토'];

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

// ── 유틸 ──────────────────────────────────────────────
function esc(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}
function pct(r) { return Math.round((r || 0) * 100); }
function todayIso() {
  const d = new Date();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${d.getFullYear()}-${m}-${day}`;
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

// ── 업무 행 렌더 (오늘 탭) ──────────────────────────────
function taskRowHtml(occ, opts) {
  opts = opts || {};
  const links = parseLinks(occ.links);
  const hasDetail = !!(occ.memo || links.length);
  const badges = [];
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
        `<a data-url="${esc(l.url || '')}" title="클릭하면 링크 복사">🔗 ${esc(l.title || l.url || '링크')}</a>`
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
  // 링크 클릭 → 클립보드 복사
  container.querySelectorAll('.task-detail a').forEach(a => {
    a.addEventListener('click', async e => {
      e.stopPropagation();
      const url = a.dataset.url;
      if (url) {
        const ok = await copyToClipboard(url);
        const orig = a.textContent;
        a.textContent = ok ? '✓ 복사됨' : '복사 실패';
        setTimeout(() => { a.textContent = orig; }, 1200);
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
async function loadManage() {
  let tasks;
  try { tasks = await invoke('list_tasks'); clearError(); }
  catch (e) { showError(e.message || e, loadManage); return; }

  const wrap = document.getElementById('manage-groups');
  wrap.innerHTML = GROUPS.map(g => {
    const items = tasks.filter(t => t.recurType === g.type);
    // 1회성 그룹만: 완료(doneOnce) 업무는 기본 숨김
    const hidden = g.type === 'once' ? items.filter(t => t.doneOnce) : [];
    const visible = g.type === 'once' ? items.filter(t => !t.doneOnce) : items;
    let body;
    if (items.length) {
      body = visible.map(t => mTaskRow(t, false)).join('');
      if (hidden.length) {
        // 숨겨진 완료 1회성: 접힌 컨테이너 + 펼침 링크
        body += `<div class="done-once-wrap" style="display:none">${hidden.map(t => mTaskRow(t, true)).join('')}</div>`;
        body += `<a class="show-done" data-count="${hidden.length}">완료된 1회성 ${hidden.length}건 보기</a>`;
      }
    } else {
      body = `<div class="empty">등록된 업무가 없습니다. <a class="add-here">＋ 업무 추가</a>로 등록하세요.</div>`;
    }
    return `<div class="group-label">${g.label} (${items.length})</div>${body}`;
  }).join('');

  // 이벤트 바인딩
  wrap.querySelectorAll('.m-task').forEach(row => {
    const id = Number(row.dataset.id);
    const task = tasks.find(t => t.id === id);
    const editBtn = row.querySelector('.edit'); // 완료 1회성 행에는 없음
    if (editBtn) editBtn.addEventListener('click', () => openModal(task));
    row.querySelector('.del').addEventListener('click', () => confirmDelete(task));
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
  wrap.querySelectorAll('.add-here').forEach(a => a.addEventListener('click', () => openModal(null)));
}

// 전체 업무 탭의 업무 행 (done=true 는 완료된 1회성: 흐리게 + 완료 배지 + 수정 숨김)
function mTaskRow(t, done) {
  return `<div class="m-task${done ? ' done' : ''}" data-id="${t.id}">
           <div class="task-name">${esc(t.name)}</div>
           <span class="rule">${esc(ruleLabelJs(t))}</span>
           ${t.notifyTime ? `<span class="notify-note">🔔 ${esc(t.notifyTime)}</span>` : ''}
           ${t.remindBefore ? `<span class="badge remind">D-${t.remindBefore} 예고</span>` : ''}
           ${done ? '<span class="badge">완료</span>' : ''}
           <div class="actions">
             ${done ? '' : '<button class="edit">수정</button>'}
             <button class="del">삭제</button>
           </div>
         </div>`;
}
// 프론트 표시용 라벨 (백엔드 rule_label 과 동일 규칙)
function ruleLabelJs(t) {
  const p = safeParam(t.recurParam);
  switch (t.recurType) {
    case 'once': {
      const dt = p.date ? new Date(p.date + 'T00:00:00') : null;
      return dt ? `1회 · ${dt.getMonth() + 1}/${dt.getDate()}` : '1회';
    }
    case 'daily': return p.weekdaysOnly ? '매일 · 평일만' : '매일';
    case 'weekly': return '매주 · ' + WEEKDAY_KO[p.weekday ?? 1];
    case 'monthly': return '매월 · ' + (p.day ?? 1) + '일';
    case 'quarterly': return '매분기 · ' + (p.monthOfQuarter ?? 1) + '번째 달 ' + (p.day ?? 1) + '일';
    case 'yearly': return '매년 · ' + (p.month ?? 1) + '/' + (p.day ?? 1);
    default: return t.recurType;
  }
}
function safeParam(raw) { try { return raw ? JSON.parse(raw) : {}; } catch { return {}; } }

async function confirmDelete(task) {
  if (!confirm(`"${task.name}" 업무를 삭제할까요?\n체크 이력도 함께 삭제됩니다.`)) return;
  try { await invoke('delete_task', { id: task.id }); clearError(); loadManage(); }
  catch (e) { showError(e.message || e, () => confirmDelete(task)); }
}

// ── 모달 (추가/수정) ──────────────────────────────────────
const modalBack = document.getElementById('modal-back');
function showParamFields(type) {
  ['once', 'daily', 'weekly', 'monthly', 'quarterly', 'yearly'].forEach(k => {
    document.getElementById('p-' + k).style.display = (k === type) ? '' : 'none';
  });
  // 매일 업무는 사전 예고가 무의미 → "N일 전 미리 알림" 입력 숨김
  document.getElementById('f-remind-wrap').style.display = (type === 'daily') ? 'none' : '';
}
document.getElementById('f-recur-type').addEventListener('change', e => showParamFields(e.target.value));

function openModal(task) {
  document.getElementById('modal-title').textContent = task ? '업무 수정' : '업무 추가';
  document.getElementById('f-id').value = task ? task.id : '';
  document.getElementById('f-name').value = task ? task.name : '';
  document.getElementById('f-memo').value = task && task.memo ? task.memo : '';
  // 링크 → 텍스트 (title|url)
  const links = task ? parseLinks(task.links) : [];
  document.getElementById('f-links').value = links.map(l => (l.title ? l.title + '|' : '') + (l.url || '')).join('\n');

  const type = task ? task.recurType : 'daily';
  document.getElementById('f-recur-type').value = type;
  showParamFields(type);
  const p = task ? safeParam(task.recurParam) : {};
  document.getElementById('f-once-date').value = p.date || todayIso();
  document.getElementById('f-weekdays-only').checked = !!p.weekdaysOnly;
  document.getElementById('f-weekday').value = String(p.weekday ?? 1);
  document.getElementById('f-month-day').value = p.day ?? 1;
  document.getElementById('f-moq').value = String(p.monthOfQuarter ?? 1);
  document.getElementById('f-q-day').value = p.day ?? 1;
  document.getElementById('f-y-month').value = p.month ?? 1;
  document.getElementById('f-y-day').value = p.day ?? 1;

  // 알림 설정 (선택)
  document.getElementById('f-notify-time').value = task && task.notifyTime ? task.notifyTime : '';
  document.getElementById('f-remind-before').value = task && task.remindBefore ? task.remindBefore : '';

  modalBack.classList.add('show');
  setTimeout(() => document.getElementById('f-name').focus(), 30);
}
function closeModal() { modalBack.classList.remove('show'); }
document.getElementById('btn-add-task').addEventListener('click', () => openModal(null));
document.getElementById('modal-cancel').addEventListener('click', closeModal);
modalBack.addEventListener('click', e => { if (e.target === modalBack) closeModal(); });

function buildRecurParam(type) {
  switch (type) {
    case 'once': return { date: document.getElementById('f-once-date').value };
    case 'daily': return { weekdaysOnly: document.getElementById('f-weekdays-only').checked };
    case 'weekly': return { weekday: Number(document.getElementById('f-weekday').value) };
    case 'monthly': return { day: Number(document.getElementById('f-month-day').value) };
    case 'quarterly': return { monthOfQuarter: Number(document.getElementById('f-moq').value), day: Number(document.getElementById('f-q-day').value) };
    case 'yearly': return { month: Number(document.getElementById('f-y-month').value), day: Number(document.getElementById('f-y-day').value) };
    default: return {};
  }
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

// ── 설정 탭 ──────────────────────────────────────────────
async function loadSettings() {
  let settings, holidays;
  try {
    [settings, holidays] = await Promise.all([invoke('get_settings'), invoke('list_holidays')]);
    clearError();
  } catch (e) { showError(e.message || e, loadSettings); return; }

  const map = {};
  settings.forEach(s => map[s.key] = s.value);
  document.getElementById('set-notify-enabled').checked = map.notify_enabled === '1';
  document.getElementById('set-notify-time').value = map.notify_time || '09:00';
  document.getElementById('set-notify-overdue').checked = map.notify_on_overdue === '1';
  document.getElementById('set-close-to-tray').checked = map.close_to_tray !== '0'; // 기본 1

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
document.getElementById('set-notify-enabled').addEventListener('change', e => saveSetting('notify_enabled', e.target.checked ? '1' : '0'));
document.getElementById('set-notify-overdue').addEventListener('change', e => saveSetting('notify_on_overdue', e.target.checked ? '1' : '0'));
document.getElementById('set-notify-time').addEventListener('change', e => saveSetting('notify_time', e.target.value));
document.getElementById('set-close-to-tray').addEventListener('change', e => saveSetting('close_to_tray', e.target.checked ? '1' : '0'));

// 자동 시작 토글 (플러그인 커맨드 경유). 실패 시 토글 원복.
document.getElementById('set-autostart').addEventListener('change', async e => {
  try { await invoke('set_autostart', { enable: e.target.checked }); clearError(); }
  catch (err) { showError(err.message || err, null); e.target.checked = !e.target.checked; }
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

// ── 시작 ──────────────────────────────────────────────
window.addEventListener('DOMContentLoaded', () => {
  if (!hasTauri()) {
    showError('Tauri 런타임을 찾을 수 없습니다. `npm run dev` 로 앱에서 실행하세요.', null);
  }
  loadToday();
});
