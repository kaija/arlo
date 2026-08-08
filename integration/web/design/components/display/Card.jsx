import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Card({ title, subtitle, action, footer, padding, interactive, children, ...rest }) {
  return (
    <div className={cx('card', padding === 'sm' && 'card-sm', interactive && 'card-interactive')} {...rest}>
      {(title || action) && (
        <div className="card-header">
          <div>
            {title && <h3 className="card-title">{title}</h3>}
            {subtitle && <p className="card-sub">{subtitle}</p>}
          </div>
          {action}
        </div>
      )}
      {children}
      {footer && <div className="card-footer">{footer}</div>}
    </div>
  );
}
