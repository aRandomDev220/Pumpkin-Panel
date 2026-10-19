import Tabs from "../ui/Tabs";

export default function ServerTabs({
  serverId,
}: {
  serverId: string;
}): React.JSX.Element {
  const base = `/servers/${encodeURIComponent(serverId)}`;

  return (
    <Tabs
      ariaLabel="Server sections"
      items={[
        { label: "Console", to: base, end: true },
        { label: "Files", to: `${base}/files` },
        { label: "Config", to: `${base}/config` },
        { label: "Backups", to: `${base}/backups` },
        { label: "Settings", to: `${base}/settings` },
      ]}
    />
  );
}
