import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Radio({ label, disabled, ...rest }) {
  return (
    <label className={cx('check', disabled && 'check-disabled')}>
      <input type="radio" disabled={disabled} {...rest} />
      <span className="check-box check-box-round"><span className="check-dot" /></span>
      {label && <span>{label}</span>}
    </label>
  );
}
