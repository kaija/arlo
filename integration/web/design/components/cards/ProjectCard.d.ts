/** Project tile: 40px project mark, title, body, mono pills, accent "Visit site →" + muted GitHub links. */
export interface ProjectCardProps {
  /** Which brand mark heads the card. @default 'arlo' */
  project?: 'arlo' | 'rust' | 'lite' | 'agui';
  title: React.ReactNode;
  /** Tech pills, e.g. ['Rust','TUI · CLI','MCP'] */
  pills?: string[];
  siteHref?: string;
  /** Link labels (for localized pages) */
  siteLabel?: string;
  repoLabel?: string;
  repoHref?: string;
  children?: React.ReactNode;
}
export declare function ProjectCard(props: ProjectCardProps): JSX.Element;
