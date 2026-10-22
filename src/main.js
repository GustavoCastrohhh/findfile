// Tauri globals — injected via withGlobalTauri: true (no bundler)
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen }  = window.__TAURI__.event;
const openDialog  = window.__TAURI__?.dialog?.open ?? (async () => {
  return prompt('Digite o caminho da pasta:') || null;
});

// ---------------------------------------------------------------------------
// LOG MODULE
// ---------------------------------------------------------------------------
const Log = (() => {
  const ICONS = { info: 'ℹ️', success: '✅', warn: '⚠️', error: '❌' };
  let entries = [];
  let unreadCount = 0;
  let activeFilter = 'all';
  let isOpen = false;

  const panel     = () => document.getElementById('log-panel');
  const container = () => document.getElementById('log-entries');
  const countEl   = () => document.getElementById('log-count');

  function updateBadges() {
    document.querySelectorAll('.log-badge').forEach(b => {
      if (unreadCount > 0) {
        b.textContent = unreadCount > 99 ? '99+' : String(unreadCount);
        b.classList.add('visible');
      } else {
        b.classList.remove('visible');
      }
    });
  }

  function renderEmpty() {
    return `<div class="log-empty">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
        <polyline points="4 17 10 11 4 5"/>
        <line x1="12" y1="19" x2="20" y2="19"/>
      </svg>
      <span>Nenhuma entrada no log</span>
    </div>`;
  }

  function appendEntry(entry) {
    const c = container();
    const wasEmpty = entries.length === 1; // just added
    if (wasEmpty) c.innerHTML = '';

    const el = document.createElement('div');
    el.className = 'log-entry';
    el.dataset.level = entry.level;
    if (activeFilter !== 'all' && activeFilter !== entry.level) {
      el.classList.add('filtered');
    }
    el.innerHTML = `
      <span class="log-entry-time">${entry.time}</span>
      <span class="log-entry-icon">${ICONS[entry.level] ?? '•'}</span>
      <span class="log-entry-msg">${escHtml(entry.msg)}</span>`;
    c.appendChild(el);

    // Count
    const visible = entries.filter(e => activeFilter === 'all' || e.level === activeFilter).length;
    countEl().textContent = `${visible} entrada${visible !== 1 ? 's' : ''}`;

    // Auto-scroll
    const cb = document.getElementById('log-autoscroll-cb');
    if (cb?.checked) c.scrollTop = c.scrollHeight;
  }

  function add(msg, level = 'info') {
    const now = new Date();
    const time = now.toLocaleTimeString('pt-BR', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    const entry = { msg, level, time };
    entries.push(entry);
    appendEntry(entry);
    if (!isOpen) {
      unreadCount++;
      updateBadges();
    }
  }

  function open() {
    isOpen = true;
    unreadCount = 0;
    updateBadges();
    panel().classList.add('open');
    panel().setAttribute('aria-hidden', 'false');
    document.querySelectorAll('.btn-log').forEach(b => b.classList.add('active'));
    if (entries.length === 0) container().innerHTML = renderEmpty();
    const cb = document.getElementById('log-autoscroll-cb');
    if (cb?.checked) {
      const c = container();
      c.scrollTop = c.scrollHeight;
    }
  }

  function close() {
    isOpen = false;
    panel().classList.remove('open');
    panel().setAttribute('aria-hidden', 'true');
    document.querySelectorAll('.btn-log').forEach(b => b.classList.remove('active'));
  }

  function clear() {
    entries = [];
    unreadCount = 0;
    updateBadges();
    container().innerHTML = renderEmpty();
    countEl().textContent = '0 entradas';
  }

  function applyFilter(level) {
    activeFilter = level;
    document.querySelectorAll('.log-filter-btn').forEach(b => {
      b.classList.toggle('active', b.dataset.level === level);
    });
    document.querySelectorAll('.log-entry').forEach(el => {
      el.classList.toggle('filtered', level !== 'all' && el.dataset.level !== level);
    });
    const visible = level === 'all'
      ? entries.length
      : entries.filter(e => e.level === level).length;
    countEl().textContent = `${visible} entrada${visible !== 1 ? 's' : ''}`;
  }

  // Wire up DOM controls (call after DOMContentLoaded is implicit here)
  function init() {
    // Open buttons
    document.querySelectorAll('.btn-log').forEach(btn => {
      btn.addEventListener('click', () => isOpen ? close() : open());
    });

    // Close / backdrop
    document.getElementById('btn-log-close').addEventListener('click', close);
    document.getElementById('log-backdrop').addEventListener('click', close);

    // Clear
    document.getElementById('btn-log-clear').addEventListener('click', clear);

    // Filters
    document.querySelectorAll('.log-filter-btn').forEach(btn => {
      btn.addEventListener('click', () => applyFilter(btn.dataset.level));
    });

    // Keyboard close
    document.addEventListener('keydown', e => { if (e.key === 'Escape' && isOpen) close(); });

    // Seed
    add('Rscover iniciado.', 'info');
  }

  return { add, open, close, clear, init };
})();

// ---------------------------------------------------------------------------
// Estado global
// ---------------------------------------------------------------------------
// Tipos de arquivo: id usado no backend → extensão salva e apresentação
const FILE_TYPES = {
  jpeg: { ext: 'jpg', label: 'JPEG', icon: '🖼️' },
  png:  { ext: 'png', label: 'PNG',  icon: '🏞️' },
  pdf:  { ext: 'pdf', label: 'PDF',  icon: '📄' },
  zip:  { ext: 'zip', label: 'ZIP',  icon: '📦' },
  text: { ext: 'txt', label: 'Text', icon: '📝' },
  mp4:  { ext: 'mp4', label: 'MP4',  icon: '🎬' },
};
const TYPE_BY_EXT = Object.fromEntries(Object.entries(FILE_TYPES).map(([id, t]) => [t.ext, id]));

const state = {
  selectedDevice: null,    // StorageDevice
  includedTypes: new Set(Object.keys(FILE_TYPES)), // tipos procurados na busca
  found: {},               // id do tipo → FoundFile[] (ordem de descoberta)
  galleryType: null,       // tipo aberto na tela 3
  outputDir: '',
  isSearching: false,
  stopRequested: false,
  lastPercent: 0,
  unlistenProgress: null,
  unlistenBlock: null,
};


// ---------------------------------------------------------------------------
// Navegação entre telas
// ---------------------------------------------------------------------------
function showScreen(id) {
  document.querySelectorAll('.screen').forEach(s => s.classList.remove('active'));
  document.getElementById(id).classList.add('active');
}

// ---------------------------------------------------------------------------
// TELA 1 – Dispositivos
// ---------------------------------------------------------------------------

async function loadDevices() {
  const list = document.getElementById('device-list');
  const countEl = document.getElementById('device-count');

  list.innerHTML = `
    <div class="loading-spinner">
      <div class="spinner"></div>
      <span>Detectando dispositivos...</span>
    </div>`;

  Log.add('Buscando dispositivos de armazenamento...', 'info');

  try {
    const devices = await invoke('list_devices');
    countEl.textContent = `${devices.length} dispositivo(s) encontrado(s)`;
    Log.add(`${devices.length} dispositivo(s) encontrado(s).`, 'success');

    list.innerHTML = '';

    if (!devices.length || (devices.length === 1 && !devices[0].path)) {
      list.innerHTML = `<div class="loading-spinner"><span>Nenhum dispositivo detectado.</span></div>`;
      Log.add('Nenhum dispositivo detectado.', 'warn');
      return;
    }

    devices.forEach(device => {
      if (!device.path) return;
      const card = document.createElement('div');
      card.className = 'device-card';
      card.innerHTML = `
        <div class="device-icon">${deviceIcon(device.kind)}</div>
        <div class="device-info">
          <div class="device-name">${escHtml(device.name)}</div>
          <div class="device-path">${escHtml(device.path)}</div>
        </div>
        <div class="device-meta">
          <span class="device-size">${escHtml(device.size_label)}</span>
          <span class="device-kind ${device.kind}">${escHtml(device.kind)}</span>
        </div>
        <span class="device-arrow">›</span>`;

      card.addEventListener('click', () => selectDevice(device));
      list.appendChild(card);
    });
  } catch (err) {
    countEl.textContent = 'Erro ao listar dispositivos';
    list.innerHTML = `<div class="loading-spinner"><span style="color:#f87171">Erro: ${escHtml(String(err))}</span></div>`;
    Log.add(`Erro ao listar dispositivos: ${err}`, 'error');
  }
}

function deviceIcon(kind) {
  const map = { disk: '💽', partition: '🗂️', removable: '💾', cdrom: '💿' };
  return map[kind] ?? '🖥️';
}

function selectDevice(device) {
  state.selectedDevice = device;
  document.getElementById('selected-disk-label').textContent = device.path || device.name;
  Log.add(`Dispositivo selecionado: ${device.name} (${device.path}) — ${device.size_label}`, 'info');
  document.getElementById('output-dir-input').value = state.outputDir || suggestOutputDir();
  showOutputError('');
  resetResults();
  showScreen('screen-2');
}

document.getElementById('btn-refresh').addEventListener('click', loadDevices);

// ---------------------------------------------------------------------------
// TELA 2 – Tipos de arquivo
// ---------------------------------------------------------------------------

// Sair do disco durante a busca interrompe a busca
document.getElementById('btn-back-2').addEventListener('click', () => {
  if (state.isSearching) {
    if (!confirm('Voltar para a lista de dispositivos interrompe a busca em andamento. Continuar?')) return;
    stopSearch();
  }
  showScreen('screen-1');
});

// Clique no tipo → galeria daquele tipo (a busca continua rodando)
document.querySelectorAll('.type-card-open').forEach(btn => {
  btn.addEventListener('click', () => openGallery(btn.dataset.open));
});

// Caixa do tipo → entra ou não na próxima busca
document.querySelectorAll('[data-include]').forEach(cb => {
  cb.addEventListener('change', () => {
    const type = cb.dataset.include;
    if (cb.checked) state.includedTypes.add(type);
    else state.includedTypes.delete(type);
    cb.closest('.type-card').classList.toggle('excluded', !cb.checked);
    document.getElementById('btn-start').disabled = state.includedTypes.size === 0;
    Log.add(`${FILE_TYPES[type].label} ${cb.checked ? 'incluído na' : 'removido da'} busca.`, 'info');
  });
});

// View Blocks
const BLOCK_CELLS = 200; // 20 colunas × 10 linhas

document.getElementById('btn-view-blocks').addEventListener('click', openBlockScreen);
document.getElementById('btn-back-blocks').addEventListener('click', closeBlockScreen);

function openBlockScreen() {
  if (!state.selectedDevice?.path) return;
  document.getElementById('blocks-disk-label').textContent = state.selectedDevice.path;
  document.getElementById('blocks-usage').textContent = 'Analisando... 0%';

  const grid = document.getElementById('block-grid');
  grid.innerHTML = '';
  for (let i = 0; i < BLOCK_CELLS; i++) {
    const cell = document.createElement('div');
    cell.className = 'block-cell';
    grid.appendChild(cell);
  }

  showScreen('screen-blocks');
  runBlockScan();
}

function closeBlockScreen() {
  // O listener é removido no finally de runBlockScan quando a análise aborta
  invoke('cancel_block_scan').catch(() => {});
  showScreen('screen-2');
}

function paintCell(cellEl, fill) {
  cellEl.classList.remove('scanning');
  cellEl.classList.toggle('used', fill >= 0.5);
  cellEl.classList.toggle('partial', fill > 0 && fill < 0.5);
  cellEl.title = `${(fill * 100).toFixed(1)}% com dados`;
}

async function runBlockScan() {
  if (state.unlistenBlock) state.unlistenBlock();

  const device = state.selectedDevice;
  const cells = document.getElementById('block-grid').children;
  const usage = document.getElementById('blocks-usage');
  if (cells[0]) cells[0].classList.add('scanning');

  Log.add(`Iniciando análise de blocos em ${device.path}...`, 'info');

  const unlisten = await listen('block-progress', ({ payload }) => {
    const el = cells[payload.cell];
    if (el) paintCell(el, payload.fill);
    cells[payload.cell + 1]?.classList.add('scanning');
    usage.textContent = `Analisando... ${Math.floor(payload.percent)}%`;
  });
  state.unlistenBlock = unlisten;

  try {
    const result = await invoke('scan_blocks', { path: device.path, cells: BLOCK_CELLS });
    result.cells.forEach((fill, i) => cells[i] && paintCell(cells[i], fill));
    usage.textContent = `${fmtGB(result.used_bytes)}/${fmtGB(result.total_bytes)} GB`;
    Log.add(`Blocos analisados: ${fmtBytes(result.used_bytes)} com dados de ${fmtBytes(result.total_bytes)} (${result.percent_used.toFixed(1)}%).`, 'success');
  } catch (err) {
    // Cancelamento pelo "Go Back" não é erro
    if (!String(err).includes('cancelada')) {
      usage.textContent = `Erro: ${err}`;
      Log.add(`Erro na análise de blocos: ${err}`, 'error');
    }
  } finally {
    // Uma análise antiga (cancelada) não pode mexer no listener/grid da nova
    if (state.unlistenBlock === unlisten) {
      for (const el of cells) el.classList.remove('scanning');
      state.unlistenBlock = null;
    }
    unlisten();
  }
}

function suggestOutputDir() {
  // Tenta dar um caminho padrão baseado no SO
  return navigator.platform.startsWith('Win')
    ? 'C:\\Rscover_Output'
    : '/tmp/rscover_output';
}

// ---------------------------------------------------------------------------
// TELA 2 – Busca (pasta de saída, iniciar/parar, progresso)
// ---------------------------------------------------------------------------

// Escolher pasta de saída
document.getElementById('btn-browse').addEventListener('click', async () => {
  try {
    const selected = await openDialog({ directory: true, multiple: false });
    if (selected) {
      document.getElementById('output-dir-input').value = selected;
      state.outputDir = selected;
      showOutputError('');
    }
  } catch {
    // plugin-dialog pode não estar disponível em dev web
    const val = prompt('Digite o caminho da pasta de saída:');
    if (val) document.getElementById('output-dir-input').value = val;
  }
});

document.getElementById('output-dir-input').addEventListener('change', e => {
  state.outputDir = e.target.value;
  showOutputError('');
});

function showOutputError(msg) {
  const el = document.getElementById('output-dir-error');
  el.textContent = msg;
  el.classList.toggle('hidden', !msg);
  document.getElementById('output-dir-input').classList.toggle('invalid', !!msg);
}

// Iniciar busca
document.getElementById('btn-start').addEventListener('click', startSearch);
document.getElementById('btn-stop').addEventListener('click', stopSearch);

/** Limpa resultados e progresso (novo disco ou nova busca). */
function resetResults() {
  state.found = {};
  state.lastPercent = 0;
  Object.keys(FILE_TYPES).forEach(updateTypeCount);
  document.getElementById('progress-section').classList.add('hidden');
  document.getElementById('main-bar').style.width = '0%';
  document.getElementById('main-percent').textContent = '0%';
  document.getElementById('found-count').textContent = '0 arquivos';
  document.getElementById('status-text').textContent = 'Aguardando...';
  document.getElementById('found-files-list').innerHTML = '';
}

function setSearchingUI(searching) {
  document.getElementById('btn-start').classList.toggle('hidden', searching);
  document.getElementById('btn-stop').classList.toggle('hidden', !searching);
  document.getElementById('btn-stop').disabled = false;
  document.getElementById('btn-browse').disabled = searching;
  document.getElementById('output-dir-input').disabled = searching;
  // Os tipos da busca em andamento não mudam; a navegação continua livre
  document.querySelectorAll('[data-include]').forEach(cb => { cb.disabled = searching; });
  // A análise de blocos também lê o disco inteiro: disputaria o HD com a busca
  const blocks = document.getElementById('btn-view-blocks');
  blocks.disabled = searching;
  blocks.title = searching ? 'Indisponível durante a busca' : '';
  document.getElementById('global-progress').classList.toggle('hidden', !searching);
  updateGalleryStatus();
}

async function startSearch() {
  const outputDir = document.getElementById('output-dir-input').value.trim();
  if (!outputDir) {
    showOutputError('Informe a pasta de saída antes de iniciar.');
    return;
  }
  if (!state.selectedDevice?.path || state.includedTypes.size === 0) return;

  showOutputError('');
  resetResults();
  state.outputDir = outputDir;
  state.isSearching = true;
  state.stopRequested = false;

  const types = Object.keys(FILE_TYPES).filter(t => state.includedTypes.has(t));
  Log.add(`Busca iniciada — Disco: ${state.selectedDevice.path} | Tipos: ${types.map(t => FILE_TYPES[t].label).join(', ')} | Saída: ${outputDir}`, 'info');

  document.getElementById('progress-section').classList.remove('hidden');
  setSearchingUI(true);

  if (state.unlistenProgress) state.unlistenProgress();
  state.unlistenProgress = await listen('file-progress', ({ payload }) => {
    updateProgress(payload);
  });

  // Uma única leitura do disco procura todos os tipos incluídos
  let failed = false;
  try {
    await invoke('search_files', { path: state.selectedDevice.path, outputDir, types, max: 0 });
  } catch (err) {
    failed = true;
    const msg = String(err);
    // Recusa por pasta no disco analisado: mostra junto do campo
    if (msg.startsWith('A pasta de saída')) {
      showOutputError(msg);
      document.getElementById('progress-section').classList.add('hidden');
    } else {
      document.getElementById('status-text').textContent = `Erro: ${msg}`;
    }
    Log.add(`Erro durante busca: ${msg}`, 'error');
  }

  finishSearch({ interrupted: state.stopRequested, failed });
}

function updateProgress(p) {
  const pct = Math.min(100, Math.round(p.percent));
  state.lastPercent = pct;
  document.getElementById('main-bar').style.width = `${pct}%`;
  document.getElementById('main-percent').textContent = `${pct}%`;
  document.getElementById('global-progress-fill').style.width = `${pct}%`;
  if (!state.stopRequested) document.getElementById('status-text').textContent = p.status;
  document.getElementById('bytes-read-label').textContent = fmtBytes(p.bytes_read) + ' lidos';
  document.getElementById('total-size-label').textContent = 'de ' + fmtBytes(p.total_size);
  updateGalleryStatus();

  if (p.status.startsWith('Concluido') || p.status.startsWith('Busca interrompida')) {
    Log.add(p.status, 'info');
  }

  if (p.found_file) {
    addFoundFile(p.found_file);
    Log.add(`${p.status} — ${p.found_file.path}`, 'success');
  }
}

// ---------------------------------------------------------------------------
// Resultados por tipo
// ---------------------------------------------------------------------------

const IMAGE_EXTS = new Set(['jpg', 'png']);

function typeIcon(ext) {
  return FILE_TYPES[TYPE_BY_EXT[ext]]?.icon ?? '📁';
}

function fileName(path) {
  return path.split(/[\\/]/).pop();
}

function addFoundFile(f) {
  const type = TYPE_BY_EXT[f.extension];
  if (!type) return;
  (state.found[type] ??= []).push(f);
  updateTypeCount(type);
  if (state.galleryType === type) {
    document.getElementById('found-files-list').prepend(renderCard(f));
    updateGalleryCount();
  }
}

function updateTypeCount(type) {
  const n = state.found[type]?.length ?? 0;
  const el = document.querySelector(`[data-count="${type}"]`);
  el.textContent = fmtNum(n);
  el.classList.toggle('has-files', n > 0);
  const total = Object.values(state.found).reduce((sum, list) => sum + list.length, 0);
  document.getElementById('found-count').textContent = `${fmtNum(total)} arquivo(s)`;
}

function renderCard(f) {
  const src = convertFileSrc(f.path);
  // div com role=button: o card do ZIP tem um botão próprio dentro
  const card = document.createElement('div');
  card.className = 'found-card';
  card.setAttribute('role', 'button');
  card.tabIndex = 0;
  card.title = f.path;

  const meta = `
    <span class="found-card-meta">
      <span class="found-file-type">${escHtml(f.type_name)}</span>
      ${fmtBytes(f.size)}
    </span>`;

  if (f.extension === 'txt') {
    // Texto: o próprio trecho é a "miniatura"
    card.classList.add('text');
    card.innerHTML = `
      <p class="found-card-preview">${escHtml(f.preview ?? '')}</p>
      <div class="found-card-info row">
        <span class="found-card-name">${escHtml(fileName(f.path))}</span>${meta}
      </div>`;
  } else {
    let thumb;
    if (IMAGE_EXTS.has(f.extension)) {
      thumb = `<img src="${src}" loading="lazy" decoding="async" alt="">`;
    } else if (f.extension === 'mp4') {
      // #t= força o WebView a renderizar um quadro como miniatura
      thumb = `<video src="${src}#t=0.5" preload="metadata" muted playsinline></video>
               <span class="found-card-play">▶</span>`;
    } else {
      thumb = `<span class="found-card-icon">${typeIcon(f.extension)}</span>`;
    }
    const action = f.extension === 'zip'
      ? `<button class="found-card-action" data-reveal title="Abrir a pasta onde o arquivo está">📂 Abrir pasta</button>`
      : '';
    card.innerHTML = `
      <div class="found-card-thumb">${thumb}</div>
      <div class="found-card-info">
        <span class="found-card-name">${escHtml(fileName(f.path))}</span>
        ${meta}
        ${action}
      </div>`;
  }

  // O backend já descarta o que não decodifica; se ainda assim o WebView
  // não conseguir exibir, o arquivo é ocultado.
  card.querySelector('img, video')?.addEventListener('error', () => hideUnreadable(card, f));
  card.querySelector('[data-reveal]')?.addEventListener('click', e => {
    e.stopPropagation();
    revealFile(f);
  });
  card.addEventListener('click', () => Viewer.open(f, card));
  card.addEventListener('keydown', e => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); Viewer.open(f, card); }
  });
  return card;
}

