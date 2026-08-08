One-line: the brand's 24px/2px-stroke line icon set for AI-app actions — submit, voice, image, attach, regenerate, feedback, and chrome.

```jsx
<Icon name="voice" aria-label="Record voice input" />
<button className="btn btn-primary"><Icon name="submit" size={20} /> Send</button>
<span style={{ color: 'var(--color-accent)' }}><Icon name="zap" /></span>
```

- **Color** comes from `currentColor` — set it on the parent (`var(--color-accent)` for active/primary, `var(--text-muted)` for idle composer affordances).
- **Sizes**: 24 default, 20 inside `size="sm"` buttons, 16 inline with body text, 44px tinted `radius-sm` box for feature tiles (matches `FeatureCard`).
- **Pairs**: `submit`/`stop` swap on the same button while streaming; `thumbs-up`/`thumbs-down` and `copy`/`regenerate` always ship together in a message action row.
- Every glyph also exists standalone in `assets/icons/<name>.svg` — copy those into static HTML instead of inlining the component.

- The full id list is exported as `IconNames`; raw inner markup as `IconPaths`.
