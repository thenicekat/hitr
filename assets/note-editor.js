const root = document.getElementById('note-editor');
const toolbar = document.getElementById('note-format-toolbar');
const popupHost = document.getElementById('note-reminder-popup');
let selectedLine = null;
let popupLine = null;
const newId = () => `line-${Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('')}`;
root.innerHTML = noteContent.clean(await dioxus.recv());
document.execCommand('styleWithCSS', false, false);

function lineAtCursor() {
  const node = window.getSelection()?.anchorNode;
  if (!node || !root.contains(node)) return null;
  const element = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
  const task = element.closest('[data-task-id]');
  if (task && root.contains(task)) return task;
  const block = element.closest('div,p,h2,h3,li,blockquote,pre');
  return block && block !== root && root.contains(block) ? block : null;
}

function putCaret(line) {
  root.focus();
  const range = document.createRange();
  const text = [...line.childNodes].find(node => !node.hasAttribute?.('data-editor-ui'));
  if (text) range.setStartBefore(text); else range.selectNodeContents(line);
  range.collapse(true);
  const selection = window.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
  selectedLine = line;
}

function ensureLine() {
  let line = lineAtCursor() || (selectedLine?.isConnected ? selectedLine : null);
  if (!line) {
    root.focus();
    document.execCommand('formatBlock', false, 'div');
    line = lineAtCursor();
  }
  if (!line) {
    line = document.createElement('div');
    line.append(document.createElement('br'));
    root.append(line);
    putCaret(line);
  }
  return line;
}

function clearTask(line) {
  delete line.dataset.taskId;
  delete line.dataset.done;
  delete line.dataset.remindAt;
  line.classList.remove('action-line', 'reminder-due', 'reminder-revealed');
  line.querySelectorAll(':scope > [data-editor-ui]').forEach(element => element.remove());
}

