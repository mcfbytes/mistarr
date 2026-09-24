import type { LaunchState, TitleVariant } from './types';

/** Why launching cannot start anything right now, or null when it can. */
export function launchBlocker(state: LaunchState | undefined): string | null {
  switch (state) {
    case 'ready':
      return null;
    case 'disabled':
      return 'Launching is turned off in System settings.';
    case 'unavailable':
      return 'Launching needs mistarr running on a MiSTer.';
    default:
      return 'Checking whether launching is available…';
  }
}

const LOADABLE = new Set(['verified', 'misnamed', 'bad']);

/** Whether every file of a variant is in the collection, as the launch route requires. */
export function inCollection(variant: TitleVariant): boolean {
  if (variant.source === 'mra') {
    const check = variant.mra?.md5_check ?? null;
    return (variant.mra?.missing_zips.length ?? 1) === 0 && check !== 'mismatch' && check !== 'missing_part';
  }
  return variant.roms.length > 0 && variant.roms.every((r) => r.file_state !== null && LOADABLE.has(r.file_state));
}
