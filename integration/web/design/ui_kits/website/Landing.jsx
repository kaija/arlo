const{Logo,GitHubIcon,Button,Pill,HeroBadge,SectionHeader,FeatureCard,ProjectCard,StackNode,Nav,Footer}=window.ArloAIDesignSystem_6316a6;
const STR={en:{
'nav.projects':'Projects','nav.stack':'How it fits','nav.principles':'Principles',
'hero.badge':'Free & Open Source · MIT','hero.title':'Open source agentic AI,<br/>top to bottom.',
'hero.subtitle':'Arlo AI is a family of open-source projects for running AI agents on your own terms — a private agent runtime for your machine, a mobile client that talks straight to your provider, and the protocol layer that connects an agent to any frontend. No backend of ours in between. Ever.',
'hero.ctaPrimary':'Explore the projects','hero.ctaSecondary':'View on GitHub',
'projects.heading':'Three projects, one idea','projects.sub':'Every one of them free, MIT-licensed, and yours to run.','projects.visit':'Visit site →','pills.byok':'BYO key',
'projects.rust.body':'A private, local-first agent that speaks whichever interface you need — an interactive TUI, a one-shot CLI, or an embeddable library — and runs on macOS, Windows, and Linux. Your keys, your machine.',
'projects.lite.body':'A React Native iOS app for talking to LLMs on the go. Bring your own API key and connect direct to your provider — no account, no subscription, nothing sitting in between.',
'projects.agui.body':'An up-to-date Rust implementation of the AG-UI protocol — the standard, event-based interface that lets a web or app frontend drive Arlo Rust, wire-compatible with the TypeScript and Python SDKs.',
'stack.heading':'How it fits together','stack.sub':'Two independent paths to the same place — your own model, without a middleman.',
'stack.frontends.title':'Web · Desktop · Mobile frontends','stack.frontends.body':'Whatever you want to build','stack.agui.body':'The standard AG-UI interface','stack.rust.body':'Private, local agent runtime',
'stack.provider.title':'Your own LLM provider','stack.provider.body':'Hosted, self-hosted, or local','stack.or':'or, straight from your pocket','stack.lite.body':'iOS, standalone','stack.lite.direct':'Connected direct, no backend',
'principles.heading':"What we won't compromise on",'principles.sub':'The same four commitments across every project.',
'principles.keys.title':'Bring your own keys','principles.keys.body':'Your credentials stay on your machine and go straight to your provider. We never see them, because there is nowhere for them to go.',
'principles.backend.title':'No backend, no subscription','principles.backend.body':'There is no Arlo server to sign up for, no seat to pay for, and no account to create. You pay your model provider and nobody else.',
'principles.agnostic.title':'Provider-agnostic','principles.agnostic.body':'Anthropic, any OpenAI-compatible API, or a model server running on your own hardware. Point it wherever you like and switch whenever you like.',
'principles.mit.title':'MIT licensed, in the open','principles.mit.body':'Every project is developed in public under the MIT license. Read it, fork it, ship it in your own product — no strings.',
'footer.license':'License'},
'zh-Hant':{
'nav.projects':'專案','nav.stack':'如何串接','nav.principles':'原則',
'hero.badge':'自由且開源 · MIT','hero.title':'從頭到尾<br/>都開源的 Agentic AI。',
'hero.subtitle':'Arlo AI 是一組開源專案，讓你依自己的方式運行 AI agent — 在自己機器上的私密 agent runtime、直連供應商的行動客戶端，以及把 agent 接上任何前端的協定層。中間沒有我們的伺服器，永遠不會有。',
'hero.ctaPrimary':'看看這些專案','hero.ctaSecondary':'在 GitHub 上查看',
'projects.heading':'三個專案，同一個信念','projects.sub':'每一個都免費、採用 MIT 授權，隨你運行。','projects.visit':'前往網站 →','pills.byok':'自備金鑰',
'projects.rust.body':'在本機優先運行的私密 agent，介面隨你挑 — 互動式 TUI、單次執行的 CLI，或嵌入你自己程式的函式庫 — 並可在 macOS、Windows 與 Linux 上運行。你的金鑰，你的機器。',
'projects.lite.body':'用 React Native 打造的 iOS App，隨時隨地與 LLM 對話。帶著自己的 API 金鑰直連供應商 — 不必註冊、不必訂閱，中間不經過任何人。',
'projects.agui.body':'緊跟規範的 AG-UI 協定 Rust 實作 — 這套以事件為基礎的標準介面，讓 Web 或 App 前端能驅動 Arlo Rust，並與 TypeScript、Python SDK 完全相容。',
'stack.heading':'如何串接','stack.sub':'兩條各自獨立的路徑，通往同一個目的地：你自己的模型，中間沒有仲介。',
'stack.frontends.title':'Web · 桌面 · 行動前端','stack.frontends.body':'你想打造的任何介面','stack.agui.body':'AG-UI 標準介面','stack.rust.body':'在本機運行的私密 agent runtime',
'stack.provider.title':'你自己的 LLM 供應商','stack.provider.body':'雲端、自架或本機皆可','stack.or':'或者，直接從你的口袋出發','stack.lite.body':'iOS，獨立運作','stack.lite.direct':'直接連線，不經過後端',
'principles.heading':'我們不會妥協的事','principles.sub':'每個專案都共享同樣的四項承諾。',
'principles.keys.title':'帶著自己的金鑰','principles.keys.body':'你的憑證留在自己機器上，直接送往你的供應商。我們看不到，因為它根本無處可去。',
'principles.backend.title':'沒有後端，沒有訂閱','principles.backend.body':'沒有 Arlo 伺服器要註冊，沒有席次要付費，也沒有帳號要建立。你只付錢給模型供應商，不必付給任何人。',
'principles.agnostic.title':'不綁定供應商','principles.agnostic.body':'Anthropic、任何相容 OpenAI 的 API，或跑在你自家硬體上的模型伺服器。想指向哪裡就指向哪裡，想換就換。',
'principles.mit.title':'MIT 授權，公開開發','principles.mit.body':'每個專案都在 MIT 授權下公開開發。讀它、fork 它、放進你自己的產品裡 — 沒有附帶條件。',
'footer.license':'授權條款'},
ja:{
'nav.projects':'プロジェクト','nav.stack':'構成','nav.principles':'理念',
'hero.badge':'無料・オープンソース · MIT','hero.title':'すべてがオープンソースの<br/>エージェンティック AI。',
'hero.subtitle':'Arlo AI は、AI エージェントを自分のやり方で動かすためのオープンソース・プロジェクト群です。手元のマシンで動くプライベートなエージェントランタイム、プロバイダーに直結するモバイルクライアント、そしてエージェントを任意のフロントエンドにつなぐプロトコル層。あいだに私たちのサーバーはありません。これからも。',
'hero.ctaPrimary':'プロジェクトを見る','hero.ctaSecondary':'GitHub で見る',
'projects.heading':'3 つのプロジェクト、1 つの考え方','projects.sub':'どれも無料、MIT ライセンス、あなたが自由に動かせます。','projects.visit':'サイトへ →','pills.byok':'自分の鍵で',
'projects.rust.body':'ローカル優先で動くプライベートなエージェント。対話型 TUI、ワンショット CLI、組み込みライブラリと、必要なインターフェースを選べます。macOS・Windows・Linux で動作。鍵もマシンもあなたのもの。',
'projects.lite.body':'React Native 製の iOS アプリで、外出先でも LLM と対話。自分の API キーでプロバイダーへ直接接続 — アカウントもサブスクもなく、あいだには何も入りません。',
'projects.agui.body':'仕様に追従した AG-UI プロトコルの Rust 実装。Web やアプリのフロントエンドから Arlo Rust を動かすための、イベントベースの標準インターフェースで、TypeScript・Python SDK とワイヤー互換です。',
'stack.heading':'どう組み合わさるか','stack.sub':'独立した 2 つの経路が、同じ場所へ。仲介者なしで、自分のモデルへ。',
'stack.frontends.title':'Web · デスクトップ · モバイルのフロントエンド','stack.frontends.body':'あなたが作りたいもの','stack.agui.body':'AG-UI 標準インターフェース','stack.rust.body':'ローカルで動くプライベートなエージェントランタイム',
'stack.provider.title':'あなた自身の LLM プロバイダー','stack.provider.body':'ホスト型・自己ホスト型・ローカル','stack.or':'あるいは、ポケットから直接','stack.lite.body':'iOS・単体で動作','stack.lite.direct':'直接接続、バックエンドなし',
'principles.heading':'譲らないこと','principles.sub':'すべてのプロジェクトに共通する 4 つの約束。',
'principles.keys.title':'自分の鍵を使う','principles.keys.body':'認証情報は手元のマシンに留まり、プロバイダーへ直接送られます。私たちには見えません。送られる先がそもそも無いからです。',
'principles.backend.title':'バックエンドもサブスクもなし','principles.backend.body':'登録すべき Arlo のサーバーも、支払うシートも、作るアカウントもありません。お金を払う相手はモデルプロバイダーだけです。',
'principles.agnostic.title':'プロバイダーを選ばない','principles.agnostic.body':'Anthropic、OpenAI 互換の API、自前のハードウェアで動くモデルサーバー。好きな先に向けて、好きなときに乗り換えられます。',
'principles.mit.title':'MIT ライセンス、開かれた開発','principles.mit.body':'すべてのプロジェクトを MIT ライセンスのもとで公開開発しています。読んで、フォークして、自分のプロダクトに載せてください。条件はありません。',
'footer.license':'ライセンス'}};
const ICONS={
keys:<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M21 2l-2 2m-7.6 7.6a5 5 0 11-7.1 7.1 5 5 0 017.1-7.1zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3"/></svg>,
backend:<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><path d="M6 6h.01M6 18h.01"/></svg>,
agnostic:<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="10"/><path d="M2 12h20M12 2a15.3 15.3 0 010 20 15.3 15.3 0 010-20z"/></svg>,
mit:<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"/><polyline points="14 2 14 8 20 8"/><path d="M9 15h6"/></svg>};
function ArloLanding(){
const[lang,setLang]=React.useState('en');
const t=(k)=>(STR[lang]&&STR[lang][k])||STR.en[k];
return <div>
<Nav links={[{label:t('nav.projects'),href:'#projects'},{label:t('nav.stack'),href:'#stack'},{label:t('nav.principles'),href:'#principles'}]} lang={lang} onLangChange={setLang}/>
<section className="hero"><div className="container">
<HeroBadge>{t('hero.badge')}</HeroBadge>
<h1 dangerouslySetInnerHTML={{__html:t('hero.title')}}/>
<p className="hero-subtitle">{t('hero.subtitle')}</p>
<div className="hero-actions"><Button variant="primary" href="#projects">{t('hero.ctaPrimary')}</Button><Button variant="secondary" href="https://github.com/kaija"><GitHubIcon size={20}/> {t('hero.ctaSecondary')}</Button></div>
</div></section>
<section id="projects" className="projects"><div className="container">
<SectionHeader title={t('projects.heading')} subtitle={t('projects.sub')}/>
<div className="projects-grid">
<ProjectCard project="rust" title="Arlo Rust" pills={['Rust','TUI · CLI','MCP']} siteHref="https://rust.arlo-ai.app" repoHref="https://github.com/kaija/arlo" siteLabel={t('projects.visit')}>{t('projects.rust.body')}</ProjectCard>
<ProjectCard project="lite" title="Arlo Lite" pills={['React Native','iOS',t('pills.byok')]} siteHref="https://lite.arlo-ai.app" repoHref="https://github.com/kaija/arlo-lite-ios" siteLabel={t('projects.visit')}>{t('projects.lite.body')}</ProjectCard>
<ProjectCard project="agui" title="AG-UI Rust" pills={['Rust','AG-UI','SSE']} siteHref="https://ag-ui-rust.arlo-ai.app" repoHref="https://github.com/kaija/ag-ui-rust" siteLabel={t('projects.visit')}>{t('projects.agui.body')}</ProjectCard>
</div></div></section>
<section id="stack" className="stack"><div className="container">
<SectionHeader title={t('stack.heading')} subtitle={t('stack.sub')}/>
<div className="stack-diagram">
<div className="stack-flow">
<StackNode title={t('stack.frontends.title')} subtitle={t('stack.frontends.body')}/>
<div className="stack-arrow" aria-hidden="true">↕</div>
<StackNode accent title="AG-UI Rust" subtitle={t('stack.agui.body')}/>
<div className="stack-arrow" aria-hidden="true">↕</div>
<StackNode accent title="Arlo Rust" subtitle={t('stack.rust.body')}/>
<div className="stack-arrow" aria-hidden="true">↕</div>
<StackNode title={t('stack.provider.title')} subtitle={t('stack.provider.body')}/>
</div>
<div className="stack-divider">{t('stack.or')}</div>
<div className="stack-aside">
<StackNode accent title="Arlo Lite" subtitle={t('stack.lite.body')}/>
<div className="stack-arrow" aria-hidden="true">→</div>
<StackNode title={t('stack.provider.title')} subtitle={t('stack.lite.direct')}/>
</div></div></div></section>
<section id="principles" className="features"><div className="container">
<SectionHeader title={t('principles.heading')} subtitle={t('principles.sub')}/>
<div className="features-grid">
<FeatureCard icon={ICONS.keys} title={t('principles.keys.title')}>{t('principles.keys.body')}</FeatureCard>
<FeatureCard icon={ICONS.backend} title={t('principles.backend.title')}>{t('principles.backend.body')}</FeatureCard>
<FeatureCard icon={ICONS.agnostic} title={t('principles.agnostic.title')}>{t('principles.agnostic.body')}</FeatureCard>
<FeatureCard icon={ICONS.mit} title={t('principles.mit.title')}>{t('principles.mit.body')}</FeatureCard>
</div></div></section>
<Footer links={[{label:'Arlo Rust',href:'https://rust.arlo-ai.app'},{label:'Arlo Lite',href:'https://lite.arlo-ai.app'},{label:'AG-UI Rust',href:'https://ag-ui-rust.arlo-ai.app'},{label:'GitHub',href:'https://github.com/kaija'},{label:t('footer.license'),href:'https://github.com/kaija/arlo/blob/main/LICENSE'}]}/>
</div>;}
window.ArloLanding=ArloLanding;
