/** Segmented language toggle (EN · 繁中 · 日本語). Active segment gets a white pill + shadow. */
export interface LangSwitchProps {
  langs?: { code: string; label: string }[];
  /** @default 'en' */
  current?: string;
  onChange?: (code: string) => void;
}
export declare function LangSwitch(props: LangSwitchProps): JSX.Element;
