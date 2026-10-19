export interface ReadyResponse {
  status: string;
  version: string;
  database: string;
  docker: string;
}

export async function fetchHealth(): Promise<ReadyResponse> {
  const res = await fetch("/api/v1/health/ready");
  if (!res.ok) throw new Error(`health ${res.status}`);
  return res.json();
}
