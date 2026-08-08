import React from 'react';
const cx = (...a) => a.filter(Boolean).join(' ');
export function Table({ columns = [], rows = [] }) {
  return (
    <div className="table-wrap">
      <table className="table">
        <thead>
          <tr>{columns.map(c => <th key={c.key} className={cx(c.align === 'right' && 'table-num')} style={c.width ? { width: c.width } : undefined}>{c.label}</th>)}</tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={row.id != null ? row.id : i}>
              {columns.map(c => <td key={c.key} className={cx(c.align === 'right' && 'table-num')}>{c.render ? c.render(row) : row[c.key]}</td>)}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
