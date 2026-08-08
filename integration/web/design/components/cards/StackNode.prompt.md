Architecture-diagram node. Arlo-owned layers get `accent`; the user's layers stay plain.

```jsx
<div className="stack-flow">
  <StackNode title="Web · Desktop · Mobile frontends" subtitle="Whatever you want to build" />
  <div className="stack-arrow" aria-hidden="true">↕</div>
  <StackNode accent title="AG-UI Rust" subtitle="The standard AG-UI interface" />
</div>
```

Arrows are unicode ↕ / → in `.stack-arrow`; `.stack-divider` renders an "or" rule between flows.
