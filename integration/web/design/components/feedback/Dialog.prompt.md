Modal dialog — confirmation and short focused forms.

```jsx
<Dialog open={open} title="Delete this chat?" icon="trash" onClose={close}
  actions={<><Button variant="secondary" onClick={close}>Cancel</Button>
             <Button variant="primary" onClick={del}>Delete</Button></>}>
  This removes the transcript from local storage. It can't be undone.
</Dialog>
```

- Title is a question or a noun phrase; body explains the consequence in one or two sentences.
- Destructive confirm still uses the accent primary button — the system has no red button; the copy carries the warning.
- Scrim is `--color-scrim` + 4px blur; the whole overlay is the click-out target.
