const root = document.getElementById('note-editor');
const toolbar = root.previousElementSibling;
const allowed = new Set(['B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'H2', 'H3', 'P', 'DIV', 'UL', 'OL', 'LI', 'BR', 'BLOCKQUOTE', 'CODE', 'PRE']);
function clean(html) {
  const parsed = new DOMParser().parseFromString(html, 'text/html');
  const output = document.createElement('div');
  function copy(source, destination) {
    for (const child of source.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) destination.append(document.createTextNode(child.textContent));
      else if (child.nodeType === Node.ELEMENT_NODE) {
        if (['SCRIPT', 'STYLE', 'IFRAME', 'OBJECT'].includes(child.tagName)) continue;
        if (allowed.has(child.tagName)) {
          const element = document.createElement(child.tagName.toLowerCase());
          copy(child, element);
          destination.append(element);
        } else copy(child, destination);
      }
    }
  }
  copy(parsed.body, output);
  return output.innerHTML;
}
root.innerHTML = clean(await dioxus.recv());
document.execCommand('styleWithCSS', false, false);
const notify = () => dioxus.send({ html: clean(root.innerHTML), plain: root.innerText });
root.addEventListener('input', notify);
// Paste as text so content from a web page can't import its styles or active HTML.
root.addEventListener('paste', event => {
  event.preventDefault();
  document.execCommand('insertText', false, event.clipboardData.getData('text/plain'));
  notify();
});
root.addEventListener('drop', event => event.preventDefault());
toolbar.addEventListener('mousedown', event => {
  if (event.target.closest('button')) event.preventDefault();
});
toolbar.addEventListener('click', event => {
  const button = event.target.closest('[data-command]');
  if (!button) return;
  root.focus();
  document.execCommand(button.dataset.command, false, button.dataset.value || null);
  notify();
});
root.focus();
notify();
// Keep the Dioxus message channel alive while this editor is mounted.
await new Promise(() => {});
