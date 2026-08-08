import React from 'react';
import { Logo } from '../brand/Logo.jsx';
import { GitHubIcon } from '../brand/GitHubIcon.jsx';
import { Button } from '../core/Button.jsx';
import { LangSwitch } from './LangSwitch.jsx';
export function Nav({ links = [], lang = 'en', onLangChange, githubHref = 'https://github.com/kaija', fixed = true }) {
  return (
    <nav className="nav" aria-label="Main navigation" style={fixed ? undefined : { position: 'static' }}>
      <div className="nav-inner container">
        <a href="/" className="nav-brand" aria-label="Arlo AI home"><Logo className="nav-logo" /><span>Arlo AI</span></a>
        <div className="nav-links">
          {links.map((l, i) => <a key={i} href={l.href}>{l.label}</a>)}
          {onLangChange ? <LangSwitch current={lang} onChange={onLangChange} /> : null}
          <Button size="sm" href={githubHref}><GitHubIcon /> GitHub</Button>
        </div>
        <button className="nav-toggle" aria-label="Toggle menu" aria-expanded="false">
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M3 12h18M3 6h18M3 18h18"/></svg>
        </button>
      </div>
    </nav>
  );
}
