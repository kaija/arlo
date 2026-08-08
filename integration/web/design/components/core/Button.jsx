import React from 'react';
export function Button({ variant, size, href, children, ...rest }) {
  const cls = ['btn', size === 'sm' && 'btn-sm', variant === 'primary' && 'btn-primary', variant === 'secondary' && 'btn-secondary'].filter(Boolean).join(' ');
  if (href) {
    const ext = /^https?:/.test(href);
    return <a className={cls} href={href} {...(ext ? { target: '_blank', rel: 'noopener' } : {})} {...rest}>{children}</a>;
  }
  return <button type="button" className={cls} {...rest}>{children}</button>;
}
