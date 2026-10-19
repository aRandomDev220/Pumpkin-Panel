import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ArrowDown, Copy, Download } from "lucide-react";
import Button from "./Button";
import { useToast } from "./toast-context";
import { stripAnsi } from "../../lib/format";

export interface LogViewerProps {
  lines: string[];
  title?: string;
  filename?: string;
  loading?: boolean;
  emptyText?: string;
  toolbar?: ReactNode;
}

export default function LogViewer({
  lines,
  title = "Live console",
  filename = "server.log",
  loading = false,
  emptyText = "No log lines yet.",
  toolbar,
}: LogViewerProps): React.JSX.Element {
  const [follow, setFollow] = useState(true);
  const bodyRef = useRef<HTMLPreElement>(null);
  const toast = useToast();
  const cleanedLines = lines.map(stripAnsi);

  useEffect(() => {
    if (!follow) return;
    const body = bodyRef.current;
    if (body) body.scrollTop = body.scrollHeight;
  }, [lines, follow]);

  async function onCopy(): Promise<void> {
    const text = cleanedLines.join("\n");
    try {
      if (navigator.clipboard) {
        await navigator.clipboard.writeText(text);
      } else {
        const area = document.createElement("textarea");
        area.value = text;
        document.body.appendChild(area);
        area.select();
        document.execCommand("copy");
        area.remove();
      }
      toast.success("Logs copied to clipboard");
    } catch {
      toast.error("Could not copy logs");
    }
  }

  function onDownload(): void {
    const blob = new Blob([cleanedLines.join("\n")], {
      type: "text/plain",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = filename;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
    toast.success("Log file downloaded");
  }

  const content = loading
    ? "Loading logs…"
    : cleanedLines.length > 0
      ? cleanedLines.join("\n")
      : emptyText;

  return (
    <div className="log-panel">
      <div className="log-toolbar">
        <span className="field-label">{title}</span>
        {toolbar !== undefined && (
          <div className="log-toolbar-extra">{toolbar}</div>
        )}
        <span className="spacer" />
        <Button
          size="sm"
          variant={follow ? "primary" : "secondary"}
          aria-pressed={follow}
          onClick={() => setFollow((current) => !current)}
          icon={<ArrowDown size={14} />}
        >
          Follow
        </Button>
        <Button
          size="sm"
          variant="secondary"
          onClick={() => void onCopy()}
          disabled={lines.length === 0}
          icon={<Copy size={14} />}
        >
          Copy
        </Button>
        <Button
          size="sm"
          variant="secondary"
          onClick={onDownload}
          disabled={lines.length === 0}
          icon={<Download size={14} />}
        >
          Download
        </Button>
      </div>
      <pre
        ref={bodyRef}
        className="log-viewer"
        role="log"
        aria-live="off"
        aria-label={title}
        tabIndex={0}
      >
        {content}
      </pre>
    </div>
  );
}
