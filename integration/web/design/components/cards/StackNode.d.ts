/** Box in the "How it fits together" diagram. accent = Arlo's own layers (glow ring); plain = user's side. */
export interface StackNodeProps {
  title: React.ReactNode;
  subtitle?: React.ReactNode;
  /** Accent-ringed treatment for Arlo-owned layers. @default false */
  accent?: boolean;
}
export declare function StackNode(props: StackNodeProps): JSX.Element;
