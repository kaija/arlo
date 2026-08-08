import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Avatar({ name = '', src, size, shape, status, ...rest }) {
  const initials = name.trim().split(/\s+/).slice(0, 2).map(w => w[0]).join('').toUpperCase();
  return (
    <span className={cx('avatar', size && 'avatar-' + size, shape === 'square' && 'avatar-square')} title={name || undefined} {...rest}>
      {src ? <img src={src} alt={name} /> : initials}
      {status && <span className={cx('avatar-status', status !== 'online' && 'avatar-status-' + status)} />}
    </span>
  );
}
export function AvatarGroup({ children }) {
  return <span className="avatar-group">{children}</span>;
}
