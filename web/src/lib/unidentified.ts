import type { UnidentifiedReason } from './types';

const REASONS: Record<UnidentifiedReason, string> = {
  off: 'Not identified: identifying CHD images by their tracks is off in System.',
  pending: 'Not identified yet: waiting for its tracks to be hashed.',
  no_layout: 'Not identified: no loaded DAT entry has this number and size of tracks.',
  unreadable: 'Not identified: the file could not be read.',
  not_chd: 'Not identified: the file is not a CHD image.',
  version: 'Not identified: an older CHD version; re-create it with a current chdman.',
  parent: 'Not identified: it needs its parent CHD.',
  not_cd: 'Not identified: not a CD image.',
  too_large: 'Not identified: larger than a CD image.',
  codec: 'Not identified: it uses a compression type mistarr does not read.',
  gdrom: 'Not identified: GD-ROM images are not supported.',
  old_layout: 'Not identified: its track list is in an old format; re-create it with a current chdman.',
  cooked: 'Not identified: a track is stored as 2048-byte sectors, so its raw track cannot be rebuilt.',
  pregap: "Not identified: a track's pregap is not stored in the image.",
  corrupt: 'Not identified: the image is damaged or truncated.',
  checksum: "Not identified: the decoded data do not match the image's own checksum.",
  no_checksum: 'Not identified: the image carries no checksum, as uncompressed CHD images do; re-create it compressed with chdman.'
};

/** The sentence the Platforms card shows for a file not identified. */
export function reasonText(reason: string): string {
  return reason in REASONS ? REASONS[reason as UnidentifiedReason] : 'Not identified.';
}

/** "g.chd, track 2" or "g.chd, track list" for a CHD member path, null for any other path. */
export function chdMemberLabel(path: string): string | null {
  const at = path.toLowerCase().indexOf('.chd#');
  if (at < 0) {
    return null;
  }
  const image = path.slice(0, at + 4).split('/').pop() ?? '';
  const member = path.slice(at + 5);
  if (/^\d+$/.test(member)) {
    return `${image}, track ${Number(member)}`;
  }
  return member.startsWith('cue') ? `${image}, track list` : null;
}

/** "about N minutes per 700 MB image" from a measured decoding speed. */
export function speedText(bytesPerSec: number | null | undefined): string {
  if (!bytesPerSec || bytesPerSec <= 0) {
    return 'Speed not measured yet.';
  }
  const minutes = Math.max(1, Math.round(700_000_000 / bytesPerSec / 60));
  return `Measured speed: about ${minutes} ${minutes === 1 ? 'minute' : 'minutes'} per 700 MB image.`;
}
