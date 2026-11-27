// Run before the stylesheet and body so desktop startup cannot paint the prototype.
if (window.__TAURI__ || new URLSearchParams(location.search).has('desktop')) {
  document.documentElement.classList.add('native-first-paint');
}
