import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Badge({ tone, dot, children, ...rest }) {
  return (
    <span className={cx('badge', tone && tone !== 'accent' && 'badge-' + tone)} {...rest}>
      {dot && <span className="badge-dot" />}
      {children}
    </span>
  );
}
