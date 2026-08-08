/**
 * User avatar — initials on accent tint, or an image; optional status dot.
 */
export interface AvatarProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Full name — initials are derived from the first two words. */
  name?: string;
  /** Image URL; falls back to initials when absent. */
  src?: string;
  /** 24 / 28 / 36 (default) / 48 px. */
  size?: 'xs' | 'sm' | 'lg';
  /** 'square' = 8px radius, for org/project avatars. */
  shape?: 'square';
  status?: 'online' | 'away' | 'offline';
}
export declare function Avatar(props: AvatarProps): JSX.Element;
export interface AvatarGroupProps { children?: React.ReactNode }
/** Overlaps its children by 8px with a 2px page-coloured ring. */
export declare function AvatarGroup(props: AvatarGroupProps): JSX.Element;
