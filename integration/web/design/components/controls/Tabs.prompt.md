Tabs — switching views within one screen.

```jsx
<Tabs tabs={[{ id: 'runs', label: 'Runs', count: 12 }, { id: 'keys', label: 'API keys' }]}
  value={tab} onChange={setTab} />
<Tabs variant="pill" tabs={['Day', 'Week', 'Month']} value={range} onChange={setRange} />
```

- Underline for page-level sections; `variant="pill"` for small in-card scopes (its chrome is the `LangSwitch` pattern).
- Max ~5 underline tabs; beyond that use a sidebar.
