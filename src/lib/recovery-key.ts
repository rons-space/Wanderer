/**
 * Pure helpers behind the recovery-key confirmation step.
 *
 * These live outside the component so they can be tested: getting the
 * verification wrong in either direction is expensive. Too lax and a user
 * confirms a key they never saved, and the vault becomes unrecoverable the
 * moment they forget the passphrase; too strict and they are locked out of
 * their own setup by a stray space or a lowercase letter.
 */

/** A recovery key is printed as dash-separated segments. */
export function splitRecoveryKey(key: string): string[] {
    return key
        .trim()
        .split("-")
        .filter((segment) => segment.length > 0);
}

/**
 * Which two segments to ask for. The first and last are the ones a user is
 * most likely to have glanced at rather than written down, so ask for an
 * interior pair when the key is long enough to have one.
 */
export function verificationIndexes(segmentCount: number): [number, number] {
    if (segmentCount < 2) {
        return [0, 0];
    }
    if (segmentCount === 2) {
        return [0, 1];
    }
    return [1, Math.max(0, segmentCount - 2)];
}

/**
 * Compares what was typed against the expected segments, case-insensitively
 * and ignoring surrounding whitespace. Returns false rather than throwing when
 * the indexes fall outside the key, so a malformed key cannot be waved through.
 */
export function verifySegments(
    segments: string[],
    indexes: [number, number],
    answers: [string, string],
): boolean {
    if (segments.length === 0) {
        return false;
    }

    return indexes.every((segmentIndex, position) => {
        const expected = segments[segmentIndex];
        if (expected === undefined) {
            return false;
        }
        return answers[position].trim().toUpperCase() === expected.trim().toUpperCase();
    });
}
