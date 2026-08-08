Spinner — indeterminate waiting, inline or in a button.

```jsx
<Spinner />
<Button variant="primary" disabled><Spinner size="sm" onAccent /> Generating…</Button>
```

- Inside a primary button always pass `onAccent` — a border-coloured ring disappears on indigo.
- One spinner per view; for whole-page loads prefer skeleton-free empty state + `Progress`.
