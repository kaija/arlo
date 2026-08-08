import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Progress({ value = 0, max = 100, label, tone, indeterminate, showValue = true }) {
  const pct = Math.max(0, Math.min(100, (value / max) * 100));
  return (
    <div className={cx(indeterminate && 'progress-indeterminate')}>
      {(label || (showValue && !indeterminate)) && (
        <div className="progress-head">
          <span>{label}</span>
          {showValue && !indeterminate && <span>{Math.round(pct)}%</span>}
        </div>
      )}
      <div className="progress" role="progressbar" aria-valuenow={indeterminate ? undefined : Math.round(pct)}>
        <div className={cx('progress-bar', tone && 'progress-bar-' + tone)} style={{ width: indeterminate ? undefined : pct + '%' }} />
      </div>
    </div>
  );
}
