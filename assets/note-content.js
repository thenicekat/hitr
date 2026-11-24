// The note HTML is the source of truth. Transient editor controls are never saved.
const noteContent = (() => {
  const allowed = new Set(['B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'H2', 'H3', 'P', 'DIV', 'UL', 'OL', 'LI', 'BR', 'BLOCKQUOTE', 'CODE', 'PRE']);
  const blocks = new Set(['DIV', 'P', 'H2', 'H3', 'LI', 'BLOCKQUOTE', 'PRE']);
  const validId = id => /^[a-zA-Z0-9_-]{1,80}$/.test(id || '');
  const validTime = value => /^\d{1,16}$/.test(value || '') && Number(value) > 0 && Number(value) <= 8640000000000000;

  function clean(html) {
    const parsed = new DOMParser().parseFromString(html, 'text/html');
    const output = document.createElement('div');
    function copy(source, destination) {
      for (const child of source.childNodes) {
        if (child.nodeType === Node.TEXT_NODE) destination.append(document.createTextNode(child.textContent));
        else if (child.nodeType === Node.ELEMENT_NODE) {
          if (child.hasAttribute('data-editor-ui') || ['SCRIPT', 'STYLE', 'IFRAME', 'OBJECT'].includes(child.tagName)) continue;
          if (allowed.has(child.tagName)) {
            const element = document.createElement(child.tagName.toLowerCase());
            if (blocks.has(child.tagName) && validId(child.dataset.taskId)) {
              element.dataset.taskId = child.dataset.taskId;
              if (child.dataset.done === 'true') element.dataset.done = 'true';
              if (validTime(child.dataset.remindAt) && child.dataset.done !== 'true') element.dataset.remindAt = child.dataset.remindAt;
            }
            copy(child, element);
            destination.append(element);
          } else copy(child, destination);
        }
      }
    }
    copy(parsed.body, output);
    return output.innerHTML;
  }

  function plain(html) {
    const root = new DOMParser().parseFromString(clean(html), 'text/html').body;
    function text(node) {
      if (node.nodeType === Node.TEXT_NODE) return node.textContent;
      if (node.nodeName === 'BR') return '\n';
      let output = '';
      for (const child of node.childNodes) {
        const block = blocks.has(child.nodeName) || ['UL', 'OL'].includes(child.nodeName);
        if (block && output && !output.endsWith('\n')) output += '\n';
        output += text(child);
        if (block && !output.endsWith('\n')) output += '\n';
      }
      return output;
    }
    return text(root).trimEnd();
  }

  function outline(html) {
    const root = new DOMParser().parseFromString(clean(html), 'text/html').body;
    return [...root.querySelectorAll('[data-task-id]')].map(line => ({
      id: line.dataset.taskId,
      text: plain(line.innerHTML).split('\n').find(text => text.trim())?.trim() || 'Untitled task',
      done: line.dataset.done === 'true',
      remind_at: line.dataset.remindAt ? Number(line.dataset.remindAt) : null,
    }));
  }

  return { clean, plain, outline, validId };
})();
