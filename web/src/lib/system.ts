import type { SystemStatus } from './types';

/** The project's own repository, for release notes; never a content URL. */
export const REPO_URL = 'https://github.com/mcfbytes/mistarr';

/** The release notes page for a release build's version, null for any other build. */
export function releaseNotesUrl(status: Pick<SystemStatus, 'version' | 'release'>): string | null {
  return status.release ? `${REPO_URL}/releases/tag/v${encodeURIComponent(status.version)}` : null;
}

/** "41 MB" or "12.4 GB" in decimal units, as storage is sold; "unknown" for null. */
export function bytesText(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) {
    return 'unknown';
  }
  if (bytes >= 1e9) {
    return `${(bytes / 1e9).toFixed(1)} GB`;
  }
  if (bytes >= 1e6) {
    return `${Math.round(bytes / 1e6)} MB`;
  }
  return `${Math.max(0, Math.round(bytes / 1e3))} kB`;
}

/** Binary mebibytes, as `mistarr doctor` prints them. */
function mib(bytes: number | null): string {
  return bytes === null ? 'unknown' : `${Math.round(bytes / 1_048_576)} MiB`;
}

/** "3 d 4 h", "1 h 15 min", "12 min" or "under a minute". */
export function uptimeText(secs: number): string {
  const minutes = Math.floor(secs / 60);
  const hours = Math.floor(minutes / 60);
  const days = Math.floor(hours / 24);
  if (days > 0) {
    return `${days} d ${hours % 24} h`;
  }
  if (hours > 0) {
    return `${hours} h ${minutes % 60} min`;
  }
  return minutes > 0 ? `${minutes} min` : 'under a minute';
}

/** Used over total as a fraction in [0, 1], null when either is unknown. */
export function usedFraction(free: number | null, total: number | null): number | null {
  if (free === null || total === null || total <= 0) {
    return null;
  }
  return Math.min(1, Math.max(0, (total - free) / total));
}

function yesNo(b: boolean): string {
  return b ? 'yes' : 'no';
}

/**
 * A plain-text summary for a bug report, in the shape of `mistarr doctor`. It carries versions,
 * states and sizes only: no paths, addresses, file names or anything naming content.
 */
export function diagnosticsText(status: SystemStatus): string {
  const lines: string[] = [];
  lines.push(`mistarr ${status.version} (${status.release ? 'release' : 'development build'})`);
  lines.push(`commit: ${status.commit ?? 'unknown'}`);
  lines.push(`uptime: ${uptimeText(status.uptime_secs)}`);
  const c = status.client;
  if (c?.kind) {
    const reach = c.reachable ? 'reachable' : 'not reachable';
    lines.push(`client: ${c.kind} ${c.version ?? 'version unknown'}, ${reach}`);
  } else {
    lines.push('client: none detected');
  }
  if (c) {
    lines.push(
      `client programs: rtorrent on PATH ${yesNo(c.rtorrent_on_path)}, transmission on PATH ${yesNo(c.transmission_on_path)}, transmission service ${yesNo(c.transmission_service)}`
    );
  }
  lines.push(`corename: ${status.corename ?? 'not present'}`);
  const held = status.waiting.length;
  const scheduler = status.paused ? `paused (${status.pause_reason ?? 'unknown'})` : 'running';
  lines.push(`scheduler: ${scheduler}, ${held} ${held === 1 ? 'job' : 'jobs'} waiting`);
  lines.push(`pause client while a core runs: ${yesNo(status.pause_client_while_playing)}`);
  lines.push(`client held: ${status.client_hold ?? 'no'}`);
  lines.push(`launching: ${status.launch}`);
  lines.push(`memory: mistarr ${mib(status.rss_bytes)}, available ${mib(status.mem_available_bytes)} of ${mib(status.mem_total_bytes)}`);
  lines.push(`free space data: ${mib(status.disk_free_bytes)} of ${mib(status.disk_total_bytes)}`);
  const rate = status.chd_decode_bytes_per_sec;
  lines.push(`CHD decoding: ${rate ? `${(rate / 1_048_576).toFixed(1)} MiB/s` : 'not measured'}`);
  lines.push(`browser: ${navigator.userAgent}`);
  return lines.join('\n');
}

/**
 * Copies `text` to the clipboard; false when the browser refused. The Clipboard API needs a
 * secure context, which a board on plain HTTP is not, so a selected textarea is the fallback.
 */
export async function copyText(text: string): Promise<boolean> {
  if (window.isSecureContext && 'clipboard' in navigator) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Denied or unfocused: the fallback below may still work.
    }
  }
  const back = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.opacity = '0';
  document.body.appendChild(area);
  area.select();
  let ok: boolean;
  try {
    // eslint-disable-next-line @typescript-eslint/no-deprecated -- the only copy path over plain HTTP.
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  }
  area.remove();
  back?.focus();
  return ok;
}
