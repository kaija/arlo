import React from 'react';
export function LangSwitch({ langs = [{ code: 'en', label: 'EN' }, { code: 'zh-Hant', label: '繁中' }, { code: 'ja', label: '日本語' }], current = 'en', onChange }) {
  return (
    <div className="lang-switch" role="group" aria-label="Select language">
      {langs.map((l) => (
        <button key={l.code} type="button" data-lang={l.code} aria-current={String(l.code === current)} onClick={() => onChange && onChange(l.code)}>{l.label}</button>
      ))}
    </div>
  );
}
