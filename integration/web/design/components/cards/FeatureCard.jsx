import React from 'react';
export function FeatureCard({ icon, title, children }) {
  return (
    <div className="feature-card">
      {icon ? <div className="feature-icon">{icon}</div> : null}
      <h3>{title}</h3>
      <p>{children}</p>
    </div>
  );
}
