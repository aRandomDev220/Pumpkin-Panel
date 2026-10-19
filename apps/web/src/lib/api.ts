export interface ReadyResponse {
  status: string;
  version: string;
  database: string;
  docker: string;
}

export async function fetchHealth(): Promise<ReadyResponse> {
  const res = await fetch("/api/v1/health/ready");
  // 503 still carries the degraded detail (database/docker fields).
  if (!res.ok && res.status !== 503) throw new Error(`health ${res.status}`);
  return res.json();
}
