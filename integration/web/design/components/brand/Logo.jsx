import React from 'react';
const MARKS = {
  arlo: [<path key="a" d="M8 20L14 8L20 20" stroke="white" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"/>, <path key="b" d="M10.6 15.4H17.4" stroke="white" strokeWidth="2.5" strokeLinecap="round"/>],
  rust: [<path key="a" d="M8 9L13 14L8 19" stroke="white" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"/>, <path key="b" d="M15 19H20" stroke="white" strokeWidth="2.5" strokeLinecap="round"/>],
  lite: [<path key="a" d="M8 20L14 8L20 20" stroke="white" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"/>, <path key="b" d="M10 16H18" stroke="white" strokeWidth="2" strokeLinecap="round"/>],
  agui: [<path key="a" d="M6 14h4l2-6 4 12 2-6h4" stroke="white" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"/>]
};
export function Logo({ project = 'arlo', size = 28, withWordmark = false, className }) {
  const NAMES = { arlo: 'Arlo AI', rust: 'Arlo Rust', lite: 'Arlo Lite', agui: 'AG-UI Rust' };
  const svg = (
    <svg className={className} width={size} height={size} viewBox="0 0 28 28" fill="none" aria-hidden="true">
      <rect width="28" height="28" rx="7" fill="#5856D6"/>
      {MARKS[project] || MARKS.arlo}
    </svg>
  );
  if (!withWordmark) return svg;
  return (
    <span style={{ display: 'inline-flex', alignItems: 'center', gap: 10, fontWeight: 600, fontSize: '1.125rem', letterSpacing: '-0.01em' }}>
      {svg}<span>{NAMES[project] || NAMES.arlo}</span>
    </span>
  );
}
