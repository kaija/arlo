import React from 'react';
import { Logo } from '../brand/Logo.jsx';
import { Pill } from '../core/Pill.jsx';
export function ProjectCard({ project = 'arlo', title, pills = [], siteHref, repoHref, siteLabel = 'Visit site →', repoLabel = 'GitHub', children }) {
  return (
    <div className="project-card">
      <Logo project={project} size={40} className="project-logo" />
      <h3>{title}</h3>
      <p>{children}</p>
      {pills.length ? <div className="project-pills">{pills.map((p, i) => <Pill key={i}>{p}</Pill>)}</div> : null}
      <div className="project-links">
        {siteHref ? <a href={siteHref}>{siteLabel}</a> : null}
        {repoHref ? <a href={repoHref} className="link-muted" target="_blank" rel="noopener">{repoLabel}</a> : null}
      </div>
    </div>
  );
}
