Generic content card — the product-side sibling of `FeatureCard`/`ProjectCard`.

```jsx
<Card title="Usage" subtitle="Current billing period" action={<Badge tone="success">Healthy</Badge>}
  footer={<Button size="sm">View details</Button>}>
  <Progress label="Tokens" value={68} />
</Card>
```

- Only pass `interactive` when the whole card is a link/button — the −2px lift signals clickability.
- Never nest a Card inside a Card; switch the inner one to a bordered row instead.
