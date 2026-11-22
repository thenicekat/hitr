const applyTheme = (preferences) => {
  document.documentElement.dataset.theme = preferences.dark ? 'dark' : 'light';
};
// Subscribe before reading, so a change in the notes window isn't missed at startup.
await window.__TAURI__.event.listen('notes-preferences-changed', event => applyTheme(event.payload));
try {
  applyTheme(await window.__TAURI__.core.invoke('load_notes_preferences'));
} catch (error) {
  console.error('Could not load appearance', error);
}
