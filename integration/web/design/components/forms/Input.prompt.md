Labelled text field — the default input for every Arlo form; also renders as a textarea.

```jsx
<Input label="Workspace name" placeholder="acme-research" hint="Lowercase, no spaces." />
<Input label="Search" icon="search" placeholder="Search projects" size="sm" />
<Input label="API key" error="That key was rejected." defaultValue="sk-…" />
<Input label="System prompt" as="textarea" rows={4} />
```

- Focus ring is always `0 0 0 3px var(--color-accent-light)` + accent border — never a browser outline.
- Set `error` instead of `hint`; the component swaps them and reddens the border.
- Disabled fields sit on `--color-bg-secondary`; don't grey the label.
