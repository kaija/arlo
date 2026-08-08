import React from 'react';
export function Slider({ label, value = 0, min = 0, max = 100, showValue = true, format, ...rest }) {
  const pct = ((value - min) / (max - min)) * 100;
  const fill = 'linear-gradient(90deg, var(--color-accent) ' + pct + '%, var(--color-bg-tertiary) ' + pct + '%)';
  return (
    <div className="field">
      {label && <label className="field-label">{label}</label>}
      <div className="slider-row">
        <input type="range" className="slider" style={{ background: fill }} value={value} min={min} max={max} {...rest} />
        {showValue && <span className="slider-value">{format ? format(value) : value}</span>}
      </div>
    </div>
  );
}
