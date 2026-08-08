Progress bar — quota, upload, multi-step completion.

```jsx
<Progress label="Context used" value={72} />
<Progress label="Indexing repo" indeterminate />
<Progress value={96} tone="danger" label="Rate limit" />
```

- Switch to `tone="danger"` above ~90% of a hard limit; `success` only when the task finished.
- For an inline "working" state with no measurable progress, use `Spinner`, not an indeterminate bar.
