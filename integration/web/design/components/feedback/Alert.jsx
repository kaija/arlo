import React from 'react';
import { Icon } from '../icons/Icon.jsx';
const cx = (...a) => a.filter(Boolean).join(' ');
const toneIcon = { info: 'info', success: 'check-circle', warning: 'alert-triangle', danger: 'alert-circle' };
export function Alert({ tone = 'info', title, onClose, children }) {
  return (
    <div className={cx('alert', tone !== 'info' && 'alert-' + tone)} role={tone === 'danger' ? 'alert' : 'status'}>
      <span className="alert-icon"><Icon name={toneIcon[tone]} size={18} /></span>
      <div>
        {title && <div className="alert-title">{title}</div>}
        {children && <div className="alert-body">{children}</div>}
      </div>
      {onClose && <button type="button" className="alert-close" aria-label="Dismiss" onClick={onClose}><Icon name="close" size={16} /></button>}
    </div>
  );
}