async function revealFile(f) {
  try {
    await invoke('reveal_found_file', { path: f.path });
  } catch (err) {
    Log.add(`Não foi possível abrir a pasta de ${f.path}: ${err}`, 'error');
  }
}

function hideUnreadable(card, f) {
  const type = TYPE_BY_EXT[f.extension];
  const list = state.found[type];
  const i = list?.indexOf(f) ?? -1;
  if (i < 0) return;
  list.splice(i, 1);
  card?.remove();
  updateTypeCount(type);
  updateGalleryCount();
  Log.add(`Arquivo ilegível ocultado: ${f.path}`, 'warn');
}

// ---------------------------------------------------------------------------
// TELA 3 – Galeria de um tipo
// ---------------------------------------------------------------------------

function openGallery(type) {
  const t = FILE_TYPES[type];
  state.galleryType = type;
  document.getElementById('gallery-icon').textContent = t.icon;
  document.getElementById('gallery-title').textContent = t.label;
  document.getElementById('gallery-disk').textContent = state.selectedDevice?.path ?? '';

  // Mais recentes primeiro, como na chegada ao vivo
  const list = document.getElementById('found-files-list');
  list.innerHTML = '';
  list.classList.toggle('list', type === 'text'); // texto em linhas, não em grade
  const cards = (state.found[type] ?? []).map(renderCard).reverse();
  list.append(...cards);

  updateGalleryCount();
  updateGalleryStatus();
  showScreen('screen-3');
}

