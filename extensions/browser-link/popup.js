chrome.runtime.sendMessage({ type: 'status' }, (res) => {
  const el = document.getElementById('status');
  el.textContent = res && res.connected ? 'Connected to AmpSync' : 'AmpSync is not running';
});
