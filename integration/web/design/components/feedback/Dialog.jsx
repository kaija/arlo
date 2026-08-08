import React from 'react';
import { Icon } from '../icons/Icon.jsx';
export function Dialog({ open = true, title, icon, onClose, actions, children }) {
  if (!open) return null;
  return (
    <div className="dialog-overlay" onClick={onClose}>
      <div className="dialog" role="dialog" aria-modal="true" aria-label={title} onClick={e => e.stopPropagation()}>
        <div className="dialog-head">
          {icon && <span className="feature-icon" style={{ width: 36, height: 36, marginBottom: 0 }}><Icon name={icon} size={18} /></span>}
          {title && <h3 className="dialog-title">{title}</h3>}
          {onClose && <button type="button" className="dialog-close" aria-label="Close" onClick={onClose}><Icon name="close" size={18} /></button>}
        </div>
        <div className="dialog-body">{children}</div>
        {actions && <div className="dialog-actions">{actions}</div>}
      </div>
    </div>
  );
}
