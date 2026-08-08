Radio button — one choice from a small, mutually exclusive set.

```jsx
<div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
  <Radio name="key" value="byo" label="Bring your own key" defaultChecked />
  <Radio name="key" value="local" label="Local model only" />
</div>
```

- Always share one `name` across the group. Two or three options → radios; more → `Select`.