function reminderLabel(line) {
  const time = Number(line.dataset.remindAt);
  if (!time) return 'Remind';
  if (time <= Date.now()) return 'Due';
  const date = new Date(time);
  return date.toLocaleString([], date.toDateString() === new Date().toDateString()
    ? { hour: 'numeric', minute: '2-digit' }
    : { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
}

function decorate() {
  const seen = new Map();
  for (const line of root.querySelectorAll('[data-task-id]')) {
    // Enter can clone block attributes. Keep the reminder on the original
    // text and give a newly created task its own identity.
    const prior = seen.get(line.dataset.taskId);
    if (prior) {
      const replace = !noteContent.plain(prior.innerHTML).trim() && noteContent.plain(line.innerHTML).trim() ? prior : line;
      replace.dataset.taskId = newId();
      delete replace.dataset.remindAt;
      delete replace.dataset.done;
    }
    if (!noteContent.validId(line.dataset.taskId)) line.dataset.taskId = newId();
    seen.set(line.dataset.taskId, line);
    line.classList.add('action-line');
    const done = line.dataset.done === 'true';
    if (done) delete line.dataset.remindAt;
    line.classList.toggle('reminder-due', !done && Number(line.dataset.remindAt) > 0 && Number(line.dataset.remindAt) <= Date.now());
    let check = line.querySelector(':scope > .line-check');
    if (!check) {
      check = document.createElement('button');
      check.type = 'button';
      check.className = 'line-check';
      check.contentEditable = 'false';
      check.dataset.editorUi = '';
      check.dataset.action = 'check';
      check.setAttribute('role', 'checkbox');
      line.prepend(check);
    }
    check.setAttribute('aria-checked', String(done));
    check.setAttribute('aria-label', done ? 'Reopen this line' : 'Complete this line');
    check.textContent = done ? '✓' : '';
    let remind = line.querySelector(':scope > .line-remind');
    if (!remind) {
      remind = document.createElement('button');
      remind.type = 'button';
      remind.className = 'line-remind';
      remind.contentEditable = 'false';
      remind.dataset.editorUi = '';
      remind.dataset.action = 'remind';
      line.append(remind);
    }
    // Contenteditable keeps a trailing <br> as a caret placeholder. The
    // reminder belongs before it, beside the text, rather than on a new row.
    let trailingBreak = null;
    for (let node = line.lastChild; node; node = node.previousSibling) {
      if (node.hasAttribute?.('data-editor-ui') || (node.nodeType === Node.TEXT_NODE && !node.textContent.trim())) continue;
      if (node.nodeName !== 'BR') break;
      trailingBreak = node;
    }
    if (remind.nextSibling !== trailingBreak) line.insertBefore(remind, trailingBreak);
    remind.hidden = done;
    remind.textContent = reminderLabel(line);
    remind.setAttribute('aria-label', `Remind me about ${noteContent.plain(line.innerHTML).split('\n')[0] || 'this line'}`);
    remind.title = line.dataset.remindAt ? new Date(Number(line.dataset.remindAt)).toLocaleString() : 'Set a subtle reminder';
    line.querySelectorAll(':scope > .line-check').forEach(element => { if (element !== check) element.remove(); });
    line.querySelectorAll(':scope > .line-remind').forEach(element => { if (element !== remind) element.remove(); });
  }
}

function notify() {
  decorate();
  const html = noteContent.clean(root.innerHTML);
  dioxus.send({ html, plain: noteContent.plain(html), lines: noteContent.outline(html) });
}

function closePopup() {
  popupLine?.classList.remove('reminder-selection');
  popupHost.replaceChildren();
  popupLine = null;
}

function schedule(time) {
  if (!popupLine?.isConnected) { closePopup(); return; }
  if (time !== null && (!Number.isFinite(time) || time <= Date.now())) return;
  const line = popupLine;
  if (time === null) delete line.dataset.remindAt;
  else {
    if (!line.dataset.taskId) line.dataset.taskId = newId();
    line.dataset.remindAt = String(Math.round(time));
    delete line.dataset.done;
  }
  closePopup();
  notify();
  putCaret(line);
}

function openReminder(line) {
  closePopup();
  popupLine = line;
  line.classList.add('reminder-selection');
  const popup = document.createElement('section');
  popup.className = 'reminder-popover';
  popup.setAttribute('role', 'dialog');
  popup.setAttribute('aria-label', 'Remind me');
  const heading = document.createElement('strong');
  heading.textContent = 'Remind me';
  const description = document.createElement('p');
  description.textContent = 'The floating icon will glow softly when it’s time.';
  const choices = document.createElement('div');
  choices.className = 'reminder-choices';
  for (const [minutes, label] of [[5, 'In 5 min'], [15, 'In 15 min'], [30, 'In 30 min'], [60, 'In 1 hour']]) {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = label;
    button.addEventListener('click', () => schedule(Date.now() + minutes * 60000));
    choices.append(button);
  }
  const custom = document.createElement('div');
  custom.className = 'reminder-custom';
  const input = document.createElement('input');
  input.type = 'datetime-local';
  input.setAttribute('aria-label', 'Reminder date and time');
  const set = document.createElement('button');
  set.type = 'button';
  set.textContent = 'Set';
  set.disabled = true;
  input.addEventListener('input', () => { set.disabled = !(new Date(input.value).getTime() > Date.now()); });
  set.addEventListener('click', () => schedule(new Date(input.value).getTime()));
  custom.append(input, set);
  const actions = document.createElement('div');
  actions.className = 'reminder-popover-actions';
  if (line.dataset.remindAt) {
    const clear = document.createElement('button');
    clear.type = 'button';
    clear.textContent = 'Clear reminder';
    clear.addEventListener('click', () => schedule(null));
    actions.append(clear);
  }
  const cancel = document.createElement('button');
  cancel.type = 'button';
  cancel.textContent = 'Cancel';
  cancel.addEventListener('click', () => { closePopup(); putCaret(line); });
  actions.append(cancel);
  popup.append(heading, description, choices, custom, actions);
  popupHost.replaceChildren(popup);
  choices.firstElementChild.focus();
}

function reveal() {
  const id = root.dataset.revealLine;
  if (!noteContent.validId(id)) return;
  const line = root.querySelector(`[data-task-id="${id}"]`);
  if (!line) { closePopup(); return; }
  root.querySelectorAll('.reminder-revealed').forEach(element => element.classList.remove('reminder-revealed'));
  line.classList.add('reminder-revealed');
  line.scrollIntoView({ block: 'center' });
  putCaret(line);
}

root.addEventListener('input', notify);
root.addEventListener('paste', event => {
  event.preventDefault();
  document.execCommand('insertText', false, event.clipboardData.getData('text/plain'));
  notify();
});
root.addEventListener('drop', event => event.preventDefault());
root.addEventListener('click', event => {
  const button = event.target.closest('[data-action]');
  const line = button?.closest('[data-task-id]');
  if (!line) { closePopup(); return; }
  event.preventDefault();
  if (button.dataset.action === 'check') {
    if (line.dataset.done === 'true') delete line.dataset.done;
    else { line.dataset.done = 'true'; delete line.dataset.remindAt; }
    closePopup();
    notify();
  } else openReminder(line);
});

function toggleTask() {
  const line = ensureLine();
  if (line.dataset.taskId) clearTask(line);
  else line.dataset.taskId = newId();
  closePopup();
  notify();
}

root.addEventListener('keydown', event => {
  if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === 'l') {
    event.preventDefault();
    toggleTask();
  }
  const line = lineAtCursor();
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && line?.dataset.taskId && !noteContent.plain(line.innerHTML).trim()) {
    event.preventDefault();
    clearTask(line);
    notify();
  }
});

