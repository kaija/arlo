Dropdown select — native `<select>` under brand chrome, so it keeps OS keyboard and mobile behaviour.

```jsx
<Select label="Model" options={['arlo-lite', 'arlo-rust', 'agui']} defaultValue="arlo-rust" />
<Select label="Visibility" size="sm" options={[{ value: 'pub', label: 'Public' }, { value: 'priv', label: 'Private' }]} />
```

- For multi-select or search-in-list, don't extend this — those aren't defined in the system yet; ask before inventing one.
- The chevron is `Icon name="chevron-down"` at 16px, `--text-faint`.
