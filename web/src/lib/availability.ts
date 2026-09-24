import type { MatchConfidence, TitleAvailability } from './types';

const LABELS: Record<MatchConfidence, string> = {
  name: 'name match',
  base: 'name and size',
  fuzzy: 'name guess',
  size: 'size only'
};

/** How a match's confidence reads on the title screen. */
export function confidenceLabel(confidence: MatchConfidence): string {
  return LABELS[confidence] ?? confidence;
}

/** One line per file that may hold a rom, e.g. "nova.nes in Example Pack (name guess)". */
export function availabilityLine(found: TitleAvailability): string {
  const file = found.path.split('/').pop() || found.path;
  return `${file} in ${found.source_name} (${confidenceLabel(found.confidence)})`;
}
