(async () => {
const $ = id => document.getElementById(id);
const adapter = native || backend;
let language = 'zh-CN';
const translatedNodes = [];
function t(text) {
  return language === 'en' && Object.hasOwn(screenLiveEnglish, text)
    ? screenLiveEnglish[text] : text;
}
function sourceLabel(name) {
  if (language !== 'en') return name;
  return t(name).replace(/ · 窗口 (\d+)$/, ' · Window $1')
    .replace(/^框选区域 (?=\d)/, 'Region ')
    .replace(/^音频设备 (?=\d)/, 'Audio device ');
}
function message(error) {
  const original = String(error);
  if (language !== 'en') return original;
  if (Object.hasOwn(screenLiveEnglish, original)) return t(original);
  const parts = [
    '麦克风访问被拒绝。请检查 Windows「设置 → 隐私和安全性 → 麦克风」中的桌面应用访问权限。',
    '麦克风访问被拒绝，请检查 Windows 麦克风权限。','找不到随应用提供的 ',
    '找不到所选 Windows 壁纸','无法读取所选 Windows 壁纸','不支持该背景',
    '录制退出：','电平不可用：','电脑声音','麦克风','启动失败：','视频收尾',
    '音频编码','MP4 封装','超时，已保留中间文件：','失败，已保留中间文件：',
  ];
  return parts.reduce((value, part) => value.replaceAll(part, t(part)), original);
}
function registerTranslations() {
  const walker = document.createTreeWalker(document.documentElement, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) {
    const node = walker.currentNode;
    const original = node.data.trim();
    if (sourceLabelKey(original)) {
      translatedNodes.push(() => {
        node.data = node.data.replace(/^(\s*).*?(\s*)$/s, (_, before, after) =>
          before + sourceLabel(original) + after);
      });
    }
  }
  for (const element of document.querySelectorAll('[title],[aria-label]')) {
    for (const attribute of ['title', 'aria-label']) {
      const original = element.getAttribute(attribute);
      if (original && Object.hasOwn(screenLiveEnglish, original)) {
        translatedNodes.push(() => element.setAttribute(attribute, t(original)));
      }
    }
  }
}
function sourceLabelKey(value) {
  return Object.hasOwn(screenLiveEnglish, value) || / · 窗口 \d+$/.test(value)
    || /^(框选区域|音频设备) \d/.test(value);
}

let outputFolder = '';
let selectedRegion = '';
let pickingRegion = false;
let choosingSource = false;
let savedConfig = {};
let configReady = false;
let configurationWrites = Promise.resolve();
let meterRequestPending = false;
let metersActive = false;
function setOutputFolder(folder) {
  outputFolder = folder;
  $('save-folder').textContent = folder || t('视频 / ScreenLive');
  $('save-folder').title = folder;
  $('reset-folder').hidden = !folder;
  try { localStorage.setItem('screenlive-output-folder', folder); } catch {}
  persistConfig();
}
function persistConfig() {
  if (!native || !configReady) return configurationWrites;
  const settings = {
    version: 1, outputFolder, language, ...cameraSettings(),
    ratio: $('ratio').value, mode: $('mode').value, background: $('background').value,
    preview: $('preview-start').value, appearance: $('appearance').value,
    inputDevice: $('input-device').value, outputDevice: $('output-device').value,
    microphone: $('microphone').checked, systemAudio: $('system-audio').checked, boostAudio: $('boost-audio').checked,
    sourceNames: [...$('source').selectedOptions].map(option => option.dataset.sourceName || option.textContent),
  };
  if (!settings.sourceNames.length && !sourceSelectionTouched) {
    settings.sourceNames = savedConfig.sourceNames || [];
  }
  if (selectedRegion) settings.sourceNames.push('框选区域 ' + $('region-size').textContent);
  configurationWrites = configurationWrites.catch(() => {}).then(() => native.saveConfig(settings))
    .catch(error => { $('status').textContent = t('设置保存失败：') + message(error); });
  return configurationWrites;
}
let sourceSelectionTouched = false;
function restoreSelect(id, value) {
  const select = $(id);
  if ([...select.options].some(option => option.value === value)) select.value = value;
}
if (native) {
  document.body.classList.add('native-panel');
  const feedback = document.createElement('div');
  feedback.id = 'native-feedback';
  feedback.hidden = true;
  const mark = document.createElement('span');
  mark.className = 'result-mark';
  mark.setAttribute('aria-hidden', 'true');
  const title = document.createElement('strong');
  title.className = 'result-title';
  const hint = document.createElement('div');
  hint.id = 'native-hint';
  hint.append($('status'));
  $('control-dock').append(hint);
  const done = document.createElement('button');
  done.id = 'result-back';
  done.type = 'button';
  done.textContent = t('返回');
  done.onclick = () => {
    feedback.hidden = true;
    $('settings').hidden = false;
    refreshNativePreview();
  };
  const actions = $('file-actions');
  feedback.append(mark, done, title, actions);
  $('controls').append(feedback);
  // A press on a button's visual bevel must not immediately start a window drag.
  let dragStart = null;
  $('controls').addEventListener('pointerdown', event => {
    dragStart = event.button === 0 && !event.target.closest('button,input,select,label,summary')
      ? {id:event.pointerId,x:event.clientX,y:event.clientY} : null;
  });
  $('controls').addEventListener('pointermove', event => {
    if (!dragStart || dragStart.id !== event.pointerId || !(event.buttons & 1)) return;
    if (Math.hypot(event.clientX-dragStart.x,event.clientY-dragStart.y) < 5) return;
    dragStart = null;
    native.dragPanel().catch(() => {});
  });
  for (const type of ['pointerup','pointercancel']) $('controls').addEventListener(type, () => { dragStart = null; });
  document.querySelector('header span').textContent = 'macOS';
  document.querySelector('nav').hidden = true;
  $('failure').hidden = true;
  $('sample').hidden = true;
  $('save-location').hidden = false;
  try { setOutputFolder(localStorage.getItem('screenlive-output-folder') || ''); } catch { setOutputFolder(''); }
  $('pick-folder').onclick = async () => {
    try {
      const folder = await native.pickFolder();
      if (folder) setOutputFolder(folder);
    } catch (error) { $('status').textContent = message(error); }
  };
  $('reset-folder').onclick = () => setOutputFolder('');
  const [wallpapers, config] = await Promise.all([
    native.backgrounds().catch(() => []),
    native.loadConfig().catch(() => ({})),
  ]);
  savedConfig = config || {};
  $('source').multiple = false;
  $('source').replaceChildren(new Option('请选择采集来源', ''));
  for (const option of $('source').options) option.dataset.sourceName = option.textContent;
  for (const { id, title, thumbnail } of wallpapers) {
    const option = new Option(title, id);
    option.dataset.thumbnail = thumbnail;
    $('background').add(option);
  }
  if (typeof savedConfig.outputFolder === 'string') setOutputFolder(savedConfig.outputFolder);
  for (const id of ['ratio', 'mode', 'background', 'appearance']) restoreSelect(id, savedConfig[id]);
  restoreSelect('preview-start', savedConfig.preview);
  if (Number.isFinite(savedConfig.closeRange)) $('close-range').value = savedConfig.closeRange;
  $('follow-range').value = Number.isFinite(savedConfig.followRange) ? savedConfig.followRange : $('close-range').value;
  $('cursor-trail').checked = savedConfig.cursorTrail === true;
  $('include-panel').checked = savedConfig.includePanel === true;
}
const layoutNames = { A: 'A · 竖屏主窗口', B: 'B · 横向工作台', C: 'C · 悬浮录制' };
const variants = Object.keys(layoutNames);
let active = new URLSearchParams(location.search).get('variant');
if (!variants.includes(active)) active = 'A';
function showVariant() {
  document.body.dataset.variant = active;
  $('variant').textContent = t(layoutNames[active]);
}
function cycle(step) {
  active = variants[(variants.indexOf(active) + step + variants.length) % variants.length];
  const url = new URL(location.href);
  url.searchParams.set('variant', active);
  try { history.replaceState(null, '', url); } catch { /* Local-file viewing can restrict URL updates. */ }
  showVariant();
}
$('prev').onclick = () => cycle(-1);
$('next').onclick = () => cycle(1);
showVariant();

let session;
const dialButtons = [...$('dial').querySelectorAll('[data-setting]')];
// Keep the existing select values as the source of truth for session settings.
// Show their options directly inside the circular panel, without a native popup.
const buildInlineSetting = select => {
  const field = select.parentElement;
  const group = document.createElement('fieldset');
  group.className = 'dial-choice-group';
  const legend = document.createElement('legend');
  legend.textContent = field.firstChild.textContent.trim();
  group.append(legend);
  for (const option of select.options) {
    const choice = document.createElement('label');
    const radio = document.createElement('input');
    radio.type = select.multiple ? 'checkbox' : 'radio';
    radio.name = 'dial-choice-' + select.id;
    radio.value = option.value;
    radio.onchange = () => {
      if (select.disabled) return;
      if (select.multiple) {
        const display = radio.value === 'primary-display';
        if (radio.checked) {
          if (display && selectedRegion) setSelectedRegion('');
          for (const candidate of select.options) {
            if (display || candidate.value === 'primary-display') candidate.selected = false;
          }
          if (select.selectedOptions.length + Number(Boolean(selectedRegion)) >= 8) {
            radio.checked = false;
            $('status').textContent = t('最多选择 8 个窗口。');
            return;
          }
        }
        option.selected = radio.checked;
      } else {
        if (!radio.checked) return;
        select.value = radio.value;
      }
      select.dispatchEvent(new Event('change', { bubbles: true }));
    };
    const caption = document.createElement('span');
    caption.textContent = option.textContent;
    if (select.id === 'background') {
      caption.classList.add('background-choice');
      caption.dataset.style = option.value;
      if (option.dataset.thumbnail) {
        const photo = document.createElement('img');
        photo.src = option.dataset.thumbnail;
        photo.alt = '';
        caption.prepend(photo);
      }
    }
    if (select.id === 'appearance') {
      caption.classList.add('appearance-choice');
      caption.dataset.theme = option.value;
    }
    choice.append(radio, caption);
    group.append(choice);
  }
  select.hidden = true;
  field.replaceWith(group);
  group.append(select);
  return { select, group };
};
const inlineSettings = [...$('dial-options').querySelectorAll('select')].map(buildInlineSetting);
if (native) {
  const entry = inlineSettings.find(item => item.select.id === 'source');
  const detail = document.createElement('small');
  entry.group.querySelectorAll('label').forEach(label => label.remove());
  let openingPicker = false;
  const display = document.createElement('button');
  display.type = 'button';
  display.textContent = t('选择显示器');
  display.onclick = async () => {
    if (entry.select.disabled || openingPicker || pickingRegion) return;
    openingPicker = true;
    choosingSource = true;
    $('status').textContent = t('请在系统选择器中选择显示器');
    try {
      await previewQueue;
      await window.__TAURI__.core.invoke('pick_display');
    } catch (error) {
      choosingSource = false;
      $('status').textContent = detail.textContent = message(error);
      refreshNativePreview();
    } finally { openingPicker = false; }
  };
  entry.group.append(display, $('pick-region'));
  const clear = document.createElement('button');
  clear.type = 'button';
  clear.textContent = t('取消选择');
  clear.disabled = true;
  clear.onclick = async () => {
    if (entry.select.disabled || clear.disabled || pickingRegion) return;
    entry.group.disabled = true;
    try {
      await previewQueue;
      await window.__TAURI__.core.invoke('clear_sources');
      choosingSource = false;
      entry.select.replaceChildren();
      detail.textContent = '';
      savedConfig.sourceNames = [];
      clear.disabled = true;
      entry.select.dispatchEvent(new Event('change', { bubbles:true }));
      $('status').textContent = t('已取消选择');
    } catch (error) { $('status').textContent = message(error); }
    finally { entry.group.disabled = entry.select.disabled; }
  };
  entry.group.append(clear);
  entry.group.append(detail, $('region-size'));
  window.screenLivePickerChanged = result => {
    const wasChoosing = choosingSource;
    choosingSource = false;
    if (result.status === 'cancelled' || result.status === 'error') {
      $('status').textContent = result.status === 'cancelled' ? '已取消选择' : result.message;
      if (wasChoosing) refreshNativePreview();
      return;
    }
    if (result.status !== 'selected') return;
    const option = new Option(result.title, result.id, true, true);
    option.dataset.sourceName = result.title;
    entry.select.replaceChildren(option);
    detail.textContent = result.title;
    clear.disabled = false;
    entry.select.dispatchEvent(new Event('change', { bubbles:true }));
  };
}
function syncInlineSettings() {
  for (const { select, group } of inlineSettings) {
    group.disabled = select.disabled;
    const checked = new Set([...select.selectedOptions].map(option => option.value));
    for (const radio of group.querySelectorAll('input')) radio.checked = checked.has(radio.value);
  }
}
for (const { select } of inlineSettings) select.addEventListener('change', syncInlineSettings);
const svgNS = 'http://www.w3.org/2000/svg';
for (const [index, button] of dialButtons.entries()) {
  const inner = index >= 4;
  const angle = -90 + (index % 4) * 90;
  const low = inner ? .165 : .338;
  const high = inner ? .301 : .476;
  // Constant perpendicular inset from the shared 45° dividing lines.
  const halfGap = .014;
  const halfAngleAt = radius => 45 - Math.asin(halfGap / radius) * 180 / Math.PI;
  const innerAngle = halfAngleAt(low);
  const outerAngle = halfAngleAt(high);
  const point = (radius, degrees) => {
    const radians = degrees * Math.PI / 180;
    return [.5 + radius * Math.cos(radians), .5 + radius * Math.sin(radians)];
  };
  const clip = document.createElementNS(svgNS, 'clipPath');
  clip.id = 'sector-' + index;
  clip.setAttribute('clipPathUnits', 'objectBoundingBox');
  const path = document.createElementNS(svgNS, 'path');
  path.setAttribute('d', `M ${point(high, angle - outerAngle)} A ${high},${high} 0 0 1 ${point(high, angle + outerAngle)}
    L ${point(low, angle + innerAngle)} A ${low},${low} 0 0 0 ${point(low, angle - innerAngle)} Z`);
  clip.append(path);
  $('dial-clips').querySelector('defs').append(clip);
  $('dial-relief').append(path.cloneNode());
  button.style.clipPath = 'url(#' + clip.id + ')';
  const [x, y] = point((low + high) / 2, angle);
  const label = document.createElement('span');
  label.textContent = button.textContent;
  label.style.left = x * 100 + '%';
  label.style.top = y * 100 + '%';
  const bevel = document.createElementNS(svgNS, 'svg');
  bevel.setAttribute('viewBox', '0 0 1 1');
  bevel.setAttribute('aria-hidden', 'true');
  const edge = path.cloneNode();
  edge.setAttribute('fill', 'none');
  edge.setAttribute('stroke', 'url(#sector-bevel)');
  edge.setAttribute('stroke-width', '.014');
  bevel.append(edge);
  button.replaceChildren(bevel, label);
}
// SVG sector clips omit the bevels. Assign only the narrow gaps to the
// nearest sector; real button clicks and keyboard activation keep their handlers.
function sectorAtEdge(x,y) {
  const radius = Math.hypot(x,y);
  if (radius < 0.15 || radius > 0.5) return -1;
  const quarter = (Math.floor((Math.atan2(y,x) + Math.PI*0.75) / (Math.PI/2)) + 4) % 4;
  return quarter + (radius < (0.301+0.338)/2 ? 4 : 0);
}
function dialTarget(event) {
  const direct = event.target.closest('button');
  if (direct) return direct.disabled ? null : direct;
  const r = $('dial').getBoundingClientRect();
  const index = sectorAtEdge((event.clientX-r.left)/r.width-0.5,(event.clientY-r.top)/r.height-0.5);
  return index >= 0 && !dialButtons[index].disabled ? dialButtons[index] : null;
}
function detailTarget(event) {
  const direct = event.target.closest('button,label,input,select');
  if (direct) {
    const label = direct.closest('label') || direct;
    return direct.matches(':disabled') || label.querySelector(':disabled') ? null : label;
  }
  const root = $('dial-detail');
  const area = root.getBoundingClientRect();
  if (event.clientX >= area.left + root.clientWidth) return null;
  let best = null, distance = 3.01;
  for (const el of root.querySelectorAll('button,label')) {
    if (!el.getClientRects().length || el.matches(':disabled') || el.querySelector(':disabled') || el.closest('[hidden]')) continue;
    const r = el.getBoundingClientRect();
    const d = Math.hypot(Math.max(r.left-event.clientX,0,event.clientX-r.right),Math.max(r.top-event.clientY,0,event.clientY-r.bottom));
    if (d < distance) { best = el; distance = d; }
  }
  return best;
}
let highlighted = null;
function highlight(next) {
  if (next === highlighted) return;
  highlighted?.classList.remove('hit-hover');
  highlighted = next;
  highlighted?.classList.add('hit-hover');
}
for (const [root,resolve] of [[$('dial'),dialTarget],[$('dial-detail'),detailTarget]]) {
  root.addEventListener('pointermove', event => highlight(resolve(event)));
  root.addEventListener('pointerover', event => highlight(resolve(event)));
  root.addEventListener('pointerleave', () => highlight(null));
  root.addEventListener('click', event => {
    if (!event.isTrusted || event.button !== 0 || event.target.closest('button,label,input,select')) return;
    resolve(event)?.click();
  });
}
function selectDialSection(id) {
  highlight(null);
  $('dial').hidden = Boolean(id);
  $('dial-detail').hidden = !id;
  for (const button of dialButtons) {
    const key = button.dataset.setting;
    const item = $(key);
    (item.closest('.dial-choice-group') || item).hidden = key !== id;
  }
  $('dial-options').hidden = !id || !$('dial-options').contains($(id));
  $('preview-range').hidden = id !== 'preview-start';
  $('follow-range-row').hidden = id !== 'preview-start';
  if (id) $('dial-back').focus();
  updateAudioLevels();
}
for (const button of dialButtons) button.onclick = () => selectDialSection(button.dataset.setting);
function setCollapsed(collapsed) {
  $('controls').hidden = collapsed;
  $('expand-dial').hidden = !collapsed;
  $('expand-dial').setAttribute('aria-expanded', !collapsed);
  if (native) {
    $('gem-menu').hidden = true;
    document.body.classList.remove('gem-menu-open');
    document.body.classList.toggle('panel-collapsed', collapsed);
    native.sizePanel(collapsed).catch(() => {});
  }
}
let suppressGemClick = false;
function setGemMenu(open) {
  if (!native || $('expand-dial').hidden) return;
  $('gem-menu').hidden = !open;
  $('gem-error').hidden = true;
  document.body.classList.toggle('gem-menu-open', open);
  native.sizePanel(true, open).catch(() => {});
  $(open ? 'gem-cancel' : 'expand-dial').focus();
}
if (native) {
  const gem = $('expand-dial');
  gem.addEventListener('contextmenu', event => {
    event.preventDefault();
    setGemMenu($('gem-menu').hidden);
  });
  $('gem-cancel').onclick = () => setGemMenu(false);
  $('gem-exit').onclick = () => native.exit().catch(error => {
    $('gem-error').textContent = message(error);
    $('gem-error').hidden = false;
  });
  let press;
  gem.addEventListener('pointerdown', event => {
    if (event.button !== 0) return;
    suppressGemClick = false;
    press = { id: event.pointerId, x: event.screenX, y: event.screenY };
    gem.setPointerCapture(event.pointerId);
  });
  gem.addEventListener('pointermove', event => {
    if (!press || press.id !== event.pointerId ||
        Math.hypot(event.screenX - press.x, event.screenY - press.y) < 5) return;
    suppressGemClick = true;
    press = null;
    if (gem.hasPointerCapture(event.pointerId)) gem.releasePointerCapture(event.pointerId);
    native.dragPanel().catch(() => {});
  });
  gem.addEventListener('lostpointercapture', () => { press = null; });
}
$('dial-back').onclick = () => {
  selectDialSection(null);
  dialButtons[0].focus();
};
$('collapse-dial').onclick = () => {
  selectDialSection(null);
  setCollapsed(true);
  $('expand-dial').focus();
};
$('expand-dial').onclick = event => {
  if (suppressGemClick && event.detail > 0) {
    suppressGemClick = false;
    return;
  }
  setCollapsed(false);
  dialButtons[0].focus();
};
function togglePreview() {
  if (native) {
    const enabled = $('preview-start').value !== 'show';
    $('preview-start').value = enabled ? 'show' : 'hide';
    $('preview-start').dispatchEvent(new Event('change', { bubbles:true }));
    return;
  }
  if (session.phase !== 'recording') return;
  $('preview').hidden = !$('preview').hidden;
  if ($('preview').hidden) $('pointer').hidden = true;
}
window.screenLiveDidHidePreview = () => {
  $('preview-start').value = 'hide';
  syncInlineSettings();
  persistConfig();
};
let previewQueue = Promise.resolve();
function sourceSelection() {
  const sources = [...$('source').selectedOptions].map(option => option.value);
  if (selectedRegion) return selectedRegion;
  return sources.join(',');
}
function setSelectedRegion(region) {
  selectedRegion = region;
  const label = $('region-size');
  label.hidden = !region;
  $('pick-region').textContent = region ? t('重新框选') : t('框选区域');
  if (region) {
    const [, left, top, right, bottom] = region.split(':').map(Number);
    label.textContent = (right - left) + ' × ' + (bottom - top);
    for (const option of $('source').options) {
      if (option.value === 'primary-display') option.selected = false;
    }
  } else label.textContent = '';
}
if (native) {
  $('pick-region').onclick = async () => {
    if (pickingRegion || !['idle', 'saved', 'failed'].includes(session?.phase)) return;
    pickingRegion = true;
    $('status').textContent = '正在准备框选画面…';
    try {
      await previewQueue;
      const region = await native.pickRegion($('source').value, $('include-panel').checked);
      if (region) {
        sourceSelectionTouched = true;
        setSelectedRegion(region);
        adapter.select(sourceSelection());
        persistConfig();
        pickingRegion = false;
        refreshNativePreview();
      } else { $('status').textContent = '已取消框选'; }
    } catch (error) {
      $('status').textContent = message(error);
    } finally {
      pickingRegion = false;
    }
  };

}
function cameraSettings() {
  return {
    closeRange: Number($('close-range').value),
    followRange: Number($('follow-range').value),
    cursorTrail: $('cursor-trail').checked,
    includePanel: $('include-panel').checked,
  };
}
function cursorColor() {
  return getComputedStyle($('control-dock')).getPropertyValue('--theme-accent').trim();
}
function refreshNativePreview() {
  if (!native || choosingSource || pickingRegion || !['idle', 'saved', 'failed', 'recording'].includes(session?.phase)) return;
  previewQueue = previewQueue.catch(() => {}).then(async () => {
    if (choosingSource || pickingRegion || !['idle', 'saved', 'failed', 'recording'].includes(session?.phase)) return;
    const result = await native.setPreview({
      source: sourceSelection(), ratio: $('ratio').value, mode: $('mode').value,
      background: $('background').value, preview: $('preview-start').value === 'show',
      ...cameraSettings(),
      cursorColor: cursorColor(),
    });
    if (result?.previewDenied) {
      $('preview-start').value = 'hide';
      syncInlineSettings();
      $('status').textContent = '已保留所选来源。系统未允许预览，可在「预览」中重新开启。';
    }
  }).catch(error => { $('status').textContent = message(error); });
}
$('hide-preview').onclick = togglePreview;
function renderSession(state) {
  const wasRunning = session?.phase === 'recording';
  session = state;
  const { phase, source, missing } = state;
  const running = phase === 'recording';
  const locked = running || phase === 'saving' || phase === 'starting';
  if (wasRunning && !running) {
    setCollapsed(false);
    selectDialSection(null);
  }
  document.body.dataset.state = phase;
  document.body.dataset.source = source;
  if (native && running && !wasRunning) {
    selectDialSection(null);
    setCollapsed(true);
  }
  $('source').disabled = locked;
  $('pick-region').disabled = locked || !$('source').value;
  $('mode').disabled = locked;
  $('ratio').disabled = locked;
  $('background').disabled = locked && !running;
  $('preview-start').disabled = locked && !running;
  $('appearance').disabled = locked;
  $('close-range').disabled = locked;
  $('follow-range').disabled = locked;
  $('cursor-trail').disabled = locked;
  $('include-panel').disabled = locked;
  syncInlineSettings();
  $('system-audio').disabled = locked || $('system-audio').dataset.unavailable === 'true';
  $('microphone').disabled = locked || $('microphone').dataset.unavailable === 'true';
  syncAudioControls(locked);
  for (const button of dialButtons) button.disabled = locked && !(running && ['preview-start','background'].includes(button.dataset.setting));
  $('record').disabled = !source || (locked && !(native && running));
  $('record').textContent = running ? t('结束录制') : phase === 'saving' ? t('正在保存…') : t('开始录制');
  $('preview-label').textContent = phase === 'saved' ? t('成片预览（模拟）') : t('实时预览');
  if (!running) $('preview').hidden = false;
  else if (!wasRunning) $('preview').hidden = $('preview-start').value === 'hide';
  $('hide-preview').hidden = !running;
  $('sample').hidden = Boolean(native) || !source || missing;
  $('pointer').hidden = true;
  $('empty').textContent = !source ? t('选择采集来源后查看画面') : missing ? t('窗口不可用 · 使用虚拟背景') : '';
  $('status').textContent = {
    idle: source === 'window' ? t('仅采集所选窗口，超出部分填入背景。') :
      source === 'screen' ? t('整屏采集可能包含其他窗口和通知。') : t('选择窗口或显示器，确认手机画面后再开录。'),
    recording: missing ? t('采集窗口已丢失，成片继续显示背景。') : t('录制中（模拟）'),
    saving: t('正在封装 MP4（模拟）'),
    saved: t('模拟保存成功 · 原型未生成文件'),
    failed: t('模拟保存失败 · 请检查输出位置后重试'),
  }[phase];
  if (native) {
    const feedback = $('native-feedback');
    const showResult = ['starting', 'saving', 'saved', 'failed'].includes(phase);
    feedback.hidden = !showResult;
    const progress = Math.min(100, Math.max(0, state.saveProgress || 0));
    const saving = phase === 'saving';
    $('save-ring').hidden = !saving;
    $('save-ring').setAttribute('aria-valuenow', progress);
    $('save-arc').style.strokeDasharray = progress + ' 100';
    $('save-light').style.transform = 'rotate(' + (progress * 3.6) + 'deg)';
    $('settings').hidden = showResult;
    const mark = feedback.querySelector('.result-mark');
    mark.textContent = saving && progress > 0 ? progress + '%' : '·';
    mark.hidden = phase !== 'starting' && phase !== 'saving';
    feedback.querySelector('.result-title').textContent = ({
      starting: t('准备录制'), saving: t('正在保存'), saved: t('视频已保存'), failed: t('保存未完成'),
    })[phase] || '';
    $('open-folder').hidden = phase !== 'saved';
    $('play-file').hidden = phase !== 'saved';
    $('result-back').hidden = phase === 'starting' || phase === 'saving';
    $('preview-label').textContent = t('录制画幅');
    $('empty').textContent = source ? t('开始录制后打开实时预览') : t('请先选择显示器');
    $('status').textContent = ({
      idle: source === 'primary-display' ? t('整屏可能包含其他窗口与通知。') :
        source ? t('F8 显示或隐藏预览 · F9 开录') :
        savedConfig.sourceNames?.length ? t('请确认采集来源 · 上次：') + savedConfig.sourceNames.map(sourceLabel).join(language === 'en' ? ', ' : '、') : t('请先选择采集来源'),
      starting: t('正在启动录制…'), recording: t('正在录制 · F8 切换预览 · F9 结束'),
      saving: t('正在封装 MP4'), saved: t('点击播放，或打开保存文件夹'), failed: state.error ? message(state.error) : t('录制失败'),
    })[phase];
    $('file-actions').hidden = phase !== 'saved';
    $('play-file').onclick = () => native.show(state.path, false).catch(error => { $('status').textContent = message(error); });
    $('open-folder').onclick = () => native.show(state.path, true).catch(error => { $('status').textContent = message(error); });
  }
}
adapter.subscribe(renderSession);
if (native && sourceSelection()) adapter.select(sourceSelection());
$('source').onchange = () => {
  setSelectedRegion('');
  sourceSelectionTouched = true;
  adapter.select(sourceSelection());
  refreshNativePreview();
};
$('preview-start').onchange = refreshNativePreview;
$('cursor-trail').onchange = refreshNativePreview;
$('include-panel').onchange = refreshNativePreview;
for (const [input, output] of [['close-range', 'close-value'], ['follow-range', 'follow-value']]) {
  const refresh = () => { $(output).value = $(input).value + '%'; };
  $(input).oninput = refresh;
  $(input).onchange = refreshNativePreview;
  refresh();
}
function applyAppearance() {
  $('control-dock').dataset.appearance = $('appearance').value;
  if (native) document.documentElement.dataset.appearance = $('appearance').value;
}
$('appearance').onchange = () => {
  applyAppearance();
  refreshNativePreview();
};
applyAppearance();
async function toggle() {
  if (session.phase === 'recording') adapter.stop($('fail').checked);
  else {
    if (native) await persistConfig();
    adapter.start({
    systemAudio: $('system-audio').checked, boostAudio: $('boost-audio').checked,
    microphone: $('microphone').checked,
    outputDevice: $('output-device').value,
    inputDevice: $('input-device').value,
    ratio: $('ratio').value, mode: $('mode').value,
    background: $('background').value, preview: $('preview-start').value === 'show',
    outputFolder,
    ...cameraSettings(),
    cursorColor: cursorColor(),
    });
  }
}
function syncAudioControls(locked = $('system-audio').disabled) {
  $('boost-audio').disabled = locked;
  const outputEnabled = $('system-audio').checked;
  const inputEnabled = $('microphone').checked;
  $('output-row').hidden = !outputEnabled;
  $('input-row').hidden = !inputEnabled;
  $('audio-scope').hidden = !outputEnabled;
  $('output-level').hidden = !outputEnabled;
  $('input-level').hidden = !inputEnabled;
  $('audio-demo').hidden = Boolean(native) || (!outputEnabled && !inputEnabled);
  $('output-device').disabled = locked || !outputEnabled;
  $('input-device').disabled = locked || !inputEnabled;
  updateAudioLevels();
}
function meterSelections() {
  const visible = !document.hidden && !$('controls').hidden && !$('settings').hidden &&
    !$('dial-detail').hidden && !$('audio-controls').hidden &&
    ['idle', 'saved', 'failed'].includes(session?.phase);
  return ['input', 'output'].map(kind =>
    visible && !$(kind + '-level').hidden ? $(kind + '-device').value || null : null);
}
function showLevel(kind, db, error = '') {
  $(kind + '-meter').value = db;
  $(kind + '-db').textContent = error ? t('不可用') : (db <= -60 ? '−∞' : String(Math.round(db))) + ' dBFS';
  $(kind + '-level').title = message(error || '');
}
async function updateAudioLevels() {
  const selections = meterSelections();
  if (native) {
    if (meterRequestPending || (!selections.some(Boolean) && !metersActive)) return;
    meterRequestPending = true;
    try {
      const levels = await native.readLevels(...selections);
      metersActive = selections.some(Boolean);
      const current = meterSelections();
      for (const [index, kind] of ['input', 'output'].entries()) {
        if (selections[index] !== current[index] || !current[index]) continue;
        const level = levels[index];
        showLevel(kind, level.db ?? -60, level.error || '');
      }
    } catch {
      for (const [index, kind] of ['input', 'output'].entries()) {
        if (selections[index]) showLevel(kind, -60, t('无法读取 macOS 音频电平，请检查系统权限。'));
      }
    } finally {
      meterRequestPending = false;
    }
    return;
  }
  if (!selections.some(Boolean)) return;
  for (const kind of ['output', 'input']) {
    if ($(kind + '-level').hidden) continue;
    const db = backend.audioLevel(kind, $(kind + '-device').value, performance.now() / 1000);
    showLevel(kind, db);
  }
}
const audioDevices = await Promise.resolve(adapter.audioDevices()).catch(() => ({ output: [], input: [] }));
for (const [kind, devices] of Object.entries(audioDevices)) {
  $(kind + '-device').replaceChildren(...devices.map(([id, label]) => new Option(label, id)));
  $(kind + '-device').onchange = updateAudioLevels;
}
if (native) {
  const missingAudio = [];
  for (const [kind, id] of [['output', 'system-audio'], ['input', 'microphone']]) {
    if (!audioDevices[kind].length) {
      $(id).disabled = true;
      $(id).dataset.unavailable = 'true';
      $(id).closest('label').title = t('未发现兼容设备');
      missingAudio.push(kind);
    }
  }
  $('audio-unavailable').hidden = missingAudio.length === 0;
  $('audio-unavailable').textContent = missingAudio.includes('input')
    ? t('未找到麦克风。请检查 macOS 声音设置和桌面应用的麦克风权限。')
    : t('未找到播放设备。请在 macOS 声音设置中确认扬声器已启用。');
  $('output-meter').setAttribute('aria-label', t('电脑声音电平'));
  $('input-meter').setAttribute('aria-label', t('麦克风电平'));
  restoreSelect('input-device', savedConfig.inputDevice);
  restoreSelect('output-device', savedConfig.outputDevice);
  $('microphone').checked = !$('microphone').disabled && savedConfig.microphone === true;
  $('boost-audio').checked = savedConfig.boostAudio !== false;
  $('system-audio').checked = !$('system-audio').disabled && savedConfig.systemAudio === true;
  syncAudioControls();
  configReady = true;
  document.addEventListener('change', persistConfig);
}
$('system-audio').onchange = () => syncAudioControls();
$('microphone').onchange = () => syncAudioControls();
setInterval(updateAudioLevels, 100);
document.addEventListener('visibilitychange', updateAudioLevels);
$('record').onclick = toggle;
for (const id of ['mode', 'background']) {
  const apply = () => {
    document.body.dataset[id] = $(id).value;
  };
  $(id).onchange = () => { apply(); refreshNativePreview(); };
  apply();
}
const outputSizes = {
  '9:16': [2160, 3840], '16:9': [3840, 2160], '1:1': [2160, 2160],
  '4:5': [2160, 2700], '4:3': [2880, 2160], '3:4': [2160, 2880],
};
function applyRatio() {
  const ratio = $('ratio').value;
  const [width, height] = outputSizes[ratio];
  $('frame').style.setProperty('--ratio', width / height);
  $('frame').style.setProperty('--fit-width', Math.min(100, (16 / 9) * height / width * 100) + '%');
  $('frame').style.setProperty('--fit-height', Math.min(100, (9 / 16) * width / height * 100) + '%');
  $('output-spec').textContent = ratio + '　·　' + width + ' × ' + height + '　·　30 fps';
  if (native) $('record').title = $('output-spec').textContent;
}
$('ratio').onchange = () => { applyRatio(); refreshNativePreview(); };
applyRatio();
document.addEventListener('keydown', event => {
  if (native && event.key === 'Escape' && !$('gem-menu').hidden) {
    event.preventDefault();
    setGemMenu(false);
    return;
  }
  if (event.key === 'F8' && session.phase === 'recording') {
    event.preventDefault();
    if (!event.repeat) togglePreview();
    return;
  }
  if (native && event.key === 'F8' && ['idle', 'saved', 'failed'].includes(session.phase)) {
    event.preventDefault();
    if (!event.repeat) {
      const setting = $('preview-start');
      setting.value = setting.value === 'show' ? 'hide' : 'show';
      setting.dispatchEvent(new Event('change', { bubbles: true }));
    }
    return;
  }
  if (event.key === 'F9') {
    event.preventDefault();
    if (!event.repeat) toggle();
    return;
  }
  if (session.phase === 'recording') {
    if (event.key === 'F7' && !event.repeat) adapter.loseSource();
    return;
  }
  if (event.target.closest('input,textarea,select,[contenteditable]')) return;
  if (event.key === 'ArrowLeft') cycle(-1);
  if (event.key === 'ArrowRight') cycle(1);
});
let mockOrigin = null;
let mockCenter = null;
let mockWide = false;
let settle;
$('frame').addEventListener('pointermove', event => {
  if (native) return;
  if (!session.source || session.missing || $('mode').value !== 'follow') return;
  const { width, height, left, top } = $('frame').getBoundingClientRect();
  const x = (event.clientX - left) / width;
  const y = (event.clientY - top) / height;
  const point = { x, y };
  const minimum = Number($('close-range').value) / 100;
  const follow = Number($('follow-range').value) / 100;
  mockOrigin ??= point;
  mockCenter ??= point;
  if (!mockWide && Math.max(Math.abs(x - mockCenter.x), Math.abs(y - mockCenter.y)) > follow * .5) {
    const span = Math.max(Math.floor(Math.abs(x - mockOrigin.x) * width * 1.2) / width,
      Math.floor(Math.abs(y - mockOrigin.y) * height * 1.2) / height, minimum);
    $('frame').style.setProperty('--zoom', String(minimum / span));
    mockWide = true;
  }
  if (!mockWide) {
    $('frame').style.setProperty('--pan', (-35 * x) + '%');
    mockCenter = point;
  }
  clearTimeout(settle);
  settle = setTimeout(() => {
    mockOrigin = mockCenter = point;
    mockWide = false;
    $('frame').style.setProperty('--pan', (-35 * x) + '%');
    $('frame').style.setProperty('--zoom', '1');
  }, 600);
  $('pointer').hidden = false;
  $('pointer').style.left = (x * 100) + '%';
  $('pointer').style.top = (y * 100) + '%';
});
$('frame').addEventListener('pointerleave', () => {
  mockOrigin = mockCenter = null;
  mockWide = false;
  clearTimeout(settle);
  $('frame').style.setProperty('--zoom', '1');
  $('pointer').hidden = true;
});
registerTranslations();
function applyLanguage(next) {
  language = next === 'en' ? 'en' : 'zh-CN';
  document.documentElement.lang = language;
  $('language').value = language;
  for (const translate of translatedNodes) translate();
  $('save-folder').textContent = outputFolder || t('视频 / ScreenLive');
  setSelectedRegion(selectedRegion);
  showVariant();
  if (session) renderSession(session);
}
let initialLanguage = savedConfig.language;
if (!native) {
  try { initialLanguage = localStorage.getItem('screenlive-language'); } catch {}
}
applyLanguage(initialLanguage);
$('language').onchange = () => {
  applyLanguage($('language').value);
  if (!native) {
    try { localStorage.setItem('screenlive-language', language); } catch {}
  }
};
if (native) {
  const controls = $('controls');
  const hint = $('native-hint');
  controls.hidden = true;
  hint.hidden = true;
  const root = document.documentElement;
  root.classList.add('native-first-paint', 'startup-pending');
  let glowTimer;
  let openTimer;
  const finish = () => {
    clearTimeout(glowTimer);
    clearTimeout(openTimer);
    root.classList.remove('native-first-paint', 'startup-pending', 'startup-glowing', 'startup-opening');
    controls.hidden = false;
    hint.hidden = false;
  };
  native.ready().then(() => {
    glowTimer = setTimeout(() => {
      root.classList.replace('startup-pending', 'startup-glowing');
      openTimer = setTimeout(() => {
        root.classList.replace('startup-glowing', 'startup-opening');
        root.classList.remove('native-first-paint');
        controls.hidden = false;
        hint.hidden = false;
        setTimeout(finish, 800);
      }, 800);
    }, 800);
  }).catch(finish);
}
})();
