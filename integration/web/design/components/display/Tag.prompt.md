Tag / chip — labels the user added, and filter toggles.

```jsx
<Tag onRemove={() => drop('rust')}>rust</Tag>
<Tag tone="accent" onClick={toggle}>MCP</Tag>
```

- `onClick` alone → filter chip; `onRemove` alone → removable label; both is allowed but rare.
- Sans-serif, unlike `Pill` (mono) — reach for `Pill` for stack/protocol names.
