import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Spinner({ size, onAccent, label, ...rest }) {
  return <span className={cx('spinner', size && 'spinner-' + size, onAccent && 'spinner-on-accent')} role="status" aria-label={label || 'Loading'} {...rest} />;
}
