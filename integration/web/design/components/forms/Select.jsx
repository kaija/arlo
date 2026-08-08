import React from 'react';
import { Icon } from '../icons/Icon.jsx';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Select({ label, hint, error, options = [], size, id, ...rest }) {
  const selId = id || 'sel-' + (label || 'field').replace(/\W+/g, '-').toLowerCase();
  return (
    <div className="field">
      {label && <label className="field-label" htmlFor={selId}>{label}</label>}
      <div className="select-wrap">
        <select id={selId} className={cx('input', 'select', size === 'sm' && 'input-sm', error && 'input-error')} {...rest}>
          {options.map(o => {
            const opt = typeof o === 'string' ? { value: o, label: o } : o;
            return <option key={opt.value} value={opt.value}>{opt.label}</option>;
          })}
        </select>
        <span className="select-chevron"><Icon name="chevron-down" size={16} /></span>
      </div>
      {(error || hint) && <p className={cx('field-hint', error && 'field-hint-error')}>{error || hint}</p>}
    </div>
  );
}
