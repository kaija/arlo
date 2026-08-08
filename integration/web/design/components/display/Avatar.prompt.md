Avatar — people and orgs in headers, tables, message rows.

```jsx
<Avatar name="Kai Jiang" status="online" />
<Avatar name="Arlo Rust" shape="square" size="lg" />
<AvatarGroup><Avatar name="A B" size="sm" /><Avatar name="C D" size="sm" /></AvatarGroup>
```

- Initials only — never emoji, never a generic person glyph, unless you pass `Icon name="user"` as a child yourself.
- Round for people, `shape="square"` for projects/orgs (echoes the brand mark's rounded square).
