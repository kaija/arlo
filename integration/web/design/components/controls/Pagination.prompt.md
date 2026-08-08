Pagination — below a `Table` or card list.

```jsx
<Pagination page={p} total={13} meta="1–20 of 248" onChange={setP} />
```

- Truncates to 7 slots with `···`; arrows disable (not hide) at the ends.
- Right-align under the table, 16px gap. Don't combine with infinite scroll.
