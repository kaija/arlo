import React from 'react';
import { Icon } from '../icons/Icon.jsx';
function pages(page, total) {
  if (total <= 7) return Array.from({ length: total }, (_, i) => i + 1);
  if (page <= 4) return [1, 2, 3, 4, 5, 'gap', total];
  if (page >= total - 3) return [1, 'gap', total - 4, total - 3, total - 2, total - 1, total];
  return [1, 'gap', page - 1, page, page + 1, 'gap2', total];
}
export function Pagination({ page = 1, total = 1, onChange, meta }) {
  const go = p => onChange && onChange(p);
  return (
    <nav className="pagination" aria-label="Pagination">
      {meta && <span className="pagination-meta">{meta}</span>}
      <button type="button" className="page-btn" disabled={page <= 1} onClick={() => go(page - 1)} aria-label="Previous page"><Icon name="chevron-left" size={16} /></button>
      {pages(page, total).map(p => typeof p === 'number'
        ? <button key={p} type="button" className="page-btn" aria-current={p === page ? 'page' : undefined} onClick={() => go(p)}>{p}</button>
        : <span key={p} className="page-gap">···</span>)}
      <button type="button" className="page-btn" disabled={page >= total} onClick={() => go(page + 1)} aria-label="Next page"><Icon name="chevron-right" size={16} /></button>
    </nav>
  );
}
