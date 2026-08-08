import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Tooltip({ label, placement, children }) {
  return (
    <span className="tooltip-wrap" tabIndex={0}>
      {children}
      <span role="tooltip" className={cx('tooltip', placement === 'bottom' && 'tooltip-bottom')}>{label}</span>
    </span>
  );
}
