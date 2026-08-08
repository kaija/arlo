Segmented control for the site's three locales, lives in the nav.

```jsx
<LangSwitch current={lang} onChange={setLang} />
```

Defaults to EN / 繁中 / 日本語. No auto language sniffing — the switcher is always visible instead.
