// Mock adapter. A desktop adapter can implement the same commands and state notifications.
const backend = (() => {
  let state = { phase: 'idle', source: '', missing: false, startedAt: 0 };
  const listeners = new Set();
  function update(patch) {
    state = { ...state, ...patch };
    listeners.forEach(listener => listener({ ...state }));
  }
  return {
    audioLevel(kind, device, seconds) {
      const offset = [...device].reduce((sum, char) => sum + char.charCodeAt(0), 0) / 12;
      const signal = Math.max(0, Math.sin(seconds * (kind === 'output' ? 2.6 : 4.3) + offset));
      return signal < .1 ? -60 : Math.round(-42 + 32 * signal);
    },
    audioDevices() {
      return {
        output: [['default', '系统默认播放设备'], ['speakers', '内置扬声器（模拟）'], ['headphones', 'USB 耳机（模拟）']],
        input: [['default', '系统默认输入设备'], ['builtin-mic', '内置麦克风（模拟）'], ['usb-mic', 'USB 麦克风（模拟）']],
      };
    },
    subscribe(listener) { listeners.add(listener); listener({ ...state }); },
    select(source) {
      if (['idle', 'saved', 'failed'].includes(state.phase)) update({ source, phase: 'idle', missing: false });
    },
    start({ systemAudio = false, microphone = false, outputDevice = 'default', inputDevice = 'default' } = {}) {
      if (state.source && ['idle', 'saved', 'failed'].includes(state.phase)) {
        update({
          phase: 'recording', missing: false, startedAt: performance.now(), systemAudio, microphone,
          outputDevice: systemAudio ? outputDevice : null,
          inputDevice: microphone ? inputDevice : null,
        });
      }
    },
    loseSource() {
      if (state.phase === 'recording' && state.source === 'window') update({ missing: true });
    },
    stop(fail) {
      if (state.phase !== 'recording') return;
      update({ phase: 'saving' });
      setTimeout(() => update({ phase: fail ? 'failed' : 'saved' }), 650);
    },
  };
})();
