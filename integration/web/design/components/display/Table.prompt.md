Data table — runs, keys, members. Wrapped in a 16px-radius card with clipped corners.

```jsx
<Table
  columns={[{ key: 'name', label: 'Run' },
            { key: 'state', label: 'Status', render: r => <Badge tone={r.tone} dot>{r.state}</Badge> },
            { key: 'tokens', label: 'Tokens', align: 'right' }]}
  rows={rows} />
```

- Numbers always `align: 'right'` so they land in mono and line up.
- Pair with `Pagination` below the card (16px gap), never inside it.
- No zebra striping — rows are separated by hairlines and lit on hover only.