function closeGallery() {
  state.galleryType = null;
  // Solta miniaturas/vídeos da memória
  document.getElementById('found-files-list').innerHTML = '';
  showScreen('screen-2');
}

document.getElementById('btn-back-3').addEventListener('click', closeGallery);

function updateGalleryCount() {
  const type = state.galleryType;
  if (!type) return;
  const n = state.found[type]?.length ?? 0;
  const label = FILE_TYPES[type].label;
  document.getElementById('gallery-count').textContent = `${fmtNum(n)} arquivo(s)`;

  const empty = document.getElementById('gallery-empty');
  empty.classList.toggle('hidden', n > 0);
  if (n === 0) {
    empty.textContent = state.isSearching
      ? `Nenhum ${label} encontrado ainda. A busca continua — os arquivos aparecem aqui assim que forem recuperados.`
      : !state.includedTypes.has(type)
        ? `${label} não está incluído na busca. Marque a caixa do tipo na tela anterior antes de iniciar.`
        : `Nenhum arquivo ${label} recuperado.`;
  }
}

function updateGalleryStatus() {
  const el = document.getElementById('gallery-status');
  el.textContent = state.isSearching ? `Busca em andamento · ${state.lastPercent}%` : '';
  updateGalleryCount();
}

// ---------------------------------------------------------------------------
// VIEWER – painel lateral de visualização
// ---------------------------------------------------------------------------
const Viewer = (() => {
  const panel = document.getElementById('viewer-panel');
  const body  = document.getElementById('viewer-body');
  let current = null;

  function open(f, card) {
    current = f;
    document.getElementById('viewer-title').textContent = fileName(f.path);
    document.getElementById('viewer-meta').textContent = `${f.type_name} · ${fmtBytes(f.size)}`;

    const src = convertFileSrc(f.path);
    if (IMAGE_EXTS.has(f.extension)) {
      body.innerHTML = `<img src="${src}" alt="">`;
    } else if (f.extension === 'mp4') {
      body.innerHTML = `<video src="${src}" controls autoplay></video>`;
    } else if (f.extension === 'pdf') {
      body.innerHTML = `<iframe src="${src}" title="Visualização do PDF"></iframe>`;
    } else if (f.extension === 'txt') {
      body.innerHTML = `<pre class="viewer-text">${escHtml(f.preview ?? '')}</pre>`;
      loadText(f);
    } else if (f.extension === 'zip') {
      body.innerHTML = `<div class="viewer-zip"><div class="viewer-loading">Lendo conteúdo do ZIP...</div></div>`;
      loadZip(f);
    } else {
      body.innerHTML = `<div class="viewer-empty">
        <span>${typeIcon(f.extension)}</span>
        Pré-visualização indisponível para ${escHtml(f.type_name)}.
        Use o botão “Abrir” acima.</div>`;
    }
    body.querySelector('img, video')?.addEventListener('error', () => {
      close();
      hideUnreadable(card, f);
    });

    panel.classList.add('open');
    panel.setAttribute('aria-hidden', 'false');
  }

  async function loadText(f) {
    try {
      const text = await invoke('read_found_text', { path: f.path });
      if (current !== f) return; // usuário já trocou de arquivo
      body.querySelector('.viewer-text').textContent = text;
    } catch (err) {
      Log.add(`Não foi possível ler ${f.path}: ${err}`, 'error');
    }
  }

  async function loadZip(f) {
    let entries;
    try {
      entries = await invoke('zip_entries', { path: f.path });
    } catch (err) {
      Log.add(`Não foi possível ler ${f.path}: ${err}`, 'error');
      entries = null;
    }
    if (current !== f) return;
    const box = body.querySelector('.viewer-zip');
    const rows = (entries ?? []).map(e => `
      <li class="${e.is_dir ? 'dir' : ''}">
        <span class="zip-name">${e.is_dir ? '📁' : '📄'} ${escHtml(e.name)}</span>
        <span class="zip-size">${e.is_dir ? '' : fmtBytes(e.size)}</span>
      </li>`).join('');
    box.innerHTML = `
      <button class="btn-primary viewer-zip-open">📂 Abrir pasta do arquivo</button>
      <p class="viewer-zip-count">${entries ? `${fmtNum(entries.length)} item(ns)${entries.length >= 500 ? ' (primeiros 500)' : ''}` : 'Conteúdo indisponível'}</p>
      <ul class="zip-list">${rows}</ul>`;
    box.querySelector('.viewer-zip-open').addEventListener('click', () => revealFile(f));
  }

  function close() {
    panel.classList.remove('open');
    panel.setAttribute('aria-hidden', 'true');
    body.innerHTML = ''; // interrompe vídeo e libera o PDF
    current = null;
  }

  document.getElementById('btn-viewer-close').addEventListener('click', close);
  document.getElementById('btn-viewer-reveal').addEventListener('click', () => {
    if (current) revealFile(current);
  });
  document.getElementById('viewer-backdrop').addEventListener('click', close);
  document.getElementById('btn-viewer-open').addEventListener('click', async () => {
    if (!current) return;
    try {
      await invoke('open_found_file', { path: current.path });
    } catch (err) {
      Log.add(`Não foi possível abrir ${current.path}: ${err}`, 'error');
    }
  });
  document.addEventListener('keydown', e => {
    if (e.key === 'Escape' && panel.classList.contains('open')) close();
  });

  return { open, close };
})();

