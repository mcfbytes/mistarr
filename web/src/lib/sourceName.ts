/** The source's file name without a trailing `.torrent` or `.magnet`, as the UI shows it. */
export function fileLabel(originFile: string): string {
  const label = originFile.replace(/\.(torrent|magnet)$/i, '');
  return label === '' ? originFile : label;
}
