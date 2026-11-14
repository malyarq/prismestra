(() => {
  'use strict';
  const key = 'prismestra.dark-study.appearance.v1';
  const presets = {
    graphite: {base: '#0d0e10', accent: '#c8d6e5'},
    ink: {base: '#101722', accent: '#b9c5e3'},
    light: {base: '#eeefed', accent: '#454f61'}
  };
  const defaults = {version: 1, ...presets.graphite};
  const validHex = value => typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value);
  const valid = value => value && typeof value === 'object' && !Array.isArray(value) &&
    Object.keys(value).length === 3 && value.version === 1 && validHex(value.base) && validHex(value.accent);
  const rgb = hex => [1, 3, 5].map(offset => parseInt(hex.slice(offset, offset + 2), 16));
  const mix = (first, second, amount) => '#' + rgb(first).map((channel, index) =>
    Math.round(channel * (1 - amount) + rgb(second)[index] * amount).toString(16).padStart(2, '0')).join('');
  const luminance = hex => rgb(hex).map(channel => {
    const value = channel / 255;
    return value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4;
  }).reduce((sum, channel, index) => sum + channel * [.2126, .7152, .0722][index], 0);
  const contrast = (first, second) => {
    const a = luminance(first), b = luminance(second);
    return (Math.max(a, b) + .05) / (Math.min(a, b) + .05);
  };
  const foreground = background => contrast(background, '#ffffff') > contrast(background, '#000000') ? '#ffffff' : '#000000';
  function derive(value) {
    const text = foreground(value.base);
    const dark = text === '#ffffff';
    const surface = dark ? '#ffffff' : '#000000';
    const accessibleSurface = amount => {
      while (amount > 0 && contrast(mix(value.base, surface, amount), text) < 4.5) amount = Math.max(0, amount - .005);
      return mix(value.base, surface, amount);
    };
    let muted = mix(value.base, text, .66);
    for (let amount = .68; contrast(value.base, muted) < 4.5 && amount <= 1; amount += .02) muted = mix(value.base, text, amount);
    const tokens = {
      '--ui-bg': value.base,
      '--ui-sidebar': accessibleSurface(.025),
      '--ui-panel': accessibleSurface(dark ? .055 : .015),
      '--ui-raised': accessibleSurface(dark ? .12 : .055),
      '--ui-field': accessibleSurface(dark ? .09 : .075),
      '--ui-border': mix(value.base, text, .2),
      '--ui-text': text,
      '--ui-muted': muted,
      '--ui-accent': value.accent,
      '--ui-on-accent': foreground(value.accent),
      '--ui-selected': accessibleSurface(.08)
    };
    if (value.base.toLowerCase()==='#0d0e10' && value.accent.toLowerCase()==='#c8d6e5') Object.assign(tokens, {'--ui-sidebar':'#121315','--ui-panel':'#191b1e','--ui-raised':'#292c31','--ui-field':'#22252a','--ui-border':'#30343a','--ui-text':'#e9edf1','--ui-muted':'#a3aab4','--ui-on-accent':'#12151a','--ui-selected':'#242c36'});
    const backgrounds = ['--ui-bg', '--ui-sidebar', '--ui-panel', '--ui-raised', '--ui-field', '--ui-selected'].map(key => tokens[key]);
    if (backgrounds.some(background => contrast(background, tokens['--ui-muted']) < 4.5)) {
      for (let amount = .66; amount <= 1.001; amount += .01) {
        const candidate = mix(value.base, tokens['--ui-text'], Math.min(1, amount));
        if (backgrounds.every(background => contrast(background, candidate) >= 4.5)) { tokens['--ui-muted'] = candidate; break; }
      }
    }
    tokens['--ui-control'] = backgrounds.every(background => contrast(background, value.accent) >= 3) ? value.accent : tokens['--ui-text'];
    tokens['--ui-on-control'] = foreground(tokens['--ui-control']);
    return tokens;
  }
  let appearance = {...defaults};
  let notice = '';
  try {
    const saved = localStorage.getItem(key);
    if (saved !== null) {
      const parsed = JSON.parse(saved);
      if (!valid(parsed)) throw Error('Invalid appearance');
      appearance = parsed;
    }
  } catch { notice = 'Настройки недоступны. Используется графитовая тема.'; }
  function apply() {
    const tokens = derive(appearance);
    Object.entries(tokens).forEach(([name, value]) => document.documentElement.style.setProperty(name, value));
    document.documentElement.style.colorScheme = luminance(appearance.base) < .3 ? 'dark' : 'light';
  }
  apply();

  const trigger = document.getElementById('appearance-toggle');
  if (!trigger) return;
  const panel = document.createElement('section');
  panel.id = 'prism-appearance-panel';
  panel.className = 'prism-appearance';
  panel.hidden = true;
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-labelledby', 'prism-appearance-title');
  panel.innerHTML = `
    <div class="prism-appearance-heading"><h2 id="prism-appearance-title">Внешний вид</h2><button type="button" class="prism-appearance-close" aria-label="Закрыть настройку внешнего вида"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18"/></svg></button></div>
    <div class="prism-appearance-presets" aria-label="Готовые темы">
      <button type="button" data-prism-preset="graphite">Тёмный</button>
      <button type="button" data-prism-preset="ink">Чернила</button>
      <button type="button" data-prism-preset="light">Светлая</button>
    </div>
    <label class="prism-appearance-color"><span>Фон</span><output id="prism-appearance-base-value"></output><input type="color" id="prism-appearance-base" aria-label="Цвет фона"></label>
    <label class="prism-appearance-color"><span>Акцент</span><output id="prism-appearance-accent-value"></output><input type="color" id="prism-appearance-accent" aria-label="Цвет акцента"></label>
    <div class="prism-appearance-swatches" aria-label="Цвета акцента">
      ${[['#d4d9e0','Серебристый'],['#b8a4ed','Лиловый'],['#a4ceb7','Шалфей'],['#edaa94','Коралловый']].map(([color,name]) => `<button type="button" data-prism-accent="${color}" style="--prism-swatch:${color}" aria-label="${name}" title="${name}"></button>`).join('')}
    </div>
    <div class="prism-appearance-footer"><button type="button" id="prism-appearance-reset">Сбросить</button><span id="prism-appearance-status" role="status" aria-live="polite"></span></div>`;
  document.body.append(panel);
  trigger.setAttribute('aria-controls', panel.id);
  trigger.setAttribute('aria-haspopup', 'dialog');
  trigger.setAttribute('aria-expanded', 'false');
  const baseInput = panel.querySelector('#prism-appearance-base');
  const accentInput = panel.querySelector('#prism-appearance-accent');
  const status = panel.querySelector('#prism-appearance-status');
  function sync() {
    baseInput.value = appearance.base;
    accentInput.value = appearance.accent;
    panel.querySelector('#prism-appearance-base-value').textContent = appearance.base.toUpperCase();
    panel.querySelector('#prism-appearance-accent-value').textContent = appearance.accent.toUpperCase();
    panel.querySelectorAll('[data-prism-preset]').forEach(button => {
      const preset = presets[button.dataset.prismPreset];
      button.setAttribute('aria-pressed', String(preset.base === appearance.base.toLowerCase() && preset.accent === appearance.accent.toLowerCase()));
    });
    panel.querySelectorAll('[data-prism-accent]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.prismAccent === appearance.accent.toLowerCase())));
  }
  function save() {
    apply();
    sync();
    try { localStorage.setItem(key, JSON.stringify(appearance)); notice = ''; status.textContent = 'Сохранено'; }
    catch { notice = 'Тема не сохранена. Проверьте доступ к хранилищу и повторите выбор.'; status.textContent = notice; }
  }
  function close(restoreFocus = true) {
    panel.hidden = true;
    trigger.setAttribute('aria-expanded', 'false');
    if (restoreFocus) trigger.focus();
  }
  trigger.addEventListener('click', () => {
    if (!panel.hidden) { close(); return; }
    sync();
    status.textContent = notice;
    panel.hidden = false;
    trigger.setAttribute('aria-expanded', 'true');
    panel.querySelector('[data-prism-preset]').focus();
  });
  panel.querySelector('.prism-appearance-close').addEventListener('click', () => close());
  document.addEventListener('keydown', event => {
    if (!panel.hidden && event.key === 'Escape') { event.preventDefault(); close(); }
  });
  document.addEventListener('click', event => {
    if (!panel.hidden && !panel.contains(event.target) && !trigger.contains(event.target)) close(false);
  });
  document.addEventListener('focusin', event => {
    if (!panel.hidden && !panel.contains(event.target) && !trigger.contains(event.target)) close(false);
  });
  panel.addEventListener('click', event => {
    const presetButton = event.target.closest('[data-prism-preset]');
    const accentButton = event.target.closest('[data-prism-accent]');
    if (presetButton) { appearance = {version: 1, ...presets[presetButton.dataset.prismPreset]}; save(); }
    if (accentButton) { appearance.accent = accentButton.dataset.prismAccent; save(); }
  });
  baseInput.addEventListener('input', () => { if (validHex(baseInput.value)) { appearance.base = baseInput.value; save(); } });
  accentInput.addEventListener('input', () => { if (validHex(accentInput.value)) { appearance.accent = accentInput.value; save(); } });
  panel.querySelector('#prism-appearance-reset').addEventListener('click', () => { appearance = {...defaults}; save(); });
  sync();
})();
