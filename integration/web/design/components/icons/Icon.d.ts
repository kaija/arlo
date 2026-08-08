/**
 * Inline line icon for AI-app actions (submit, voice, image, regenerate, …).
 * 24px / 2px stroke / round caps, painted in `currentColor` — set color on the parent.
 */
export interface IconProps {
  /** Icon id — see `IconNames` for the full set. */
  name:
    | 'submit' | 'send' | 'stop'
    | 'image' | 'camera' | 'voice' | 'waveform' | 'volume'
    | 'attach' | 'file' | 'code'
    | 'regenerate' | 'copy' | 'edit' | 'trash' | 'thumbs-up' | 'thumbs-down'
    | 'chat' | 'new-chat' | 'search' | 'zap' | 'globe' | 'settings'
    | 'download' | 'share' | 'check' | 'close' | 'more'
    | 'chevron-down' | 'chevron-up' | 'chevron-left' | 'chevron-right'
    | 'info' | 'check-circle' | 'alert-triangle' | 'alert-circle'
    | 'user' | 'filter' | 'calendar' | 'external';
  /** Rendered box in px. Default 24. Use 20 inside sm controls, 16 for inline-with-text. */
  size?: number;
  /** Stroke width. Default 2 — drop to 1.75 above 32px so weight stays optically even. */
  strokeWidth?: number;
  className?: string;
  style?: React.CSSProperties;
  /** Provide when the icon is the only label; otherwise it renders aria-hidden. */
  'aria-label'?: string;
}
export declare function Icon(props: IconProps): JSX.Element;
/** Map of icon id → inner SVG markup, for copying a single glyph out. */
export declare const IconPaths: Record<string, string>;
/** All available icon ids. */
export declare const IconNames: string[];
