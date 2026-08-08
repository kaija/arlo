import React from 'react';
import { Icon } from '../icons/Icon.jsx';
const cx = (...a) => a.filter(Boolean).join(' ');
const toneIcon = { info: 'info', success: 'check-circle', warning: 'alert-triangle', danger: 'alert-circle' };
export function Toast({ tone = 'info', actionLabel, onAction, children }) {
  return (
    <div className={cx('toast', tone !== 'info' && 'toast-' + tone)} role="status">
      <span className="toast-icon"><Icon name={toneIcon[tone]} size={18} /></span>
      <span>{children}</span>
      {actionLabel && <button type="button" className="toast-action" onClick={onAction}>{actionLabel}</button>}
    </div>
  );
}
export function ToastStack({ children }) {
  return <div className="toast-stack">{children}</div>;
}
