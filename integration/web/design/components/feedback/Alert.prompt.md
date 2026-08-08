Inline alert — persistent, in-flow messages tied to the surrounding content.

```jsx
<Alert tone="warning" title="No API key set">Add a key in Settings to run remote models.</Alert>
<Alert tone="danger" title="Run failed" onClose={dismiss}>The model returned a 429.</Alert>
```

- Icons are fixed per tone (info/check-circle/alert-triangle/alert-circle) — don't override them.
- In-flow and permanent; for transient confirmation use `Toast`.
