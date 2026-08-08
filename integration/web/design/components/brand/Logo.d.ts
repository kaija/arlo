/**
 * The Arlo AI marks: each project's glyph inside the same #5856D6 rounded square.
 * Hub "A" monogram is the parent mark; Arlo Rust ">", Arlo Lite peak-with-bar, AG-UI waveform.
 */
export interface LogoProps {
  /** Which mark to render. @default 'arlo' */
  project?: 'arlo' | 'rust' | 'lite' | 'agui';
  /** Square size in px. Nav uses 28, project cards 40, footer 24. @default 28 */
  size?: number;
  /** Render the project name next to the mark, nav-brand style. @default false */
  withWordmark?: boolean;
  className?: string;
}
export declare function Logo(props: LogoProps): JSX.Element;
