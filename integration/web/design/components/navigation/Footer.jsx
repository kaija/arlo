import React from 'react';
import { Logo } from '../brand/Logo.jsx';
export function Footer({ links = [] }) {
  return (
    <footer className="footer">
      <div className="container">
        <div className="footer-inner">
          <div className="footer-brand"><Logo size={24} className="nav-logo" /><span>Arlo AI</span></div>
          <div className="footer-links">
            {links.map((l, i) => <a key={i} href={l.href} {...(/^https?:/.test(l.href) ? { target: '_blank', rel: 'noopener' } : {})}>{l.label}</a>)}
          </div>
        </div>
      </div>
    </footer>
  );
}
