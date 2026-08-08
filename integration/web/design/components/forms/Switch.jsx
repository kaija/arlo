import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Switch({ label, size, disabled, ...rest }) {
  return (
    <label className={cx('switch', size === 'sm' && 'switch-sm', disabled && 'check-disabled')}>
      <input type="checkbox" role="switch" disabled={disabled} {...rest} />
      <span className="switch-track"><span className="switch-knob" /></span>
      {label && <span>{label}</span>}
    </label>
  );
}
