// Small formatting helpers shared across pages.

/** Humanize a byte count, e.g. 1536 -> "1.50 KB". */
export function humanizeBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  let value = bytes
  let unitIndex = 0
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex += 1
  }
  const precision = unitIndex === 0 ? 0 : 2
  return `${value.toFixed(precision)} ${units[unitIndex]}`
}

/** Fixed-precision number formatting, tolerant of NaN/undefined-ish input. */
export function formatNumber(value: number, digits: number): string {
  if (!Number.isFinite(value)) return (0).toFixed(digits)
  return value.toFixed(digits)
}

/** Format a fraction (0..1) as a percentage string, e.g. 0.125 -> "12.5%". */
export function formatPercent(fraction: number, digits = 1): string {
  if (!Number.isFinite(fraction)) return `0.${'0'.repeat(digits)}%`
  return `${(fraction * 100).toFixed(digits)}%`
}