function finishSearch({ interrupted = false, failed = false } = {}) {
  state.isSearching = false;
  setSearchingUI(false);
  if (state.unlistenProgress) {
    state.unlistenProgress();
    state.unlistenProgress = null;
  }
  if (failed) return;

  if (interrupted) {
    document.getElementById('status-text').textContent = '⛔ Busca interrompida pelo usuário.';
    Log.add('Busca interrompida pelo usuário.', 'warn');
    return;
  }
  document.getElementById('main-bar').style.width = '100%';
  document.getElementById('main-percent').textContent = '100%';
  document.getElementById('status-text').textContent = '✅ Busca concluída!';
  Log.add('Busca concluída com sucesso.', 'success');
}

// O backend para no próximo bloco/candidato; startSearch finaliza a tela
// quando o invoke retornar.
function stopSearch() {
  if (!state.isSearching || state.stopRequested) return;
  state.stopRequested = true;
  document.getElementById('btn-stop').disabled = true;
  document.getElementById('status-text').textContent = 'Interrompendo...';
  invoke('cancel_search').catch(() => {});
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
function fmtBytes(b) {
  if (!b) return '0 B';
  const units = ['B','KB','MB','GB','TB'];
  let i = 0;
  while (b >= 1024 && i < units.length - 1) { b /= 1024; i++; }
  return `${b.toFixed(i > 0 ? 1 : 0)} ${units[i]}`;
}

function fmtGB(b) {
  return (b / 1024 ** 3).toFixed(2);
}

function fmtNum(n) {
  return Number(n).toLocaleString('pt-BR');
}

function escHtml(str) {
  return String(str)
    .replace(/&/g,'&amp;')
    .replace(/</g,'&lt;')
    .replace(/>/g,'&gt;')
    .replace(/"/g,'&quot;');
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------
Log.init();
loadDevices();
