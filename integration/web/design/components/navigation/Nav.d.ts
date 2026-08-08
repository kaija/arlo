/** Fixed frosted-glass top nav: brand, anchor links, language switch, small GitHub button. 64px tall. */
export interface NavProps {
  links?: { label: string; href: string }[];
  /** Current locale for the LangSwitch. @default 'en' */
  lang?: string;
  /** Provide to show the LangSwitch */
  onLangChange?: (code: string) => void;
  /** @default 'https://github.com/kaija' */
  githubHref?: string;
  /** position:fixed (the real site) vs static (for embedding in cards). @default true */
  fixed?: boolean;
}
export declare function Nav(props: NavProps): JSX.Element;