toolbar.addEventListener('mousedown', event => { if (event.target.closest('button')) event.preventDefault(); });
toolbar.addEventListener('click', event => {
  const button = event.target.closest('button');
  if (!button) return;
  if (button.dataset.action === 'task') { toggleTask(); return; }
  if (button.dataset.action === 'remind') { openReminder(ensureLine()); return; }
  if (!button.dataset.command) return;
  const line = lineAtCursor();
  const task = line?.dataset.taskId ? {
    taskId: line.dataset.taskId,
    done: line.dataset.done,
    remindAt: line.dataset.remindAt,
  } : null;
  root.focus();
  document.execCommand(button.dataset.command, false, button.dataset.value || null);
  // Formatting a paragraph as a heading/list may replace the block node.
  // Keep the actionable line's identity and deadline on that replacement.
  const replacement = lineAtCursor();
  if (task && replacement && (!line.isConnected || !line.dataset.taskId)) {
    for (const [key, value] of Object.entries(task)) {
      if (value !== undefined) replacement.dataset[key] = value;
    }
  }
  notify();
});

const onSelection = () => { const line = lineAtCursor(); if (line) selectedLine = line; };
const onKey = event => { if (event.key === 'Escape' && popupLine) { const line = popupLine; closePopup(); putCaret(line); } };
document.addEventListener('selectionchange', onSelection);
document.addEventListener('keydown', onKey);
const revealObserver = new MutationObserver(reveal);
root.addEventListener('hitr-reveal-line', reveal);
revealObserver.observe(root, { attributes: true, attributeFilter: ['data-reveal-line'] });
const timer = setInterval(() => { if (root.isConnected) decorate(); }, 15000);
notify();
if (root.dataset.revealLine) reveal(); else root.focus();

// Dispose global listeners when Dioxus switches notes or collapses the window.
await new Promise(resolve => {
  const observer = new MutationObserver(() => {
    if (root.isConnected) return;
    clearInterval(timer);
    document.removeEventListener('selectionchange', onSelection);
    document.removeEventListener('keydown', onKey);
    revealObserver.disconnect();
    observer.disconnect();
    closePopup();
    resolve();
  });
  observer.observe(document.body, { childList: true, subtree: true });
});
