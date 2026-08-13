/**
 * Shared, pure formatting helpers.
 *
 * These were previously duplicated across UploadQueue and DuplicateReview.
 * Centralizing them keeps behavior identical and makes the logic unit-testable.
 */

/** Format a byte count into a human-readable size (e.g. "1.5 MB"). */
export function formatBytes(bytes?: number): string {
    if (!bytes || bytes <= 0) {
        return "Unknown size"
    }
    const units = ["B", "KB", "MB", "GB", "TB"]
    let value = bytes
    let idx = 0
    while (value >= 1024 && idx < units.length - 1) {
        value /= 1024
        idx += 1
    }
    const precision = idx === 0 ? 0 : 1
    return `${value.toFixed(precision)} ${units[idx]}`
}

/** Format a transfer rate in bytes/second into a human-readable string. */
export function formatSpeed(bps: number): string {
    if (bps < 1024) return `${bps.toFixed(0)} B/s`
    if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(1)} KB/s`
    return `${(bps / (1024 * 1024)).toFixed(1)} MB/s`
}

/** Format a duration in seconds into a short ETA string (e.g. "~5 min"). */
export function formatEta(seconds: number): string {
    if (seconds < 60) return `~${seconds}s`
    if (seconds < 3600) return `~${Math.ceil(seconds / 60)} min`
    return `~${(seconds / 3600).toFixed(1)} hr`
}

/** Extract the file name from a path, handling both POSIX and Windows separators. */
export function getFileNameFromPath(path: string): string {
    return path.split(/[/\\]/).pop() || path
}

/**
 * Derive a short, uppercased file-type label, preferring the MIME subtype and
 * falling back to the file extension. JPEG normalizes to JPG.
 */
export function getFileTypeFromPath(path: string, mimeType?: string): string {
    const fromMime = mimeType?.split("/")[1]
    if (fromMime) {
        return fromMime.toUpperCase() === "JPEG" ? "JPG" : fromMime.toUpperCase()
    }
    const filename = getFileNameFromPath(path)
    const ext = filename.split(".").pop()
    return ext ? ext.toUpperCase() : "UNKNOWN"
}
