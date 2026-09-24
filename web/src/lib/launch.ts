import type { LaunchState, PlatformKind, TitleVariant } from './types';

/** Why launching cannot start anything right now, or null when it can. */
export function launchBlocker(state: LaunchState | undefined, statusFailed = false): string | null {
  switch (state) {
    case 'ready':
      return null;
    case 'disabled':
      return 'Launching is turned off in System settings.';
    case 'unavailable':
      return 'Launching needs mistarr running on a MiSTer.';
    default:
      return statusFailed
        ? 'The server did not report whether launching is available.'
        : 'Checking whether launching is available…';
  }
}

const LOADABLE = new Set(['verified', 'misnamed', 'bad']);

/** Whether the launch route would start this variant, mirroring the server's rules. */
export function canPlay(variant: TitleVariant, platformId: string, kind: PlatformKind | undefined): boolean {
  if (variant.retired || variant.flags.includes('bios')) {
    return false;
  }
  if (variant.source === 'mra') {
    const check = variant.mra?.md5_check ?? null;
    return (variant.mra?.missing_zips.length ?? 1) === 0 && check !== 'mismatch' && check !== 'missing_part';
  }
  if (platformId === 'arcade') {
    return false;
  }
  const allowed = kind === 'disc' ? new Set(['verified']) : LOADABLE;
  return variant.roms.length > 0 && variant.roms.every((r) => r.file_state !== null && allowed.has(r.file_state));
}
