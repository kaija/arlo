Checkbox — for independent on/off choices in a list.

```jsx
<Checkbox label="Stream responses token by token" defaultChecked />
<Checkbox label="Send telemetry" disabled />
```

- Stack multiple in a `display:flex; flex-direction:column; gap:12px` group; never rely on source whitespace.
- Use `Switch` instead when the change applies instantly, `Checkbox` when it applies on submit.
