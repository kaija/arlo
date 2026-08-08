import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Tabs({ tabs = [], value, onChange, variant }) {
  return (
    <div className={cx('tabs', variant === 'pill' && 'tabs-pill')} role="tablist">
      {tabs.map(t => {
        const tab = typeof t === 'string' ? { id: t, label: t } : t;
        return (
          <button key={tab.id} type="button" role="tab" className="tab" aria-selected={value === tab.id}
            onClick={() => onChange && onChange(tab.id)}>
            {tab.label}
            {tab.count != null && <span className="tab-count">{tab.count}</span>}
          </button>
        );
      })}
    </div>
  );
}
