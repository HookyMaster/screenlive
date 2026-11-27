const native = (() => {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return null;
  let state = { phase: 'idle', source: '', missing: false };
  const listeners = new Set();
  let timer;
  function update(patch) {
    state = { ...state, ...patch };
    for (const listener of listeners) listener({ ...state });
  }
  async function poll() {
    try {
      const status = await invoke('recording_status');
      if (status.state === 'running' || status.state === 'saving') {
        if (status.state === 'saving' && (state.phase !== 'saving' || state.saveProgress !== status.progress)) {
          update({ phase: 'saving', saveProgress: status.progress });
        }
        timer = setTimeout(poll, 140);
        return;
      }
      update({ phase: status.state, path: status.path, error: status.error });
    } catch (error) { update({ phase: 'failed', error: String(error) }); }
  }
  return {
    sizePanel: (collapsed, menu = false) => invoke('size_panel', { collapsed, menu }),
    ready: () => invoke('panel_ready'),
    dragPanel: () => invoke('drag_panel'),
    exit: () => invoke('exit_app'),
    pickFolder: () => invoke('pick_folder'),
    pickRegion: () => invoke('pick_region'),
    loadConfig: () => invoke('load_config'),
    saveConfig: config => invoke('save_config', { config }),
    setPreview: options => invoke('set_preview', { options }),
    show: (path, folder) => invoke('show_recording', { path, folder }),
    listSources: () => invoke('list_sources'),
    backgrounds: () => invoke('background_choices'),
    audioDevices: () => invoke('audio_devices'),
    readLevels: (input, output) => invoke('read_levels', { input, output }),
    subscribe(listener) { listeners.add(listener); listener({ ...state }); },
    select(source) { update({ source, phase: 'idle', path: null, error: null }); },
    async start(settings) {
      if (!state.source || ['starting', 'recording', 'saving'].includes(state.phase)) return;
      update({ phase: 'starting', error: null, path: null, saveProgress: 0 });
      try {
        await invoke('begin', { options: {
          source: state.source, ratio: settings.ratio, mode: settings.mode,
          background: settings.background, preview: settings.preview,
          inputDevice: settings.microphone ? settings.inputDevice : null,
          outputDevice: settings.systemAudio ? settings.outputDevice : null,
          outputFolder: settings.outputFolder || null,
          closeRange: settings.closeRange,
          followRange: settings.followRange,
          cursorTrail: settings.cursorTrail,
          cursorColor: settings.cursorColor,
          includePanel: settings.includePanel,
        } });
        update({ phase: 'recording' });
        clearTimeout(timer);
        timer = setTimeout(poll, 140);
      } catch (error) { update({ phase: 'failed', error: String(error) }); }
    },
    async stop() {
      if (state.phase !== 'recording') return;
      update({ phase: 'saving', saveProgress: 0 });
      try { await invoke('control', { action: 'stop' }); } catch { /* The process may already be saving. */ }
    },
    loseSource() {},
  };
})();
