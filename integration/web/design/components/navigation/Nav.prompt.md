The site's fixed top navigation — white at 82% opacity with saturate(180%) blur(20px).

```jsx
<Nav links={[{label:'Projects',href:'#projects'},{label:'How it fits',href:'#stack'},{label:'Principles',href:'#principles'}]} lang="en" onLangChange={setLang} />
```

Pages using the fixed nav need ~160px top padding on the hero. Pass `fixed={false}` inside preview frames.
