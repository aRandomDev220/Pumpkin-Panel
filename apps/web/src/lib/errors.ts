export function technicalDetail(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  try {
    return JSON.stringify(error);
  } catch {
    return "Unknown error";
  }
}

export function errorStatus(error: unknown): number | null {
  const message = technicalDetail(error);
  const match = /\((\d{3})\)/.exec(message) ?? /status\s+(\d{3})/i.exec(message);
  if (match === null) return null;
  const status = Number(match[1]);
  return Number.isFinite(status) ? status : null;
}

function stripPrefix(message: string): string {
  return message
    .replace(/^[a-z][a-z0-9 _-]* failed \(\d{3}\):\s*/i, "")
    .replace(/_/g, " ")
    .trim();
}

export function humanizeError(error: unknown): string {
  const raw = technicalDetail(error);
  const upper = raw.toUpperCase();
  const status = errorStatus(error);

  if (upper.includes("LAST_OWNER")) {
    return "The panel refused this because it would remove the last active owner. Promote or activate another owner first.";
  }
  if (upper.includes("USER_EXISTS")) {
    return "That username is already taken.";
  }
  if (upper.includes("UPDATE_NO_BACKUP")) {
    return "The update was refused because no verified backup exists. Create a backup first.";
  }
  if (upper.includes("CONFIG_CONFLICT") || upper.includes("REVISION_CONFLICT")) {
    return "The file changed on the server since you opened it. Reload and apply your changes again.";
  }
  if (upper.includes("PROTECTED_BACKUP")) {
    return "That backup is protected by a retention policy and cannot be deleted.";
  }
  if (/FETCH FAILED|FAILED TO FETCH|NETWORKERROR|LOAD FAILED|ECONNREFUSED/i.test(raw)) {
    return "Cannot reach the manager. Check that it is running and try again.";
  }

  switch (status) {
    case 401:
      return "Your session expired. Sign in again.";
    case 403:
      return "You do not have permission to do that.";
    case 404:
      return "That item no longer exists.";
    case 409:
      return "That change conflicts with the current state. Reload and try again.";
    case 422:
      return "Some values are invalid. Check the highlighted fields and try again.";
    default:
      break;
  }

  if (status !== null && status >= 500) {
    return "The manager hit a server error. Try again; if it persists, check the manager logs.";
  }

  const cleaned = stripPrefix(raw);
  if (cleaned === "") return "Request failed. Try again.";
  return cleaned.charAt(0).toUpperCase() + cleaned.slice(1);
}
