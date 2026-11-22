// A short movement is a click; a longer movement moves the whole native window.
// Serialize moves so a slow IPC response can't put the window at an old position.
let gesture = null;
let suppressClick = false;
const invoke = window.__TAURI__.core.invoke;

document.addEventListener('pointerdown', (event) => {
  const button = event.target.closest('.bubble');
  if (!button || event.button !== 0) return;
  suppressClick = false;
  button.setPointerCapture(event.pointerId);
  gesture = {
    pointer: event.pointerId,
    x: event.screenX,
    y: event.screenY,
    moved: false,
    latest: null,
    running: false,
    origin: invoke('notes_drag_origin'),
  };
}, true);

async function flush(state) {
  if (state.running) return;
  state.running = true;
  try {
    const [x, y, scale] = await state.origin;
    while (state.latest) {
      const [dx, dy] = state.latest;
      state.latest = null;
      await invoke('move_notes', { x: Math.round(x + dx * scale), y: Math.round(y + dy * scale) });
    }
  } catch (error) {
    console.error('Could not move notes window', error);
  } finally {
    state.running = false;
  }
}

document.addEventListener('pointermove', (event) => {
  const state = gesture;
  if (!state || event.pointerId !== state.pointer) return;
  const dx = event.screenX - state.x;
  const dy = event.screenY - state.y;
  if (!state.moved && Math.hypot(dx, dy) < 4) return;
  state.moved = true;
  state.latest = [dx, dy];
  void flush(state);
}, true);

document.addEventListener('pointerup', (event) => {
  if (!gesture || event.pointerId !== gesture.pointer) return;
  suppressClick = gesture.moved;
  gesture = null;
}, true);

document.addEventListener('pointercancel', () => { gesture = null; }, true);
document.addEventListener('click', (event) => {
  if (suppressClick && event.target.closest('.bubble')) {
    event.preventDefault();
    event.stopImmediatePropagation();
    suppressClick = false;
  }
}, true);
