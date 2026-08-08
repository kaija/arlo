import React from 'react';
export function StackNode({ title, subtitle, accent = false }) {
  return (
    <div className={accent ? 'stack-node stack-node-accent' : 'stack-node'}>
      <strong>{title}</strong>
      <span>{subtitle}</span>
    </div>
  );
}
