import { useEffect } from "react";

export function usePageTitle(title?: string): void {
  useEffect(() => {
    document.title =
      title === undefined || title === ""
        ? "Pumpkin Manager"
        : `${title} — Pumpkin Manager`;
  }, [title]);
}
