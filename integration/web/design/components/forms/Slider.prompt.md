Slider — continuous numeric settings (temperature, top-p, context length).

```jsx
<Slider label="Temperature" min={0} max={2} step={0.1} value={t}
  format={v => Number(v).toFixed(1)} onChange={e => setT(+e.target.value)} />
```

- The fill is painted as a gradient from `value`, so it must be a controlled input.
- Readout is always `--font-mono` at 13px, right-aligned in a 44px column so it doesn't jitter.
