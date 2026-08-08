import React from 'react';
import { Icon } from '../icons/Icon.jsx';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Checkbox({ label, disabled, ...rest }) {
  return (
    <label className={cx('check', disabled && 'check-disabled')}>
      <input type="checkbox" disabled={disabled} {...rest} />
      <span className="check-box"><Icon className="check-tick" name="check" size={12} strokeWidth={3} /></span>
      {label && <span>{label}</span>}
    </label>
  );
}
