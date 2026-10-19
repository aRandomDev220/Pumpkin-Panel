import type { ReactNode } from "react";

export interface AuthLayoutProps {
  eyebrow: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  notes?: ReactNode[];
  children: ReactNode;
}

export default function AuthLayout({
  eyebrow,
  title,
  description,
  notes,
  children,
}: AuthLayoutProps): React.JSX.Element {
  return (
    <section className="auth-page">
      <aside className="auth-aside">
        <div className="auth-brand">
          <span className="brand-mark" aria-hidden="true">
            <img src="/favicon.svg" alt="" />
          </span>
          <span>Pumpkin Manager</span>
        </div>
        <div className="eyebrow">{eyebrow}</div>
        <h1>{title}</h1>
        {description !== undefined && <p>{description}</p>}
        {notes !== undefined && notes.length > 0 && (
          <div className="auth-notes">
            {notes.map((note, index) => (
              <div className="auth-note" key={index}>
                <span className="auth-note-mark" aria-hidden="true" />
                <span>{note}</span>
              </div>
            ))}
          </div>
        )}
      </aside>
      <div className="auth-card">{children}</div>
    </section>
  );
}
