Toast — transient confirmation of something the user just did.

```jsx
<ToastStack>
  <Toast tone="success" actionLabel="Undo" onAction={undo}>Chat deleted</Toast>
</ToastStack>
```

- One short clause, no period, no emoji: "Chat deleted", "Key copied".
- Light surface (not a dark pill) so it matches the rest of the system; `shadow-lg` carries the elevation.
- Auto-dismiss ~4s; anything the user must act on belongs in an `Alert` or `Dialog`.
