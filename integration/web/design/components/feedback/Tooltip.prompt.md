Tooltip — names an icon-only control or explains a truncated value.

```jsx
<Tooltip label="Regenerate response"><button className="tag-remove"><Icon name="regenerate" /></button></Tooltip>
<Tooltip label="Copied to clipboard" placement="bottom"><Badge>sk-…4f2</Badge></Tooltip>
```

- CSS-only (hover + focus-within), so it works without JS but does not flip at viewport edges — keep it off screen edges.
- `white-space: nowrap`: a few words maximum. Longer explanation → `Alert` or hint text.
