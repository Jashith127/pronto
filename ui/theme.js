/* Shared theme resolver for all Pronto WebViews */

(function () {
  const media = window.matchMedia('(prefers-color-scheme: dark)');
  let preference = 'system';

  function normalizePreference(value) {
    const raw = String(value || 'system').toLowerCase();
    if (raw === 'light' || raw === 'dark' || raw === 'system') return raw;
    return 'system';
  }

  function resolveTheme(pref) {
    const normalized = normalizePreference(pref);
    if (normalized === 'light') return 'light';
    if (normalized === 'dark') return 'dark';
    return media.matches ? 'dark' : 'light';
  }

  function applyTheme(pref) {
    preference = normalizePreference(pref ?? preference);
    const resolved = resolveTheme(preference);
    const root = document.documentElement;
    root.dataset.theme = resolved;
    root.dataset.themePreference = preference;
    root.style.colorScheme = resolved;
    return resolved;
  }

  function initTheme(pref) {
    applyTheme(pref);
    if (!media.__prontoThemeBound) {
      media.addEventListener('change', () => {
        if (preference === 'system') applyTheme('system');
      });
      media.__prontoThemeBound = true;
    }
  }

  window.ProntoTheme = {
    applyTheme,
    initTheme,
    resolveTheme,
    getPreference: () => preference
  };
})();
