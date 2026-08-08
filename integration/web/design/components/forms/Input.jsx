import React from 'react';
import { Icon } from '../icons/Icon.jsx';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Input({ label, hint, error, icon, size, as, id, ...rest }) {
  const inputId = id || 'in-' + (label || 'field').replace(/\W+/g, '-').toLowerCase();
  const Tag = as === 'textarea' ? 'textarea' : 'input';
  const field = <Tag id={inputId} className={cx('input', size === 'sm' && 'input-sm', error && 'input-error')} {...rest} />;
  return (
    <div className="field">
      {label && <label className="field-label" htmlFor={inputId}>{label}</label>}
      {icon ? <div className="input-wrap"><span className="input-icon"><Icon name={icon} size={16} /></span>{field}</div> : field}
      {(error || hint) && <p className={cx('field-hint', error && 'field-hint-error')}>{error || hint}</p>}
    </div>
  );
}
